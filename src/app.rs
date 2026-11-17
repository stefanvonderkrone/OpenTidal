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
use crate::library::Library;
use crate::state::State;
use crate::store::Store;
use crate::tidal::{self, MusicService, ServiceError, User};

pub struct App {
    pub core: State,
    pub search: TextEdit,
    /// The rename buffer, shared by the playlist and profile rename fields.
    pub rename: TextEdit,
    pub auth: Auth,
    pub service: Arc<dyn MusicService>,
    pub oauth: OAuthConfig,
    /// The keychain, holding the token and nothing else.
    pub tokens: Arc<dyn TokenStore>,
    /// The open library database with the account id it belongs to, or `None`
    /// when nothing can be saved. The pair is one value, so an account id can
    /// never exist without the database it keys.
    library: Option<OpenLibrary>,
    /// The library as last written to disk, so a frame can tell whether the
    /// in-memory library changed.
    on_disk: Library,
    /// Set after a write fails, so a dirty library does not wake the loop on
    /// every frame. The next edit clears it through a successful write.
    write_failed: bool,
    /// Where this app keeps its database, when it is not the default path. A scene
    /// or a test sets it, so a run that writes a fabricated library cannot land in
    /// the real one. `None` means the path the app really uses.
    pub(crate) db_override: Option<std::path::PathBuf>,
    /// Why the last profile fetch failed, when the failure did not end the session.
    /// A transport error keeps the account signed in on what the disk holds, so the
    /// reason is shown beside the name rather than over a sign-in button.
    pub profile_error: Option<String>,
    /// Why the last library write did not land, when sign-out was refused because
    /// of it. The account screen shows this. It is not `Auth::Failed`, because that
    /// state offers a sign-in button, and signing in again would drop the same
    /// edits. Read as the write of a library, never **save** in the album sense.
    pub(crate) write_error: Option<String>,
    /// Wakes the event loop so `poll` runs after a change. A windowed app stops
    /// drawing frames at rest, so without this a click in the last frame before
    /// idle would sit unwritten until the next input.
    waker: Option<Waker>,
    /// Whether the live client is on, from `OPENTIDAL_LIVE` or the config file.
    pub live: bool,
    /// A counter bumped on sign-out and new sign-ins. In-flight profile results
    /// from an earlier generation are discarded, so a stale failure cannot
    /// clobber `SignedOut`.
    pub session: u64,
    /// The sign-in dance (loopback + exchange), drained by `poll`. Carries the
    /// session it was started under, so a result that lands after a sign-out or
    /// a newer sign-in is dropped.
    pub auth_flow: Latest<AuthFlow>,
    /// The profile fetch after sign-in, drained by `poll`. Carries the session
    /// it was started under and the possibly-refreshed token to persist.
    pub profile: Latest<ProfileFetch>,
}

/// One account's open library. The account id and the handle are one value, so a
/// save always knows which rows it writes.
struct OpenLibrary {
    user_id: String,
    store: Store,
    /// Set when this account has no row on disk yet, because the first write
    /// failed. The next flush writes the row before anything else, so one
    /// transient error does not disable saving for the whole session.
    row_pending: bool,
}

/// A finished sign-in exchange, tagged with the session it ran under.
type AuthFlow = (u64, Result<Token, String>);

