//! The rune app state: the proven [`State`] core plus the ephemeral parts that
//! must not enter it. The edit buffers back the search and rename fields; the
//! auth, service and token store hold the network side, so `State` stays
//! `PartialEq`/`Eq`/`Hash` and the invariant proof stays bounded.
//!
//! The service is a [`MusicService`] in an `Arc`. The default is the
//! deterministic [`FakeService`]; set `OPENTIDAL_LIVE=1` to use the real
//! [`HttpService`] and the macOS keychain.

use std::sync::Arc;

use rune::{Latest, TextEdit, Waker};
use serde::Deserialize;
use crate::auth::{self, Auth, OAuthConfig, Token, TokenStore};
use crate::state::State;
use crate::tidal::{self, MusicService, ServiceError, User};

pub struct App {
    pub core: State,
    pub search: TextEdit,
    /// The rename buffer, shared by the playlist and profile rename fields.
    pub rename: TextEdit,
    pub auth: Auth,
    pub service: Arc<dyn MusicService>,
    pub oauth: OAuthConfig,
    pub store: Arc<dyn TokenStore>,
    /// Whether the live client is on, from `OPENTIDAL_LIVE` or the config file.
    pub live: bool,
    /// A counter bumped on sign-out and new sign-ins. In-flight profile results
    /// from an earlier generation are discarded, so a stale failure cannot
    /// clobber `SignedOut`.
    pub session: u64,
    /// The sign-in dance (loopback + exchange), drained by `poll`. Carries the
    /// session it was started under, so a result that lands after a sign-out or
    /// a newer sign-in is dropped.
    pub auth_flow: Latest<(u64, Result<Token, String>)>,
    /// The profile fetch after sign-in, drained by `poll`. Carries the session
    /// it was started under and the possibly-refreshed token to persist.
    pub profile: Latest<(u64, Result<(Token, User), String>)>,
}

/// A dummy token for the deterministic sign-in, so the fake path exercises the
/// same `SignedIn` state without a network.
fn fake_token() -> Token {
    Token { access_token: "fake-access".to_string(), refresh_token: "fake-refresh".to_string(), expires_in: 0 }
}

impl App {
    /// Attach the wakers and restore a stored session when signed out. The
    /// deterministic default uses a memory store, so this is a no-op headless;
    /// the live build loads the keychain and fetches the profile, refreshing an
    /// expired token first.
    pub fn setup(&mut self, waker: Waker) {
        self.auth_flow.set_waker(waker.clone());
        self.profile.set_waker(waker);
        if self.auth == Auth::SignedOut
            && let Some(token) = self.store.load()
        {
            eprintln!("opentidal: restoring a stored session");
            self.auth = Auth::SignedIn { token: token.clone(), name: String::new() };
            self.fetch_profile(token);
        }
    }

    /// Start sign-in. The deterministic build signs in at once; the live build
    /// binds the loopback port, opens the browser, and runs the exchange on a
    /// worker thread. The listener is bound before the browser opens so a fast
    /// redirect never reaches an unopened port.
    pub fn start_sign_in(&mut self) {
        if !self.live {
            self.auth = Auth::SignedIn { token: fake_token(), name: "Fake User".to_string() };
            return;
        }
        let (url, verifier, state) = auth::begin(&self.oauth);
        let listener = match tidal::bind_loopback(&self.oauth.redirect_uri) {
            Ok(listener) => listener,
            Err(message) => {
                self.auth = Auth::Failed { message };
                return;
            }
        };
        eprintln!("opentidal: sign in, redirect_uri={}", self.oauth.redirect_uri);
        eprintln!("opentidal: opening {url}");
        self.session += 1;
        let session = self.session;
        self.auth = Auth::Authorizing { state: state.clone(), verifier: verifier.clone() };
        self.open_browser(&url);
        let config = self.oauth.clone();
        self.auth_flow.request((config, state, verifier, listener), move |(cfg, st, v, listener)| {
            (session, tidal::run_loopback(cfg, st, v, listener))
        });
    }

    pub fn sign_out(&mut self) {
        self.session += 1;
        match self.store.clear() {
            Ok(()) => self.auth = Auth::SignedOut,
            Err(e) => {
                // Do not claim sign-out while the token still persists; the
                // next launch would restore it.
                eprintln!("opentidal: sign-out failed: {e}");
                self.auth = Auth::Failed { message: format!("Sign-out failed: {e}") };
            }
        }
    }

    /// Fetch the signed-in user's profile off the UI thread, refreshing an
    /// expired or rejected token first. The result carries the (possibly
    /// renewed) token to persist and the session it belongs to.
    fn fetch_profile(&mut self, token: Token) {
        let service = self.service.clone();
        let config = self.oauth.clone();
        let store = self.store.clone();
        let session = self.session;
        self.profile.request(token, move |t| {
            (session, resolve_user(&service, &config, &store, t))
        });
    }

