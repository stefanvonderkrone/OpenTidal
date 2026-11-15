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
    pub store: Arc<dyn TokenStore>,
    /// The library database, open only while the token names a user. The
    /// deterministic fake signs in with a token that is not a JWT, so no user
    /// id is known and nothing is written in the headless build.
    pub db: Option<Store>,
    /// The user id from the token's `uid` claim, when a session is established.
    pub user_id: Option<String>,
    /// The library as last written to disk, so a frame can tell whether the
    /// in-memory library changed.
    saved: Library,
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
            && let Some(token) = self.store.load()
        {
            eprintln!("opentidal: restoring a stored session");
            self.user_id = session_user_id(&token);
            if let Some(user_id) = self.user_id.clone() {
                self.restore_library(&user_id);
            }
            self.auth = Auth::SignedIn { token: token.clone() };
            self.fetch_profile(token);
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
    /// The testable half of [`App::restore_library`], with the path supplied.
    fn open_library(&mut self, user_id: &str, path: &std::path::Path) {
        // The user id and the open database are one fact: a session that can
        // write needs both, so taking the id here means no caller can open the
        // library and forget the key it will save under.
        self.user_id = Some(user_id.to_string());
        let db = match Store::open(path) {
            Ok(db) => db,
            Err(error) => {
                eprintln!("opentidal: cannot open {}: {error:?}", path.display());
                return;
            }
        };
        // A missing row means a new account, so the in-memory library must not
        // carry the previous account's rows into this one.
        match db.load(user_id) {
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
            }
            Err(error) => {
                eprintln!("opentidal: cannot load the library, leaving it unwritten: {error:?}");
                return;
            }
        }
        self.saved = self.core.lib.clone();
        self.db = Some(db);
    }

    /// Write the library when it differs from what is on disk. This runs at the
    /// start of every frame, so a click in one frame is on disk by the next.
    /// One comparison covers every operation in the UI, so a new screen action
    /// needs no persistence call of its own.
    fn flush_library(&mut self) {
        if self.core.lib == self.saved {
            return;
        }
        let Some(user_id) = self.user_id.clone() else {
            return;
        };
        let Some(db) = self.db.as_mut() else {
            return;
        };
        match db.save(&user_id, &self.core.lib) {
            Ok(()) => self.saved = self.core.lib.clone(),
            Err(error) => eprintln!("opentidal: cannot save the library: {error:?}"),
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
        match self.store.clear() {
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
        self.db = None;
        self.user_id = None;
        self.core.lib = Library::default();
        self.saved = Library::default();
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
                    self.store.save(&token);
                    self.user_id = session_user_id(&token);
                    if let Some(user_id) = self.user_id.clone() {
                        self.restore_library(&user_id);
                    }
                    self.auth = Auth::SignedIn { token: token.clone() };
                    self.fetch_profile(token);
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
                    self.store.save(&token);
                    if let Auth::SignedIn { token: held } = &mut self.auth {
                        *held = token;
                    }
                    self.core.lib.set_name(user.name);
                }
                Err(e) => {
                    if matches!(&e, ServiceError::InvalidGrant(_)) {
                        let _ = self.store.clear();
                    }
                    self.auth = Auth::Failed { message: format!("{e:?}") };
                }
            }
        }
        self.flush_library();
        changed
    }

    /// Ask for one more frame when the library changed. The store write happens
    /// in that frame's `drain`. A window at rest draws no frames, so a change in
    /// the frame that puts it to rest would otherwise wait for the next input.
    pub fn wake_if_dirty(&self) {
        if self.core.lib != self.saved
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
            db: None,
            user_id: None,
            saved: Library::default(),
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

    fn db_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("opentidal-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// An app with the user id a live session would carry, so a save has a key.
    fn signed_in(user_id: &str) -> App {
        App { user_id: Some(user_id.to_string()), ..Default::default() }
    }

    /// A library written by one run is the library the next run shows, with no
    /// network step in between.
    #[test]
    fn a_restart_shows_the_saved_library() {
        let dir = db_dir("restart");
        let path = dir.join("opentidal.db");

        let mut first = signed_in("173210380");
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
        assert!(second.db.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stored item the catalogue cannot resolve costs only that item. The app
    /// opens, keeps the rest of the library, and stays writable, because the
    /// next sync restores what was dropped.
    #[test]
    fn an_unknown_item_costs_only_that_item() {        let dir = db_dir("unreadable");
        let path = dir.join("opentidal.db");

        let mut seeded = signed_in("173210380");
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

        let mut app = signed_in("173210380");
        app.open_library("173210380", &path);
        assert!(app.db.is_some(), "one unknown item closed the database");
        assert_eq!(app.core.lib.name, "Ada");
        assert_eq!(app.core.lib.liked.count_ones(), 1, "the good like was lost");

        app.core.lib.toggle_like(TrackId(2));
        app.flush_library();
        let mut reread = signed_in("173210380");
        reread.open_library("173210380", &path);
        assert_eq!(reread.core.lib.liked.count_ones(), 2, "the new like was not saved");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that is not a database at all keeps its bytes. There is nothing to
    /// open, and nothing to write over.
    #[test]
    fn a_broken_file_is_never_overwritten() {
        let dir = db_dir("broken");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("opentidal.db");
        std::fs::write(&path, b"not a database").unwrap();

        let mut app = signed_in("173210380");
        app.open_library("173210380", &path);
        assert!(app.db.is_none(), "a broken file was opened for writing");

        app.core.lib.set_name("Ada".to_string());
        app.flush_library();
        assert_eq!(std::fs::read(&path).unwrap(), b"not a database");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Signing out of account A and into account B shows B's empty library, not
    /// A's rows. A leak here writes A's data under B's id.
    #[test]
    fn a_second_account_does_not_inherit_the_first() {
        let dir = db_dir("accounts");
        let path = dir.join("opentidal.db");

        let mut first = signed_in("111");
        first.open_library("111", &path);
        first.core.lib.set_name("Ada".to_string());
        first.core.lib.toggle_like(TrackId(0));
        first.flush_library();
        first.sign_out();
        first.open_library("222", &path);

        assert!(first.core.lib.liked == 0, "account B inherited A's likes");
        assert!(first.db.is_some(), "account B has no database");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A quit writes what is outstanding. The window's quit takes the fast exit,
    /// which drops no state, so a destructor would never run.
    #[test]
    fn a_quit_writes_what_is_outstanding() {
        let dir = db_dir("quit");
        let path = dir.join("opentidal.db");

        let mut app = signed_in("173210380");
        app.open_library("173210380", &path);
        app.core.lib.set_name("Ada".to_string());
        app.core.lib.toggle_like(TrackId(5));
        app.quit();

        let mut reread = signed_in("173210380");
        reread.open_library("173210380", &path);
        assert_eq!(reread.core.lib.name, "Ada");
        assert!(reread.core.lib.is_liked(TrackId(5)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dirty library asks for another frame, and a clean one does not. A
    /// window at rest draws no frames, so this is what gets a change on disk.
    #[test]
    fn only_a_dirty_library_asks_for_a_frame() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = db_dir("wake");
        let path = dir.join("opentidal.db");
        let mut app = signed_in("173210380");
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

        let _ = std::fs::remove_dir_all(&dir);
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