/// A finished profile fetch, tagged with its session, carrying the token to
/// persist.
type ProfileFetch = (u64, Result<(Token, User), ServiceError>);

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
        self.profile.set_waker(waker.clone());
        self.waker = Some(waker);
        if self.auth == Auth::SignedOut
            && let Some(token) = self.tokens.load()
        {
            // A stored token with no `uid` claim names no account, so there is
            // no session to restore. That is a stale sentinel or a corrupt item,
            // and signing out is the only honest reading.
            if session_user_id(&token).is_none() {
                eprintln!("opentidal: the stored token names no account, signing out");
                let _ = self.tokens.clear();
                return;
            }
            eprintln!("opentidal: restoring a stored session");
            self.begin_session(token);
        }
    }

    /// Open the database and load this user's library into the core state. The
    /// library stays as it is, and the store stays closed for writing, when the
    /// file cannot be read. Overwriting an unreadable library with the default
    /// would destroy it, and the next sync is the only thing that can rebuild
    /// it.
    fn restore_library(&mut self, user_id: &str) {
        let Some(path) = self.db_override.clone().or_else(db_path) else {
            return;
        };
        self.open_library(user_id, &path);
    }

    /// Open the database for an account and load its library. The account id and
    /// the handle become one `Session`, so nothing downstream can save without
    /// knowing which account it saves for.
    ///
    /// The library stays as it is, and no session is established, when the file
    /// cannot be read. Overwriting an unreadable library with the default would
    /// destroy it, and only a sync could rebuild it.
    fn open_library(&mut self, user_id: &str, path: &std::path::Path) {
        // Re-opening the account that is already open is the common path: a
        // re-sign-in with the same token, or a restart restoring the same session.
        // Detaching first would drop edits the in-memory library still owes to
        // disk, and a failed flush has no other copy, so this returns early.
        if self.library.as_ref().is_some_and(|open| open.user_id == user_id) {
            return;
        }
        // A different account detaches first, so one account's rows can never show
        // under another's id or save under it. An edit that could not be written
        // is lost here on purpose, and said so in the log: keeping it would write
        // it under the wrong account, which is the worse failure.
        if !self.forget_session() {
            eprintln!(
                "opentidal: the previous account's last edits were not saved, and are dropped rather than written under {user_id}"
            );
        }

        let mut store = match Store::open(path) {
            Ok(store) => store,
            Err(error) => {
                eprintln!("opentidal: cannot open {}: {error:?}", path.display());
                // The account is signed in and has nowhere to save. Say so where the
                // user can see it, instead of accepting edits that will be dropped.
                self.write_error = Some(
                    "Your library could not be opened, so changes cannot be written. Sign out and back in, or check the disk."
                        .to_string(),
                );
                return;
            }
        };
        // A missing row means a new account, so the in-memory library must not
        // carry the previous account's rows into this one.
        match store.load(user_id) {
            Ok(Some(loaded)) => {
                if loaded.dropped > 0 {
                    eprintln!(
                        "opentidal: {} stored item(s) name something the catalogue no longer has, and the next sync restores them",
                        loaded.dropped
                    );
                }
                self.core.lib = loaded.lib;
            }
            Ok(None) => {
                self.core.lib = Library::default();
                // Write the row now, so a restart finds this account even when
                // the profile fetch never returns a name. This cannot go through
                // `flush_library`, which skips a library equal to the one on
                // disk, and a new account's library is exactly that.
                //
                // A failure keeps the store open and leaves `on_disk` different
                // from the library, so the next frame retries. Closing the store
                // here would disable saving for the rest of the session after one
                // transient error, and the user would lose every edit.
                let row_pending = match store.write(user_id, &self.core.lib) {
                    Ok(()) => {
                        self.on_disk = self.core.lib.clone();
                        false
                    }
                    Err(error) => {
                        eprintln!("opentidal: cannot open a row for a new account: {error:?}");
                        true
                    }
                };
                self.library = Some(OpenLibrary {
                    user_id: user_id.to_string(),
                    store,
                    row_pending,
                });
                return;
            }
            Err(error) => {
                eprintln!("opentidal: cannot load the library, leaving it unwritten: {error:?}");
                self.write_error = Some(
                    "Your library could not be read, so changes cannot be written. Sign out and back in, or check the disk."
                        .to_string(),
                );
                return;
            }
        }
        self.on_disk = self.core.lib.clone();
        self.library = Some(OpenLibrary {
            user_id: user_id.to_string(),
            store,
            row_pending: false,
        });
    }

    /// Write the library when it differs from what is on disk. This runs at the
    /// start of every frame, so a click in one frame is on disk by the next.
    /// One comparison covers every operation in the UI, so a new screen action
    /// needs no persistence call of its own.
    /// Returns true when nothing is owed to disk: either the write landed, or
    /// there was nothing to write. A caller that is about to discard the library
    /// must check this, or a failed write loses the only copy.
    fn flush_library(&mut self) -> bool {
        let Some(open) = self.library.as_mut() else {
            // Signed in with no open library means the database would not open, so
            // anything edited now can never be written. Reporting "nothing owed"
            // would let a caller discard it silently.
            return !matches!(self.auth, Auth::SignedIn { .. });
        };
        // An account whose first write failed has no row yet. The library may
        // still equal `on_disk`, so this has to be checked before that
        // comparison, or the row is never written.
        if open.row_pending {
            match open.store.write(&open.user_id, &self.core.lib) {
                Ok(()) => {
                    open.row_pending = false;
                    self.on_disk = self.core.lib.clone();
                    self.write_failed = false;
                    self.write_error = None;
                }
                Err(error) => {
                    self.write_failed = true;
                    eprintln!("opentidal: cannot open a row for a new account: {error:?}");
                    return false;
                }
            }
            return true;
        }
        if self.core.lib == self.on_disk {
            // Nothing is owed, so no failure is outstanding. Without this a refused
            // sign-out's message would sit over a library that is fully written, and
            // the wake flag would stay set so the next edit never asked for a frame.
            self.write_failed = false;
            self.write_error = None;
            return true;
        }
        match open.store.write(&open.user_id, &self.core.lib) {
            Ok(()) => {
                self.on_disk = self.core.lib.clone();
                self.write_failed = false;
                // A write landed, so any refusal shown earlier is out of date.
                self.write_error = None;
                true
            }
            Err(error) => {
                // Keep the library dirty so a later edit tries again, but stop
                // waking for this failure.
                self.write_failed = true;
                eprintln!("opentidal: cannot write the library: {error:?}");
                false
            }
        }
    }

    /// Start sign-in. The deterministic build signs in at once; the live build
    /// binds the loopback port, opens the browser, and runs the exchange on a
    /// worker thread. The listener is bound before the browser opens so a fast
    /// redirect never reaches an unopened port.
    pub fn start_sign_in(&mut self) {
        if !self.live {
            self.core.lib.set_name("Fake User".to_string());
            self.auth = Auth::SignedIn { token: fake_token() };
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
        // A session with no open library has nowhere to write and nothing it could
        // lose, so it always leaves. Refusing here would strand the deterministic
        // build, and a live account whose database would not open, with no way out.
        let storeless = self.library.is_none();
        // Otherwise write first, and keep the session if that fails. Discarding the
        // library after a failed write throws away the only copy of the user's edits.
        if !storeless && !self.flush_library() {
            // Stay signed in and keep the edits. `Auth::Failed` would offer a
            // sign-in button, and taking it drops the library this just refused
            // to discard.
            self.write_error = Some(
                "Your changes are not written to disk yet. Sign-out waits for them.".to_string(),
            );
            return;
        }
        self.write_error = None;
        self.session += 1;
        match self.tokens.clear() {
            Ok(()) => {
                self.forget_session();
                self.auth = Auth::SignedOut;
            }
            Err(e) => {
                // Do not claim sign-out while the token still persists; the
                // next launch would restore it.
                eprintln!("opentidal: sign-out failed: {e}");
                self.auth = Auth::Failed { message: format!("Sign-out failed: {e}") };
            }
        }
    }

    /// Drop everything tied to the signed-in account. The rows stay on disk, so
    /// the next sign-in with that account finds its library. A kept library in
    /// memory would show one account's data while another is signed in, and the
    /// next save would write it under the new id.
    /// Returns true when the library was on disk before it was dropped. A caller
    /// that must not lose edits checks this result and keeps the session instead.
    fn forget_session(&mut self) -> bool {
        let saved = self.flush_library();
        self.library = None;
        self.write_failed = false;
        self.profile_error = None;
        // The message belongs to the account that just closed. It must not follow
        // the next account onto its own Account screen.
        self.write_error = None;
        self.core.lib = Library::default();
        // A playlist screen for the old account names a playlist that no longer
        // exists, which the state invariant forbids. Drop those routes and keep
        // the stack non-empty, the same way `state::delete_playlist` does.
        self.core
            .stack
            .retain(|route| !matches!(route, crate::state::Route::UserPlaylist(_)));
        if self.core.stack.is_empty() {
            self.core.stack.push(crate::state::Route::Home);
        }
        self.on_disk = Library::default();
        saved
    }

    /// Adopt a token as the current session: open that account's library, then
    /// fetch the profile. A token with no `uid` claim names no account, so it is
    /// a failed sign-in rather than a session without a library. The deterministic
    /// build does not come through here; [`App::start_sign_in`] signs in directly.
    fn begin_session(&mut self, token: Token) {
        let Some(user_id) = session_user_id(&token) else {
            self.auth = Auth::Failed { message: "the token names no account".to_string() };
            return;
        };
        // A new session starts with no fetch history, so a message about the previous
        // one's failed fetch must not follow it onto the Account screen.
        self.profile_error = None;
        self.restore_library(&user_id);
        self.auth = Auth::SignedIn { token: token.clone() };
        self.fetch_profile(token);
    }

    /// Fetch the signed-in user's profile off the UI thread, refreshing an
    /// expired or rejected token first. The result carries the (possibly
    /// renewed) token to persist and the session it belongs to.
    fn fetch_profile(&mut self, token: Token) {
        let service = self.service.clone();
        let config = self.oauth.clone();
        let session = self.session;
        self.profile.request(token, move |t| {
            (session, resolve_user(&service, &config, t))
        });
    }

    /// Drain any landed auth or profile result. Returns true when state changed.
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        if let Some((session, result)) = self.auth_flow.take()
            && session == self.session
        {
            changed = true;
            match result {
                Ok(token) => {
                    self.tokens.save(&token);
                    self.begin_session(token);
                }
                Err(message) => self.auth = Auth::Failed { message },
            }
        }
        if let Some((session, result)) = self.profile.take()
            && session == self.session
        {
            changed = true;
            match result {
                Ok((token, user)) => {
                    self.tokens.save(&token);
                    if let Auth::SignedIn { token: held } = &mut self.auth {
                        *held = token;
                    }
                    self.core.lib.set_name(user.name);
                    // The fetch landed, so the reason an earlier one did not is out
                    // of date. Leaving it would show "could not reach Tidal" over a
                    // name that just arrived.
                    self.profile_error = None;
                }
                Err(e) => {
                    // A dead refresh token means no session at all. The rows stay
                    // on disk for a later sign-in, but the library in memory must
                    // not survive, or the next account to sign in would inherit
                    // it and save it under its own id.
                    if matches!(&e, ServiceError::InvalidGrant(_)) {
                        let _ = self.tokens.clear();
                        // The token is dead, so the session cannot continue and the
                        // library has to go. Say so when its last edits could not be
                        // written, rather than dropping them in silence.
                        if !self.forget_session() {
                            eprintln!(
                                "opentidal: the dead token ended the session before the library was written, so the last edits are lost"
                            );
                        }
                        self.auth = Auth::Failed { message: format!("{e:?}") };
                    } else {
                        // A transport error is not a lost session. The disk holds the
                        // name and the library, so the launch stays signed in and the
                        // fetch retries on the next request. Falling back to
                        // `Auth::Failed` here would put a sign-in button over a session
                        // that still works, and the button would drop the library.
                        eprintln!(
                            "opentidal: the profile fetch failed, staying signed in: {e:?}"
                        );
                        // A reader does not need the error detail, which the log keeps
                        // for a bug report. The message must say which kind of failure
                        // this is: a network that came back needs nothing from them, a
                        // session Tidal will not accept needs a new sign-in.
                        self.profile_error = Some(match &e {
                            ServiceError::Unauthorized | ServiceError::Auth(_) => {
                                "Tidal would not accept this session. Sign out and sign in again."
                                    .to_string()
                            }
                            _ => "Could not reach Tidal. Showing what is saved on this Mac."
                                .to_string(),
                        });
                    }
                }
            }
        }
        self.flush_library();
        changed
    }

    /// Ask for one more frame when the library changed and there is somewhere to
    /// write it. The write happens in that frame's `drain`.
    ///
    /// Two guards, both load-bearing. No open library means no write will happen,
    /// so waking would leave the library dirty and wake again on every frame, which
    /// spins the CPU. A failed write keeps the library dirty, so a retry sets a
    /// flag that stops the wake until the next edit.
    pub fn wake_if_dirty(&self) {
        if self.library.is_none() {
            return;
        }
        // `drain` clears the flag when a write succeeds, so a failure alone stops
        // the wake. The next `drain` retries, because the library is still dirty.
        if self.core.lib != self.on_disk
            && !self.write_failed
            && let Some(waker) = &self.waker
        {
            waker();
        }
    }

    /// Write anything outstanding before the app exits.
    ///
    /// Returns whether the library is on disk. A caller cannot refuse the quit: the
    /// event loop's only cancel comes from the system, so an app that asks to stay
    /// open here would be ignored. It reports instead, because a lost edit must not
    /// leave in silence.
    pub fn quit(&mut self) -> bool {
        if self.flush_library() {
            return true;
        }
        eprintln!(
            "opentidal: the last edits could not be written before quit, so they are lost"
        );
        false
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
/// caller can persist a renewed one. A refresh the endpoint rejects outright
/// (for example `invalid_grant`) surfaces as [`ServiceError::InvalidGrant`],
/// which the caller maps to clearing the stored token.
fn resolve_user(
    service: &Arc<dyn MusicService>,
    config: &OAuthConfig,
    token: Token,
) -> Result<(Token, User), ServiceError> {
    resolve_user_with(service, &|t| refresh(config, t), token)
}

/// `resolve_user`, with the refresh as a parameter. A test needs to count the refreshes
/// and make them fail on demand, which the network call cannot do without a server.
/// Production passes the real one, so the logic below is the same code in both.
fn resolve_user_with(
    service: &Arc<dyn MusicService>,
    refresh: &dyn Fn(&Token) -> Result<Token, ServiceError>,
    token: Token,
) -> Result<(Token, User), ServiceError> {
    // Whether this call already refreshed for expiry decides how a rejection reads.
    // A token that was expired and refreshed, then rejected anyway, is rejected in its
    // freshest form: refreshing again cannot help, and a transport failure on that
    // second refresh would surface as "not reachable", keeping the session signed in
    // on a token the API will not take.
    let expired = auth::token_expired(&token);
    let token = if expired {
        refresh(&token)?
    } else {
        token
    };
    match service.current_user(&token.access_token) {
        Ok(user) => Ok((token, user)),
        // The one refresh-and-retry for a token that was still current. It is worth
        // trying once: the API rejects a token whose clock it disagrees with.
        Err(ServiceError::Unauthorized) if !expired => {
            let fresh = refresh(&token)?;
            let user = after_refresh(service.current_user(&fresh.access_token))?;
            Ok((fresh, user))
        }
        Err(ServiceError::Unauthorized) => {
            Err(ServiceError::InvalidGrant("a fresh token was rejected".to_string()))
        }
        Err(e) => Err(e),
    }
}

/// Read the answer to a request made with a freshly minted token. A token minted a
/// moment ago and rejected anyway means this account cannot be reached with it, not
/// that the network is down, so it is reported as a dead grant. Left as
/// `Unauthorized` it would read as retryable, and the session would stay signed in
/// with a token that can never work.
fn after_refresh(result: Result<User, ServiceError>) -> Result<User, ServiceError> {
    match result {
        Err(ServiceError::Unauthorized) => {
            Err(ServiceError::InvalidGrant("a fresh token was rejected".to_string()))
        }
        other => other,
    }
}

/// Refresh a token. A refresh the endpoint rejects outright surfaces as
/// [`ServiceError::InvalidGrant`]; the caller decides whether to clear the
/// store, so this worker never writes to it.
fn refresh(config: &OAuthConfig, token: &Token) -> Result<Token, ServiceError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| ServiceError::Http(e.to_string()))?;
    match tidal::refresh_token(&client, config, &token.refresh_token) {
        Ok(fresh) => {
            eprintln!("opentidal: refreshed the access token");
            Ok(fresh)
        }
        Err(e) => {
            eprintln!("opentidal: refresh failed: {e:?}");
            Err(e)
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
        let tokens: Arc<dyn TokenStore> = if live {
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
            tokens,
            library: None,
            on_disk: Library::default(),
            write_failed: false,
            write_error: None,
            db_override: None,
            profile_error: None,
            waker: None,
            live,
            session: 0,
            auth_flow: Latest::default(),
            profile: Latest::default(),
        }
    }
}

