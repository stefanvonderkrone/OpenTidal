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
use crate::auth::{self, Auth, OAuthConfig, Token, TokenStore};
use crate::state::State;
use crate::tidal::{self, MusicService, User};

pub struct App {
    pub core: State,
    pub search: TextEdit,
    /// The rename buffer, shared by the playlist and profile rename fields.
    pub rename: TextEdit,
    pub auth: Auth,
    pub service: Arc<dyn MusicService>,
    pub oauth: OAuthConfig,
    pub store: Arc<dyn TokenStore>,
    /// Whether the live client is on, from `OPENTIDAL_LIVE`.
    pub live: bool,
    /// The sign-in dance (loopback + exchange), drained by `poll`.
    pub auth_flow: Latest<Result<Token, String>>,
    /// The profile fetch after sign-in, drained by `poll`.
    pub profile: Latest<Result<User, String>>,
}

/// A dummy token for the deterministic sign-in, so the fake path exercises the
/// same `SignedIn` state without a network.
fn fake_token() -> Token {
    Token { access_token: "fake-access".to_string(), refresh_token: "fake-refresh".to_string(), expires_in: 0 }
}

impl App {
    /// Attach the wakers and restore a stored session when signed out. The
    /// deterministic default uses a memory store, so this is a no-op headless;
    /// the live build loads the keychain and fetches the profile.
    pub fn setup(&mut self, waker: Waker) {
        self.auth_flow.set_waker(waker.clone());
        self.profile.set_waker(waker);
        if self.auth == Auth::SignedOut
            && let Some(token) = self.store.load()
        {
            self.auth = Auth::SignedIn { token: token.clone(), name: String::new() };
            self.fetch_profile(token);
        }
    }

    /// Start sign-in. The deterministic build signs in at once; the live build
    /// opens the browser and runs the loopback exchange on a worker thread.
    pub fn start_sign_in(&mut self) {
        if !self.live {
            self.auth = Auth::SignedIn { token: fake_token(), name: "Fake User".to_string() };
            return;
        }
        let (url, verifier, state) = auth::begin(&self.oauth);
        self.auth = Auth::Authorizing { state: state.clone(), verifier: verifier.clone() };
        self.open_browser(&url);
        let config = self.oauth.clone();
        self.auth_flow.request((config, state, verifier), |(cfg, st, v)| tidal::run_loopback(cfg, st, v));
    }

    pub fn sign_out(&mut self) {
        self.store.clear();
        self.auth = Auth::SignedOut;
    }

    /// Fetch the signed-in user's profile off the UI thread.
    fn fetch_profile(&mut self, token: Token) {
        let service = self.service.clone();
        self.profile.request(token, move |t| {
            service.current_user(&t.access_token).map_err(|e| format!("{e:?}"))
        });
    }

    /// Drain any landed auth or profile result. Returns true when state changed.
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        if let Some(result) = self.auth_flow.take() {
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
        if let Some(result) = self.profile.take() {
            changed = true;
            match result {
                Ok(user) => {
                    if let Auth::SignedIn { name, .. } = &mut self.auth {
                        *name = user.name;
                    }
                }
                Err(message) => self.auth = Auth::Failed { message },
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

impl Default for App {
    fn default() -> Self {
        let live = std::env::var("OPENTIDAL_LIVE").is_ok();
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
            env_or("TIDAL_CLIENT_ID"),
            env_or("TIDAL_CLIENT_SECRET"),
            env_or("TIDAL_REDIRECT_URI"),
            env_or("TIDAL_SCOPE"),
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
            auth_flow: Latest::default(),
            profile: Latest::default(),
        }
    }
}

fn env_or(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
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