    /// Drain any landed auth or profile result. Returns true when state changed.
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        if let Some((session, result)) = self.auth_flow.take() {
            if session == self.session {
                changed = true;
                match result {
                    Ok(token) => {
                        self.store.save(&token);
                        self.auth = Auth::SignedIn { token: token.clone(), name: String::new() };
                        self.fetch_profile(token);
                    }
                    Err(message) => self.auth = Auth::Failed { message },
                }
            }
        }
        if let Some((session, result)) = self.profile.take() {
            if session == self.session {
                changed = true;
                match result {
                    Ok((token, user)) => {
                        self.store.save(&token);
                        if let Auth::SignedIn { name, token: held, .. } = &mut self.auth {
                            *held = token;
                            *name = user.name;
                        }
                    }
                    Err(message) => self.auth = Auth::Failed { message },
                }
            }
        }
        changed
    }

    #[cfg(target_os = "macos")]
    fn open_browser(&self, url: &str) {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }

    #[cfg(not(target_os = "macos"))]
    fn open_browser(&self, url: &str) {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

/// Resolve the current user for a token, refreshing it first when it is expired
/// or when the service rejects it. Returns the token actually used, so the
/// caller can persist a renewed one. A refresh that the auth endpoint rejects
/// outright (for example `invalid_grant`) drops the stored token, so the next
/// launch starts clean instead of failing the same way.
fn resolve_user(
    service: &Arc<dyn MusicService>,
    config: &OAuthConfig,
    store: &Arc<dyn TokenStore>,
    token: Token,
) -> Result<(Token, User), String> {
    let token = if auth::token_expired(&token) {
        refresh(config, store, &token)?
    } else {
        token
    };
    match service.current_user(&token.access_token) {
        Ok(user) => Ok((token, user)),
        Err(ServiceError::Unauthorized) => {
            let fresh = refresh(config, store, &token)?;
            let user = service.current_user(&fresh.access_token).map_err(|e| format!("{e:?}"))?;
            Ok((fresh, user))
        }
        Err(e) => Err(format!("{e:?}")),
    }
}

/// Refresh a token, clearing the store when the endpoint rejects the refresh
/// outright. A network or transport failure keeps the token, so a transient
/// outage does not throw away a working session.
fn refresh(config: &OAuthConfig, store: &Arc<dyn TokenStore>, token: &Token) -> Result<Token, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    match tidal::refresh_token(&client, config, &token.refresh_token) {
        Ok(fresh) => {
            eprintln!("opentidal: refreshed the access token");
            Ok(fresh)
        }
        Err(e) => {
            let dead = matches!(&e, ServiceError::Auth(_));
            if dead {
                let _ = store.clear();
                eprintln!("opentidal: refresh rejected, session cleared: {e:?}");
            } else {
                eprintln!("opentidal: refresh failed: {e:?}");
            }
            Err(format!("{e:?}"))
        }
    }
}

impl Default for App {
    fn default() -> Self {
        let file = file_config();
        let live = match std::env::var("OPENTIDAL_LIVE").as_deref() {
            Ok("1") => true,
            Ok(_) => false,
            Err(_) => file.live,
        };
        let service: Arc<dyn MusicService> = if live {
            Arc::new(tidal::HttpService::new("https://openapi.tidal.com/v2"))
        } else {
            Arc::new(tidal::FakeService::default())
        };
        let store: Arc<dyn TokenStore> = if live {
            keychain_store()
        } else {
            Arc::new(auth::MemoryStore::default())
        };
        let oauth = OAuthConfig::tidal(
            env_or("TIDAL_CLIENT_ID", &file.client_id),
            env_or("TIDAL_CLIENT_SECRET", &file.client_secret),
            env_or("TIDAL_REDIRECT_URI", &file.redirect_uri),
            env_or("TIDAL_SCOPE", &file.scope),
        );
        App {
            core: State::default(),
            search: TextEdit::new(""),
            rename: TextEdit::new(""),
            auth: Auth::default(),
            service,
            oauth,
            store,
            live,
            session: 0,
            auth_flow: Latest::default(),
            profile: Latest::default(),
        }
    }
}

/// An env var, or the matching field of the config file when the var is unset.
fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

/// The optional config file, named by `OPENTIDAL_CONFIG`. A shipped `.app` has
/// no environment, so its live switch and credentials come from here. The file
/// is the operator's and stays out of the repo.
#[derive(Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    live: bool,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    client_secret: String,
    #[serde(default)]
    redirect_uri: String,
    #[serde(default)]
    scope: String,
}

fn file_config() -> FileConfig {
    let Some(path) = config_path() else {
        return FileConfig::default();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The config file path: `OPENTIDAL_CONFIG` when set, else the per-user default
/// (`~/Library/Application Support/OpenTidal/config.json` on macOS).
fn config_path() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("OPENTIDAL_CONFIG") {
        if !path.is_empty() {
            return Some(std::path::PathBuf::from(path));
        }
    }
    let home = std::env::var("HOME").ok()?;
    let sub = if cfg!(target_os = "macos") {
        "Library/Application Support/OpenTidal/config.json"
    } else {
        ".config/opentidal/config.json"
    };
    Some(std::path::PathBuf::from(home).join(sub))
}

/// The macOS keychain store, or an in-memory store on other platforms.
fn keychain_store() -> Arc<dyn TokenStore> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(auth::KeychainStore::new("opentidal", "tidal"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(auth::MemoryStore::default())
    }
}