/// The Tidal user id from a token's `uid` claim, or `None` when the token is
/// not a JWT. The deterministic fake token has no claim, so the headless build
/// never touches the database.
fn session_user_id(token: &Token) -> Option<String> {
    auth::decode_claims(&token.access_token)?.user_id()
}

/// The library database path. `OPENTIDAL_DB` names it outright, which the
/// headless harness uses to prove persistence. Otherwise it sits beside the
/// config file, so the app owns one directory per user.
fn db_path() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("OPENTIDAL_DB")
        && !path.is_empty()
    {
        return Some(std::path::PathBuf::from(path));
    }
    db_path_beside(config_path()?)
}

/// The database sits beside the config file, so the app owns one directory per
/// user. Split out from [`db_path`] so a test can ask the question without
/// touching the environment.
pub(crate) fn db_path_beside(config: std::path::PathBuf) -> Option<std::path::PathBuf> {
    Some(config.with_file_name("opentidal.db"))
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
    if let Ok(path) = std::env::var("OPENTIDAL_CONFIG")
        && !path.is_empty()
    {
        return Some(std::path::PathBuf::from(path));
    }
    config_path_under(&std::path::PathBuf::from(std::env::var("HOME").ok()?))
}

/// The config path this app would use with no override, which is where its database
/// sits beside it. A store check compares against this, so a directory that merely
/// shares the app's name is not treated as the app's own.
pub(crate) fn app_directory_suffix() -> Option<std::path::PathBuf> {
    let home = std::path::PathBuf::from("home");
    let config = config_path_under(&home)?;
    config.parent().map(|p| p.strip_prefix(&home).ok().map(|r| r.to_path_buf()).unwrap_or_else(|| p.to_path_buf()))
}

