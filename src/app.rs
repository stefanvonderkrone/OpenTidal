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
    /// Wakes the event loop so `poll` runs after a change. A windowed app stops
    /// drawing frames at rest, so without this a click in the last frame before
    /// idle would sit unsaved until the next input.
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
        let Some(path) = db_path() else {
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
        let mut store = match Store::open(path) {
            Ok(store) => store,
            Err(error) => {
                eprintln!("opentidal: cannot open {}: {error:?}", path.display());
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
                if let Err(error) = store.write(user_id, &self.core.lib) {
                    eprintln!("opentidal: cannot open a row for a new account: {error:?}");
                    return;
                }
                self.on_disk = self.core.lib.clone();
                self.library = Some(OpenLibrary { user_id: user_id.to_string(), store });
                return;
            }
            Err(error) => {
                eprintln!("opentidal: cannot load the library, leaving it unwritten: {error:?}");
                return;
            }
        }
        self.on_disk = self.core.lib.clone();
        self.library = Some(OpenLibrary { user_id: user_id.to_string(), store });
    }

    /// Write the library when it differs from what is on disk. This runs at the
    /// start of every frame, so a click in one frame is on disk by the next.
    /// One comparison covers every operation in the UI, so a new screen action
    /// needs no persistence call of its own.
    fn flush_library(&mut self) {
        if self.core.lib == self.on_disk {
            return;
        }
        let Some(open) = self.library.as_mut() else {
            return;
        };
        match open.store.write(&open.user_id, &self.core.lib) {
            Ok(()) => {
                self.on_disk = self.core.lib.clone();
                self.write_failed = false;
            }
            Err(error) => {
                // Keep the library dirty so a later edit tries again, but stop
                // waking for this failure.
                self.write_failed = true;
                eprintln!("opentidal: cannot save the library: {error:?}");
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
    fn forget_session(&mut self) {
        self.flush_library();
        self.library = None;
        self.core.lib = Library::default();
        self.on_disk = Library::default();
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
                }
                Err(e) => {
                    // A dead refresh token means no session at all. The rows stay
                    // on disk for a later sign-in, but the library in memory must
                    // not survive, or the next account to sign in would inherit
                    // it and save it under its own id.
                    if matches!(&e, ServiceError::InvalidGrant(_)) {
                        let _ = self.tokens.clear();
                        self.forget_session();
                    }
                    self.auth = Auth::Failed { message: format!("{e:?}") };
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
    pub fn quit(&mut self) {
        self.flush_library();
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
    let token = if auth::token_expired(&token) {
        refresh(config, &token)?
    } else {
        token
    };
    match service.current_user(&token.access_token) {
        Ok(user) => Ok((token, user)),
        Err(ServiceError::Unauthorized) => {
            let fresh = refresh(config, &token)?;
            let user = service.current_user(&fresh.access_token)?;
            Ok((fresh, user))
        }
        Err(e) => Err(e),
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
    Some(config_path()?.with_file_name("opentidal.db"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::TrackId;

    /// An app on its way to a session. The account id comes from `open_library`,
    /// so nothing here can set one without the database it keys.
    fn app_without_a_session() -> App {
        App::default()
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

        let mut second = App::default();
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