/// The default config path under a given home directory. Split out from
/// [`config_path`] so a test can ask where the app keeps its files without
/// touching the environment.
fn config_path_under(home: &std::path::Path) -> Option<std::path::PathBuf> {
    let sub = if cfg!(target_os = "macos") {
        "Library/Application Support/OpenTidal/config.json"
    } else {
        ".config/opentidal/config.json"
    };
    Some(home.join(sub))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::TrackId;

    /// The directory this app really keeps its database in is the one the backup
    /// exclusion matches. Both halves are the real functions, so a change to where
    /// the app keeps its files fails here instead of silently stopping the
    /// exclusion of the `-wal` file.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_default_database_directory_is_the_one_excluded() {
        let config = config_path_under(std::path::Path::new("/Users/example")).unwrap();
        let default_db = db_path_beside(config).unwrap();
        let dir = default_db.parent().unwrap();
        assert!(
            crate::store::is_app_directory(dir),
            "the default database directory {} is not the one the exclusion matches",
            dir.display()
        );
        // Another program's directory must not match, or one flag would cover its
        // files too.
        assert!(!crate::store::is_app_directory(std::path::Path::new(
            "/Users/example/Documents"
        )));
        // Another program's directory with the same name must not match either, or one
        // flag would leave its files out of the backup.
        assert!(!crate::store::is_app_directory(std::path::Path::new(
            "/Volumes/Work/Other App/OpenTidal"
        )));
    }

    /// The deterministic build signs in with no store, which is by design. Sign-out
    /// must still work there: there is no store, so nothing can be lost, and a
    /// refusal would leave no way out at all.
    #[test]
    fn the_storeless_session_can_still_sign_out() {
        let mut app = app_without_a_session();
        app.auth = Auth::SignedIn { token: fake_token() };
        assert!(app.library.is_none(), "the fixture opened a library");

        app.sign_out();

        assert_eq!(app.auth, Auth::SignedOut, "a session with no store could not sign out");
        assert!(app.write_error.is_none());
    }

    /// An app on its way to a session. The account id comes from `open_library`,
    /// so nothing here can set one without the database it keys.
    ///
    /// The token store and the service are forced to the in-memory and fake ones.
    /// `App::default()` reads `OPENTIDAL_LIVE` and the user's config file, so
    /// without this a developer with live mode on would run these tests against the
    /// real keychain, and a test that clears the token would delete their session.
    fn app_without_a_session() -> App {
        let app = App {
            tokens: Arc::new(auth::MemoryStore::default()),
            service: Arc::new(tidal::FakeService::default()),
            live: false,
            ..App::default()
        };
        // A fresh store, so the tests start from no token whatever the config says.
        app.tokens.clear().unwrap();
        app
    }

    /// A temporary directory that removes itself, so a failing test leaves no
    /// database behind.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir = std::env::temp_dir()
                .join(format!("opentidal-app-{name}-{}", std::process::id()));
            TempDir(dir)
        }

        fn path(&self) -> std::path::PathBuf {
            self.0.join("opentidal.db")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A library written by one run is the library the next run shows, with no
    /// network step in between.
    #[test]
    fn a_restart_shows_the_saved_library() {
        let dir = TempDir::new("restart");
        let path = dir.path();

        let mut first = app_without_a_session();
        first.open_library("173210380", &path);
        first.core.lib.set_name("Ada".to_string());
        let id = first.core.lib.create_playlist("Road trip".to_string()).unwrap();
        first.core.lib.add_track(id, TrackId(3));
        first.flush_library();

        // A second run of the app, built through the same fixture so this test
        // cannot reach the real keychain when live mode is on.
        let mut second = app_without_a_session();
        second.open_library("173210380", &path);
        assert_eq!(second.core.lib.name, "Ada");
        assert_eq!(second.core.lib.playlist(id).unwrap().name, "Road trip");
        assert_eq!(second.core.lib.playlist(id).unwrap().tracks, vec![TrackId(3)]);
        assert!(second.library.is_some());

    }

    /// A rejected refresh token closes the library as well as the token. Leaving
    /// it open let a later sign-in inherit the rows and save them under its own id.
    #[test]
    fn a_dead_refresh_token_closes_the_library() {
        let dir = TempDir::new("invalid-grant");
        let path = dir.path();

        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        app.core.lib.set_name("Ada".to_string());
        app.core.lib.toggle_like(TrackId(1));
        app.auth = Auth::SignedIn { token: fake_token() };
        assert!(app.library.is_some());

        // The work runs on a worker thread, so the drain has to wait for the
        // result to land. The result must carry the current session, or the drain
        // discards it as stale.
        let session = app.session;
        let dead = ServiceError::InvalidGrant("refresh token revoked".to_string());
        app.profile.request((), move |_| (session, Err(dead)));
        for _ in 0..1_000 {
            if app.drain() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        assert!(app.library.is_none(), "a dead token left the library writable");
        assert_eq!(app.core.lib, Library::default(), "the library stayed in memory");
        assert_eq!(app.tokens.load(), None, "the dead token stayed stored");
    }

    /// A driven scene must not touch anything the user owns. The `signed-in-stored`
    /// scene writes a fabricated token and a fabricated library, so with live mode on
    /// it used to reach the real keychain and the real database. That happened for
    /// real: an old binary in live mode replaced a real token.
    ///
    /// This asks the question the scene has to answer, with no live run: after the
    /// scene, is the token store in memory, and does the database path point away
    /// from the real one?
    #[test]
    fn the_stored_scene_owns_its_store_and_its_database() {
        // A store that is not the memory one, standing in for the real keychain.
        // It records nothing and touches no file, so removing the scene's own store
        // makes the test fail instead of damaging the machine.
        struct NotMemory;
        impl auth::TokenStore for NotMemory {
            fn is_memory(&self) -> bool {
                false
            }
            fn load(&self) -> Option<auth::Token> {
                None
            }
            fn save(&self, _token: &auth::Token) {}
            fn clear(&self) -> Result<(), String> {
                Ok(())
            }
        }
        // The test owns the path and removes it. The scene honours `OPENTIDAL_DB`, so a
        // test that cleaned up whatever path came back could delete a database it does
        // not own. It makes its own directory, points the scene at a file inside it, and
        // removes only that directory.
        let dir = std::env::temp_dir().join(format!(
            "opentidal-scene-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("scene.db");

        let mut app = App {
            tokens: std::sync::Arc::new(NotMemory),
            library: None,
            db_override: Some(db.clone()),
            ..App::default()
        };
        crate::signed_in_stored_scene(&mut app);

        assert!(
            app.tokens.is_memory(),
            "the scene's token store is the real keychain"
        );
        assert_eq!(
            app.db_override.as_ref(),
            Some(&db),
            "the scene ignored the database path it was given"
        );
        drop(app);
        std::fs::remove_file(&db).ok();
        std::fs::remove_dir_all(&dir).ok();
        assert!(!dir.exists(), "the test left its own directory behind");
    }

    /// A token minted a moment ago and rejected anyway is a dead session, not a
    /// network error. Left as `Unauthorized` it reads as retryable, and the session
    /// stays signed in with a token that can never work.
    #[test]
    fn a_fresh_token_that_is_rejected_ends_the_session() {
        let user = User {
            id: crate::tidal::UserId("173210380".to_string()),
            name: "Ada".to_string(),
        };
        assert!(
            matches!(
                after_refresh(Err(ServiceError::Unauthorized)),
                Err(ServiceError::InvalidGrant(_))
            ),
            "a rejected fresh token was left retryable"
        );
        // Every other answer is passed through: a network error must stay a network
        // error, and a good answer must stay good.
        assert!(matches!(
            after_refresh(Err(ServiceError::Http("offline".to_string()))),
            Err(ServiceError::Http(_))
        ));
        assert!(matches!(
            after_refresh(Err(ServiceError::InvalidGrant("revoked".to_string()))),
            Err(ServiceError::InvalidGrant(_))
        ));
        assert_eq!(after_refresh(Ok(user.clone())).unwrap().name, user.name);
    }

    /// A token refreshed for expiry, then rejected anyway, must not refresh a second
    /// time. A second refresh that fails on transport surfaces as "not reachable", and
    /// the session stays signed in on a token the API will not take.
    #[test]
    fn an_expiry_refreshed_token_is_not_refreshed_again() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct AlwaysRejects;
        impl MusicService for AlwaysRejects {
            fn current_user(&self, _token: &str) -> Result<User, ServiceError> {
                Err(ServiceError::Unauthorized)
            }
        }
        let calls = AtomicUsize::new(0);
        let refresh = |t: &Token| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Token {
                access_token: format!("fresh-{}", t.access_token),
                refresh_token: t.refresh_token.clone(),
                expires_in: 3600,
            })
        };
        let service: Arc<dyn MusicService> = Arc::new(AlwaysRejects);
        // Expired, so this call refreshes first and then sees the rejection.
        let expired = Token {
            access_token: "old".to_string(),
            refresh_token: "r".to_string(),
            expires_in: 0,
        };
        let result = resolve_user_with(&service, &refresh, expired);

        assert_eq!(calls.load(Ordering::SeqCst), 1, "the token was refreshed more than once");
        assert!(
            matches!(result, Err(ServiceError::InvalidGrant(_))),
            "a rejected fresh token was not treated as a dead grant: {result:?}"
        );

        // A token that is still current keeps its one refresh-and-retry, because the
        // API may be rejecting a clock disagreement.
        calls.store(0, Ordering::SeqCst);
        let current = Token {
            access_token: "current".to_string(),
            refresh_token: "r".to_string(),
            expires_in: 3600,
        };
        let result = resolve_user_with(&service, &refresh, current);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the retry refresh did not happen");
        assert!(matches!(result, Err(ServiceError::InvalidGrant(_))));
    }

    /// A launch with the network off must stay signed in. The disk has the name
    /// and the library, so a transport error is a pending fetch, not a lost
    /// session. Only a rejected refresh token ends a session.
    #[test]
    fn a_network_error_keeps_the_session_signed_in() {
        let dir = TempDir::new("offline");
        let path = dir.path();
        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        app.core.lib.set_name("Ada".to_string());
        app.auth = Auth::SignedIn { token: fake_token() };
        app.tokens.save(&fake_token());

        let session = app.session;
        let offline = ServiceError::Http("dns: no route to host".to_string());
        app.profile.request((), move |_| (session, Err(offline)));
        for _ in 0..1_000 {
            if app.drain() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        assert!(
            matches!(app.auth, Auth::SignedIn { .. }),
            "a network error ended the session: {:?}",
            app.auth
        );
        assert_eq!(app.core.lib.name, "Ada", "the name from disk was lost");
        assert!(app.library.is_some(), "the library closed on a network error");
        assert_eq!(
            app.tokens.load(),
            Some(fake_token()),
            "a network error cleared the stored token"
        );
        assert!(
            app.profile_error.is_some(),
            "the offline launch says nothing about the fetch it could not make"
        );

        // A later fetch that lands clears the reason, so a message about a network
        // that came back does not sit over the name it just refreshed.
        let session = app.session;
        let user = crate::tidal::User {
            id: crate::tidal::UserId("173210380".to_string()),
            name: "Ada".to_string(),
        };
        app.profile.request((), move |_| (session, Ok((fake_token(), user))));
        for _ in 0..1_000 {
            if app.drain() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            app.profile_error.is_none(),
            "a landed fetch left the offline message showing"
        );
        assert_eq!(app.core.lib.name, "Ada");
    }

    /// Sign-out refuses to throw away edits it could not save. The old path
    /// flushed, ignored the error, and dropped the library, which lost the only
    /// copy of a playlist the user had just made.
    #[test]
    fn sign_out_keeps_unwritten_edits() {
        let dir = TempDir::new("signout-unwritten");
        let path = dir.path();

        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        app.auth = Auth::SignedIn { token: fake_token() };
        // A trigger that aborts every insert makes the write fail for real, with
        // the handle open, which is the state a full disk leaves.
        app.library.as_ref().unwrap().store.abort_writes_for_test();
        app.core.lib.create_playlist("Unsaved".to_string()).unwrap();

        app.sign_out();

        assert!(
            app.library.is_some(),
            "sign-out discarded unwritten edits after a failed write"
        );
        assert!(
            app.core.lib.playlist(crate::library::PlaylistId(0)).is_some(),
            "the unwritten playlist was dropped"
        );
        assert_eq!(
            app.auth,
            Auth::SignedIn { token: fake_token() },
            "a failed write left the screen in a state whose button would drop the edits"
        );
        assert!(app.write_error.is_some(), "the refusal was not explained to the user");
    }

    /// A failed write in one account must not suppress the next account's wake.
    /// The flag outliving the session would leave the new account's edits waiting
    /// for an input event that never comes.
    #[test]
    fn a_failed_write_does_not_silence_the_next_account() {
        let dir = TempDir::new("next-account");
        let path = dir.path();

        let mut app = app_without_a_session();
        app.open_library("account-a", &path);
        app.library.as_ref().unwrap().store.abort_writes_for_test();
        app.core.lib.create_playlist("A".to_string()).unwrap();
        app.flush_library();
        assert!(app.write_failed, "the fixture did not fail a write");

        app.open_library("account-b", &path);
        assert!(
            !app.write_failed,
            "the previous account's failed write left the new account unable to ask for a frame"
        );
    }

    /// Opening the account that is already open must not drop edits the last write
    /// could not land. A re-sign-in with the same token is the common path, and a
    /// detach would throw away the only copy.
    #[test]
    fn reopening_the_same_account_keeps_unwritten_edits() {
        let dir = TempDir::new("same-account");
        let path = dir.path();

        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        app.library.as_ref().unwrap().store.abort_writes_for_test();
        app.core.lib.create_playlist("Unsaved".to_string()).unwrap();

        // The same account opens again, the way a refreshed token re-establishes
        // the session.
        app.open_library("173210380", &path);

        assert!(
            app.core.lib.playlist(crate::library::PlaylistId(0)).is_some(),
            "reopening the same account dropped edits that were never written"
        );
    }

    /// A signed-in account whose database would not open must not look like it can
    /// save. The edit would land in memory, the next frame would report nothing
    /// owed, and the next account switch would drop it in silence.
    #[test]
    fn an_account_that_cannot_write_says_so() {
        let dir = TempDir::new("no-store");
        // A directory where the database file should be makes `Store::open` fail.
        let blocked = dir.0.join("blocked.db");
        std::fs::create_dir_all(&blocked).unwrap();

        let mut app = app_without_a_session();
        app.open_library("173210380", &blocked);
        app.auth = Auth::SignedIn { token: fake_token() };

        assert!(app.library.is_none(), "the fixture opened a library");
        assert!(
            app.write_error.is_some(),
            "a signed-in account with no storage shows no reason its edits will not be saved"
        );

        app.core.lib.create_playlist("Lost".to_string()).unwrap();
        assert!(
            !app.flush_library(),
            "a session with no open library reported that nothing was owed"
        );
    }

    /// A refusal belongs to the account that saw it. The next account must open
    /// its Account screen without the previous account's message on it.
    #[test]
    fn a_refusal_does_not_follow_the_next_account() {
        let dir = TempDir::new("refusal-next");
        let path = dir.path();

        let mut app = app_without_a_session();
        app.open_library("account-a", &path);
        app.auth = Auth::SignedIn { token: fake_token() };
        app.library.as_ref().unwrap().store.abort_writes_for_test();
        app.core.lib.create_playlist("A".to_string()).unwrap();
        app.sign_out();
        assert!(app.write_error.is_some(), "the fixture did not refuse the sign-out");

        app.open_library("account-b", &path);
        assert!(
            app.write_error.is_none(),
            "account A's refusal is still on screen after account B opened"
        );
    }

    /// Nothing owed to disk means no failure is outstanding. A refused sign-out
    /// followed by a dead token leaves the message set, and the early return would
    /// keep it on screen over a library that is fully written.
    #[test]
    fn a_clean_library_clears_a_stale_failure() {
        let dir = TempDir::new("clear-stale");
        let path = dir.path();

        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        app.auth = Auth::SignedIn { token: fake_token() };
        app.library.as_ref().unwrap().store.abort_writes_for_test();
        app.core.lib.create_playlist("A".to_string()).unwrap();
        app.sign_out();
        assert!(app.write_error.is_some());

        // The edit is gone again, so nothing is owed and no write will ever run.
        app.core.lib = app.on_disk.clone();
        app.flush_library();

        assert!(
            app.write_error.is_none(),
            "the account screen still reports a failed write over a library that is fully written"
        );
        assert!(
            !app.write_failed,
            "the wake flag stayed set, so the next edit would not ask for a frame"
        );
    }

    /// The refusal message clears once a write lands. Otherwise the Account screen
    /// keeps saying the changes are not on disk after they are.
    #[test]
    fn the_refusal_clears_when_a_write_lands() {
        let dir = TempDir::new("stale-refusal");
        let path = dir.path();

        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        app.auth = Auth::SignedIn { token: fake_token() };
        app.library.as_ref().unwrap().store.abort_writes_for_test();
        app.core.lib.create_playlist("Unsaved".to_string()).unwrap();
        app.sign_out();
        assert!(app.write_error.is_some(), "the fixture did not refuse the sign-out");

        // The disk accepts writes again and the next frame settles the library.
        Store::open(&path).unwrap().allow_writes_again_for_test();
        app.flush_library();

        assert!(
            app.write_error.is_none(),
            "the account screen still says the changes are not written after they are"
        );
    }

    /// An account whose first row write failed still gets its row, without any
    /// further edit from the user. The write inside `open_library` is made to fail
    /// by a trigger that lives in the file, so `row_pending` is set by the code
    /// and not by the test.
    #[test]
    fn a_missing_row_is_written_without_an_edit() {
        let dir = TempDir::new("retry-row");
        let path = dir.path();

        // The trigger is written into the file, so the account opens against a
        // database that refuses its first write, the way a full disk would.
        let seed = Store::open(&path).unwrap();
        seed.abort_writes_for_test();
        drop(seed);

        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        assert!(
            app.library.as_ref().is_some_and(|open| open.row_pending),
            "a failed first write was not recorded as owed"
        );
        assert_eq!(
            app.core.lib, app.on_disk,
            "the retry must not depend on the libraries differing"
        );

        // The disk frees up, and the next frame settles the debt.
        Store::open(&path).unwrap().allow_writes_again_for_test();
        app.flush_library();

        let reopened = Store::open(&path).unwrap();
        assert!(
            reopened.load("173210380").unwrap().is_some(),
            "the row was never written, so the account would be new on every launch"
        );
        assert!(!app.library.as_ref().unwrap().row_pending, "the retry stayed pending");
    }

    /// A failed restore must not leave the previous account signed in and
    /// writable. Otherwise the new account sees the old rows and its saves land
    /// under the old id, which is the one thing the account key exists to stop.
    #[test]
    fn a_failed_restore_detaches_the_previous_account() {
        let dir = TempDir::new("failed-restore");
        let path = dir.path();

        let mut app = app_without_a_session();
        app.open_library("account-a", &path);
        let a = app.core.lib.create_playlist("A".to_string()).unwrap();
        assert!(app.library.is_some());

        // A directory where the database should be makes `Store::open` fail.
        let blocked = dir.0.join("blocked.db");
        std::fs::create_dir_all(&blocked).unwrap();
        app.open_library("account-b", &blocked);

        assert!(
            app.library.is_none(),
            "account A's library stayed writable after B failed to open"
        );
        assert_eq!(app.core.lib, Library::default(), "A's rows stayed on screen");
        assert_eq!(app.core.lib.playlist(a), None);
    }

    /// Signing out clears the library, so a route to one of its playlists must go
    /// too. The state invariant forbids a route to a playlist that no longer
    /// exists, and a stacked playlist screen would become one.
    #[test]
    fn signing_out_leaves_no_route_to_a_gone_playlist() {
        let dir = TempDir::new("signout-route");
        let path = dir.path();

        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        let id = app.core.lib.create_playlist("Keep".to_string()).unwrap();
        crate::state::open_detail(&mut app.core, crate::state::Route::UserPlaylist(id));
        assert_eq!(app.core.stack.last(), Some(&crate::state::Route::UserPlaylist(id)));
        assert!(crate::state::invariant(&app.core), "the fixture is already invalid");

        app.forget_session();
        assert!(
            crate::state::invariant(&app.core),
            "a route to a playlist that no longer exists survived sign-out"
        );
    }

    /// A dirty library with nothing to write it to asks for no frame. Otherwise
    /// the app would wake, fail to save, stay dirty, and wake again forever, which
    /// is a spinning CPU on a machine with no session or a full disk.
    #[test]
    fn a_dirty_library_with_no_session_asks_for_no_frame() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let mut app = app_without_a_session();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        app.waker = Some(Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }));

        app.core.lib.toggle_like(TrackId(3));
        app.wake_if_dirty();
        assert_eq!(count.load(Ordering::SeqCst), 0, "a library with no store woke the loop");

        app.flush_library();
        app.wake_if_dirty();
        assert_eq!(count.load(Ordering::SeqCst), 0, "a failed write kept waking the loop");
    }

    /// A stored token that names no account signs out instead of signing in.
    /// Keeping it would show a session with no library behind it.
    #[test]
    fn a_token_without_an_account_signs_out() {
        let mut app = app_without_a_session();
        app.tokens.save(&fake_token());
        app.setup(std::sync::Arc::new(|| ()));
        assert_eq!(app.auth, Auth::SignedOut);
        assert_eq!(app.tokens.load(), None, "the unusable token stayed stored");
    }

    /// A new account has a row on disk before any name arrives, so a restart
    /// finds it. Without the row the next launch treats the account as new.
    #[test]
    fn a_new_account_is_written_at_once() {
        let dir = TempDir::new("new-account");
        let path = dir.path();

        let mut app = app_without_a_session();
        app.open_library("999", &path);
        assert!(app.library.is_some(), "no session was established");

        let reopened = Store::open(&path).unwrap();
        assert!(
            reopened.load("999").unwrap().is_some(),
            "a new account left no row, so a restart would not find it"
        );
    }

    /// A stored item the catalogue cannot resolve costs only that item. The app
    /// opens, keeps the rest of the library, and stays writable, because the
    /// next sync restores what was dropped.
    #[test]
    fn an_unknown_item_costs_only_that_item() {
        let dir = TempDir::new("unknown-item");
        let path = dir.path();

        let mut seeded = app_without_a_session();
        seeded.open_library("173210380", &path);
        seeded.core.lib.set_name("Ada".to_string());
        seeded.core.lib.toggle_like(TrackId(0));
        seeded.core.lib.toggle_like(TrackId(1));
        seeded.flush_library();
        drop(seeded);

        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE \"like\" SET track_id = 999 WHERE track_id = 1 AND user_id = '173210380'",
            [],
        )
        .unwrap();
        drop(conn);

        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        assert!(app.library.is_some(), "one unknown item closed the database");
        assert_eq!(app.core.lib.name, "Ada");
        assert_eq!(app.core.lib.liked.count_ones(), 1, "the good like was lost");

        app.core.lib.toggle_like(TrackId(2));
        app.flush_library();
        let mut reread = app_without_a_session();
        reread.open_library("173210380", &path);
        assert_eq!(reread.core.lib.liked.count_ones(), 2, "the new like was not saved");

    }

    /// A file that is not a database at all keeps its bytes. There is nothing to
    /// open, and nothing to write over.
    #[test]
    fn a_broken_file_is_never_overwritten() {
        let dir = TempDir::new("broken");
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.path();
        std::fs::write(&path, b"not a database").unwrap();

        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        assert!(app.library.is_none(), "a broken file was opened for writing");

        app.core.lib.set_name("Ada".to_string());
        app.flush_library();
        assert_eq!(std::fs::read(&path).unwrap(), b"not a database");

    }

    /// Signing out of account A and into account B shows B's empty library, not
    /// A's rows. A leak here writes A's data under B's id.
    #[test]
    fn a_second_account_does_not_inherit_the_first() {
        let dir = TempDir::new("accounts");
        let path = dir.path();

        let mut first = app_without_a_session();
        first.open_library("111", &path);
        first.core.lib.set_name("Ada".to_string());
        first.core.lib.toggle_like(TrackId(0));
        first.flush_library();
        first.sign_out();
        first.open_library("222", &path);

        assert!(first.core.lib.liked == 0, "account B inherited A's likes");
        assert!(first.library.is_some(), "account B has no database");

    }

    /// A quit whose write fails must not report the library as on disk. Nothing can
    /// refuse a quit, so the failure is a lost edit that has to be reported.
    #[test]
    fn a_quit_that_cannot_write_reports_the_edits_as_lost() {
        let dir = TempDir::new("quit-fail");
        let path = dir.path();
        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        app.core.lib.set_name("Ada".to_string());
        assert!(app.flush_library());
        app.library.as_ref().unwrap().store.abort_writes_for_test();

        app.core.lib.set_name("Grace".to_string());
        assert!(
            !app.quit(),
            "a quit whose write failed reported the library as on disk"
        );
        assert_eq!(
            app.library
                .as_ref()
                .unwrap()
                .store
                .load("173210380")
                .unwrap()
                .unwrap()
                .lib
                .name,
            "Ada",
            "the failed write was supposed to leave the old name on disk"
        );
    }

    /// A quit writes what is outstanding. The window's quit takes the fast exit,
    /// which drops no state, so a destructor would never run.
    #[test]
    fn a_quit_writes_what_is_outstanding() {
        let dir = TempDir::new("quit");
        let path = dir.path();

        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        app.core.lib.set_name("Ada".to_string());
        app.core.lib.toggle_like(TrackId(5));
        app.quit();

        let mut reread = app_without_a_session();
        reread.open_library("173210380", &path);
        assert_eq!(reread.core.lib.name, "Ada");
        assert!(reread.core.lib.is_liked(TrackId(5)));

    }

    /// A dirty library asks for another frame, and a clean one does not. A
    /// window at rest draws no frames, so this is what gets a change on disk.
    #[test]
    fn only_a_dirty_library_asks_for_a_frame() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = TempDir::new("wake");
        let path = dir.path();
        let mut app = app_without_a_session();
        app.open_library("173210380", &path);

        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        app.waker = Some(Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }));

        app.wake_if_dirty();
        assert_eq!(count.load(Ordering::SeqCst), 0, "a clean library woke the loop");

        app.core.lib.toggle_like(TrackId(3));
        app.wake_if_dirty();
        assert_eq!(count.load(Ordering::SeqCst), 1, "a change did not wake the loop");

    }

    /// The fake token carries no `uid` claim, so the headless build never
    /// touches a database file.
    #[test]
    fn the_fake_token_names_no_user() {
        assert_eq!(session_user_id(&fake_token()), None);
    }

    /// The user id comes from the token claim, which is what keys the rows.
    #[test]
    fn the_user_id_comes_from_the_token() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let payload = URL_SAFE_NO_PAD.encode(br#"{"uid":173210380}"#);
        let token = Token {
            access_token: format!("h.{payload}.sig"),
            refresh_token: "r".to_string(),
            expires_in: 1,
        };
        assert_eq!(session_user_id(&token).as_deref(), Some("173210380"));
    }
}
