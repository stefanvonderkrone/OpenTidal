//! The rune app state: the proven [`State`] core plus the ephemeral parts that
//! must not enter it. The edit buffers back the search and rename fields; the
//! auth, service and token store hold the network side, so `State` stays
//! `PartialEq`/`Eq`/`Hash` and the invariant proof stays bounded.
//!
//! The service is a [`MusicService`] in an `Arc`. The default is the
//! deterministic [`FakeService`]; set `OPENTIDAL_LIVE=1` to use the real
//! [`HttpService`] and the macOS keychain.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use rune::{Image, ImageCache, Latest, TextEdit, Waker};
use serde::Deserialize;
use crate::auth::{self, Auth, OAuthConfig, Token, TokenStore};
use crate::catalogue::{Album, Artist, ArtworkId, Catalogue, CataloguePlaylist, CoverOwner, CoverRequest};
use crate::covers::{self, CoverCache, CoverOutcome};
use crate::library::{Library, RemoteList};
use crate::pager::{Landed, PageResult, PassStatus, Passes};
use crate::state::State;
use crate::store::Store;
use crate::tidal::{self, MusicService, ServiceError, User};

pub struct App {
    pub core: State,
    /// The one catalogue. It starts from the seed and only grows: a load absorbs
    /// the catalogue rows the file holds. `State` holds ids only, so the proof
    /// never hashes the catalogue.
    pub catalogue: Catalogue,
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
    /// Whether the pending profile fetch is a renewal a rejected page forced. A
    /// renewal starts no sync. Set with each profile request.
    profile_renews: bool,
    /// Refreshes a token. A field, so a test renews without a network.
    pub(crate) refresh: fn(&OAuthConfig, &Token) -> Result<Token, ServiceError>,
    /// The page worker, drained by `poll`. `Passes` asks for one page at a
    /// time, so `Latest` never replaces a page in flight for the same account.
    pub pages: Latest<PageFetch>,
    /// Mints pass ids. Never reset, so a page for a closed library finds no pass.
    next_pass: u64,
    /// Whether entering a list's route starts a pass. `live` by default. A
    /// deterministic scene turns it off, so its fixture library is not erased.
    pub(crate) revalidate_on_entry: bool,
    /// `Catalogue::revision` at the last write, so a change to the catalogue
    /// alone, such as a playlist's tracks, still reaches disk.
    on_disk_revision: u64,
    /// The process's image cache, made in `setup` with the waker. `None` before
    /// setup, so nothing decodes on the startup path.
    images: Option<ImageCache>,
    /// The cover worker. One batch at a time: a batch starts only when none is
    /// pending, so `Latest` never replaces one.
    pub cover_fetch: Latest<CoverFetch>,
    /// Where cover caches live when it is not `covers::cover_root()`. A scene
    /// or a test sets it, beside `db_override`.
    pub(crate) cover_root_override: Option<PathBuf>,
    /// Block on each cover decode as it lands. Only the `signed-in-covers` scene
    /// sets it, so the decode is done before a headless snap. An interactive
    /// app relies on the image cache's waker instead.
    pub(crate) wait_for_decode: bool,
}

/// The session and the account a worker started under. The result is applied
/// only when both still match, which `ticket_holds` checks. The session alone is
/// not enough: a dead refresh token closes the library without a new session.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ticket {
    session: u64,
    user_id: String,
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
    /// This account's covers, or `None` when no cover root can be named.
    /// Dropping the open library drops every shown image and the cache handle,
    /// so no cover outlives its account in memory.
    covers: Option<AccountCovers>,
    /// This account's passes. Dropped with the library, so a page that lands
    /// for another account finds no pass.
    passes: Passes,
}

/// One account's covers. `states` is the only record of what the view wants,
/// what a batch holds, and what is shown. The view and `drain` both run on the
/// UI thread and never overlap, so a `RefCell` is enough.
struct AccountCovers {
    cache: CoverCache,
    states: RefCell<BTreeMap<ArtworkId, CoverState>>,
    /// How many shown images had decoded at the last drain, so a drain can
    /// tell that a decode landed and the screen must redraw.
    ready: Cell<usize>,
}

/// One cover's place in its life. One enum in one map, so a cover cannot be
/// both queued and shown. Named state, not slot, because **slot** is a user
/// playlist position.
enum CoverState {
    /// The view drew it and no batch took it yet.
    Wanted(CoverOwner),
    /// A batch holds it. The owner returns it to `Wanted` when the batch gives
    /// it no outcome, because the batch reached its fetch cap first.
    Fetching(CoverOwner),
    /// `ImageCache::file` returned this handle. The handle keeps the decoded
    /// pixels alive, because the image cache holds only weak references.
    Shown(Image),
    /// No cover for now. A landed page clears it, so the next frame asks again.
    Unavailable,
}

/// A finished cover batch, tagged with its ticket.
type CoverFetch = (Ticket, Vec<(ArtworkId, CoverOutcome)>);

/// The size a cover decodes at: the 200 pt hero at 2x. The 168 pt cards use
/// the same handle.
const COVER_PT: f32 = 200.0;
const COVER_SCALE: f32 = 2.0;

/// A finished sign-in exchange, tagged with the session it ran under.
type AuthFlow = (u64, Result<Token, String>);

/// A finished profile fetch, tagged with its session, carrying the token to
/// persist.
type ProfileFetch = (u64, Result<(Token, User), ServiceError>);

/// A finished page, tagged with the account it was read for.
type PageFetch = (Ticket, PageResult);

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
        self.pages.set_waker(waker.clone());
        self.cover_fetch.set_waker(waker.clone());
        self.images = Some(ImageCache::new(waker.clone()));
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
        let loaded = store.load(user_id, &mut self.catalogue);
        // What the file holds, so the drain writes back a list emptied below.
        let loaded_revision = self.catalogue.revision();
        // One file serves every account, and a Tidal playlist keeps one track
        // list for all of them, so with another account in the file this one
        // could see that account's list. A file read failure counts as shared.
        if loaded.is_ok() && store.holds_another_account(user_id).unwrap_or(true) {
            self.catalogue.forget_playlist_tracks();
        }
        match loaded {
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
                let row_pending = match store.write(user_id, &self.core.lib, &self.catalogue) {
                    Ok(()) => {
                        self.on_disk = self.core.lib.clone();
                        self.on_disk_revision = self.catalogue.revision();
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
                    covers: self.account_covers(user_id),
                    passes: Passes::default(),
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
        self.on_disk_revision = loaded_revision;
        self.library = Some(OpenLibrary {
            user_id: user_id.to_string(),
            store,
            row_pending: false,
            covers: self.account_covers(user_id),
            passes: Passes::default(),
        });
    }

    /// The covers of a newly opened account, under the override root or the
    /// XDG cache root. Does no I/O, so it is safe on the startup path.
    fn account_covers(&self, user_id: &str) -> Option<AccountCovers> {
        let root = self.cover_root_override.clone().or_else(|| {
            let home = std::env::var_os("HOME");
            covers::cover_root(std::env::var_os("XDG_CACHE_HOME").as_deref(), home.as_deref().map(std::path::Path::new))
        })?;
        let cache = CoverCache::for_account(&root, user_id)?;
        Some(AccountCovers { cache, states: RefCell::new(BTreeMap::new()), ready: Cell::new(0) })
    }

    /// The view-facing covers. With no open library every cover is a gradient.
    pub fn covers(&self) -> Covers<'_> {
        Covers {
            open: self.library.as_ref().and_then(|open| open.covers.as_ref()),
            waker: self.waker.as_ref(),
        }
    }

    /// Land a cover batch, settle decoded images, then start the next batch.
    /// Returns true when the screen must redraw.
    fn drain_covers(&mut self) -> bool {
        let mut changed = false;
        let landed = self.cover_fetch.take();
        let holds = landed.as_ref().is_some_and(|(ticket, _)| self.ticket_holds(ticket));
        let Some(open) = self.library.as_ref().and_then(|open| open.covers.as_ref()) else {
            return false;
        };
        let mut states = open.states.borrow_mut();
        if let Some((_, outcomes)) = landed {
            if holds {
                changed = true;
                for (id, outcome) in outcomes {
                    // A deferred cover stays `Fetching`, and the sweep below
                    // returns it to `Wanted` with the owner it holds.
                    if outcome == CoverOutcome::Deferred {
                        continue;
                    }
                    let state = match (outcome, &self.images) {
                        (CoverOutcome::Ready(path), Some(images)) => {
                            let image = images.file(path, COVER_PT, COVER_SCALE);
                            if self.wait_for_decode {
                                let _ = image.wait();
                            }
                            CoverState::Shown(image)
                        }
                        _ => CoverState::Unavailable,
                    };
                    states.insert(id, state);
                }
            }
            // A cover the batch gave no outcome waits for the next one. A stale
            // batch gives none, so its covers go back to wanted as well.
            for state in states.values_mut() {
                if let CoverState::Fetching(owner) = state {
                    *state = CoverState::Wanted(owner.clone());
                }
            }
        }
        let mut ready = 0;
        for (id, state) in states.iter_mut() {
            let CoverState::Shown(image) = state else { continue };
            if image.error().is_some() {
                // A file that will not decode is removed, so a later batch
                // fetches it again rather than serving it as a hit.
                open.cache.evict(id);
                *state = CoverState::Unavailable;
                changed = true;
            } else if image.is_ready() {
                ready += 1;
            }
        }
        if open.ready.replace(ready) != ready {
            changed = true;
        }
        let idle = !self.cover_fetch.is_pending() && !self.token_in_use() && self.images.is_some();
        let (Some(library), Auth::SignedIn { token }) = (&self.library, &self.auth) else {
            return changed;
        };
        if !idle {
            return changed;
        }
        let requests: Vec<CoverRequest> = states
            .iter_mut()
            .filter_map(|(id, state)| {
                let CoverState::Wanted(owner) = state else { return None };
                let owner = owner.clone();
                *state = CoverState::Fetching(owner.clone());
                Some(CoverRequest { artwork: id.clone(), owner })
            })
            .collect();
        drop(states);
        if requests.is_empty() {
            return changed;
        }
        let ticket = Ticket { session: self.session, user_id: library.user_id.clone() };
        let cache = open.cache.clone();
        let token = (!auth::token_expired(token)).then(|| token.access_token.clone());
        let service = self.service.clone();
        self.cover_fetch.request(requests, move |requests| {
            (ticket, covers::resolve(&cache, &*service, token.as_deref(), requests))
        });
        changed
    }

    /// Clear every unavailable cover after a page lands, so the next frame asks
    /// for it again. A cover never retries on a timer.
    fn rearm_covers(&self) {
        if let Some(open) = self.library.as_ref().and_then(|open| open.covers.as_ref()) {
            open.states.borrow_mut().retain(|_, state| !matches!(state, CoverState::Unavailable));
        }
    }

    /// Whether the library or the catalogue changed since the last write. The
    /// one dirty test, so `flush_library` and `wake_if_dirty` cannot disagree.
    fn owes_disk(&self) -> bool {
        self.core.lib != self.on_disk || self.catalogue.revision() != self.on_disk_revision
    }

    /// Write the library when it differs from what is on disk. This runs at the
    /// start of every frame, so a click in one frame is on disk by the next.
    /// One comparison covers every operation in the UI, so a new screen action
    /// needs no persistence call of its own.
    /// Returns true when nothing is owed to disk: either the write landed, or
    /// there was nothing to write. A caller that is about to discard the library
    /// must check this, or a failed write loses the only copy.
    fn flush_library(&mut self) -> bool {
        let owes_disk = self.owes_disk();
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
            match open.store.write(&open.user_id, &self.core.lib, &self.catalogue) {
                Ok(()) => {
                    open.row_pending = false;
                    self.on_disk = self.core.lib.clone();
                    self.on_disk_revision = self.catalogue.revision();
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
        if !owes_disk {
            // Nothing is owed, so no failure is outstanding. Without this a refused
            // sign-out's message would sit over a library that is fully written, and
            // the wake flag would stay set so the next edit never asked for a frame.
            self.write_failed = false;
            self.write_error = None;
            return true;
        }
        match open.store.write(&open.user_id, &self.core.lib, &self.catalogue) {
            Ok(()) => {
                self.on_disk = self.core.lib.clone();
                self.on_disk_revision = self.catalogue.revision();
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
        //
        // A Tidal playlist still resolves, because the catalogue never forgets, but
        // its name belongs to the old account. The next account must not find it
        // one Back away.
        self.core.stack.retain(|route| match route {
            crate::state::Route::UserPlaylist(_) => false,
            crate::state::Route::CataloguePlaylist(id) => id.is_seed(),
            _ => true,
        });
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
        let (service, config, session, refresh) = (self.service.clone(), self.oauth.clone(), self.session, self.refresh);
        self.profile_renews = false;
        self.profile.request(token, move |t| {
            (session, with_fresh_token(&|t| refresh(&config, t), t, &|access| service.current_user(access)))
        });
    }

    /// Refresh the session's token, then prove it with a profile fetch. A
    /// rejected page or an expired token starts it. A no-op while a profile
    /// fetch is pending, because two refreshes of one refresh token race.
    fn renew_token(&mut self) {
        if self.profile.is_pending() {
            return;
        }
        let Auth::SignedIn { token } = &self.auth else {
            return;
        };
        let (service, config, session, refresh) = (self.service.clone(), self.oauth.clone(), self.session, self.refresh);
        self.profile_renews = true;
        self.profile.request(token.clone(), move |t| {
            (session, renew(&|t| refresh(&config, t), t, &|access| service.current_user(access)))
        });
    }

    /// Read every mirrored list to its last page. A no-op with no open library.
    /// The pages start in `pump`, once no profile fetch is pending.
    pub fn sync(&mut self) {
        if let Some(open) = self.library.as_mut() {
            open.passes.read_to_end(&mut self.next_pass);
        }
        // The button is the user's retry after a renewal that could not reach
        // Tidal, which `pump` does not repeat on its own.
        if let Auth::SignedIn { token } = &self.auth
            && auth::token_expired(token)
        {
            self.renew_token();
        }
    }

    /// The view shows the first `rows` members of `list`. A no-op with no open
    /// library, or with no pass for the list. The view reports its rows during
    /// layout, before the next `drain`, so this enters the route and starts the
    /// page itself. A window at rest runs no other `drain` to start it.
    /// Returns whether the list's status changed, so the view draws it.
    pub fn want(&mut self, list: RemoteList, rows: usize) -> bool {
        let before = self.pass_status(list.clone());
        self.enter_route();
        if let Some(open) = self.library.as_mut() {
            open.passes.want(&list, rows);
        }
        self.pump();
        self.pass_status(list.clone()) != before
    }

    /// How far the open account's pass for `list` got. `Idle` with no library.
    pub fn pass_status(&self, list: RemoteList) -> PassStatus {
        self.library.as_ref().map_or(PassStatus::Idle, |open| open.passes.status(&list))
    }

    /// Start a pass for the list the top route shows. `Passes::enter` reads
    /// only a change of list, so calling this every frame starts nothing new.
    fn enter_route(&mut self) {
        if !self.revalidate_on_entry {
            return;
        }
        let list = RemoteList::of(self.core.stack.last());
        if let Some(open) = self.library.as_mut() {
            open.passes.enter(list, &mut self.next_pass);
        }
    }

    /// Start the next page, if a pass wants one. Nothing starts while the
    /// profile fetch may refresh the token. An expired token is renewed first,
    /// unless the last profile fetch could not reach Tidal, so an offline
    /// session does not retry on every frame. The page worker never refreshes.
    fn pump(&mut self) {
        if self.token_in_use() {
            return;
        }
        let (Some(open), Auth::SignedIn { token }) = (self.library.as_mut(), &self.auth) else {
            return;
        };
        if auth::token_expired(token) {
            if self.profile_error.is_none() {
                self.renew_token();
            }
            return;
        }
        let Some(request) = open.passes.next_request(&token.access_token) else {
            return;
        };
        let ticket = Ticket { session: self.session, user_id: open.user_id.clone() };
        let (service, access) = (self.service.clone(), token.access_token.clone());
        self.pages.request((), move |_| {
            let result = service.page(&access, &ticket.user_id, &request.of, request.after.as_ref());
            (ticket, PageResult { pass: request.pass, result, token: access })
        });
    }

    /// Land a finished page on its pass and act on what the pass says.
    /// Returns true when a page for the open account landed.
    fn drain_pages(&mut self) -> bool {
        let Some((ticket, page)) = self.pages.take() else {
            return false;
        };
        if !self.ticket_holds(&ticket) {
            return false;
        }
        let Some(open) = self.library.as_mut() else {
            return false;
        };
        match open.passes.land(page) {
            Landed::Merge(merge) => {
                let report = self.core.lib.merge(&mut self.catalogue, merge);
                if report.dropped > 0 {
                    eprintln!("opentidal: a page named {} item(s) the catalogue cannot hold", report.dropped);
                }
                self.rearm_covers();
            }
            Landed::Renew => self.renew_token(),
            Landed::DeadGrant(error) => self.end_dead_session(error),
            Landed::Kept => {}
        }
        true
    }

    /// Whether a worker that may refresh the token is running. A page or a
    /// cover batch waits for it, because two refreshes of one refresh token
    /// race each other. Only the profile fetch refreshes.
    fn token_in_use(&self) -> bool {
        self.profile.is_pending()
    }

    /// Whether a worker result still belongs to the open library: the same
    /// session and the same account.
    fn ticket_holds(&self, ticket: &Ticket) -> bool {
        ticket.session == self.session
            && self.library.as_ref().is_some_and(|open| open.user_id == ticket.user_id)
    }

    /// Persist a token a worker used, and make it the session's token.
    fn keep_token(&mut self, token: Token) {
        self.tokens.save(&token);
        if let Auth::SignedIn { token: held } = &mut self.auth {
            *held = token;
        }
    }

    /// End a session whose refresh token is dead. The rows stay on disk for a
    /// later sign-in, but the library in memory must not survive, or the next
    /// account to sign in would inherit it and save it under its own id.
    fn end_dead_session(&mut self, error: ServiceError) {
        let _ = self.tokens.clear();
        // Say so when the last edits could not be written, rather than dropping
        // them in silence.
        if !self.forget_session() {
            eprintln!(
                "opentidal: the dead token ended the session before the library was written, so the last edits are lost"
            );
        }
        self.auth = Auth::Failed { message: format!("{error:?}") };
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
            let renewal = std::mem::take(&mut self.profile_renews);
            match result {
                Ok((token, user)) => {
                    self.keep_token(token);
                    self.core.lib.set_name(user.name);
                    // The fetch landed, so the reason an earlier one did not is out
                    // of date. Leaving it would show "could not reach Tidal" over a
                    // name that just arrived.
                    self.profile_error = None;
                    // The live build syncs once the token is proven. The fake build
                    // syncs from the Account screen only, so a scene or a test that
                    // fetches the profile never replaces its library by surprise. A
                    // renewal resumes the passes it was started for instead.
                    if self.live && !renewal {
                        self.sync();
                    }
                }
                Err(e) => {
                    if matches!(&e, ServiceError::InvalidGrant(_)) {
                        self.end_dead_session(e);
                    } else {
                        // A transport error is not a lost session. The disk holds the
                        // name and the library, so the launch stays signed in and the
                        // fetch retries on the next request. Falling back to
                        // `Auth::Failed` here would put a sign-in button over a session
                        // that still works, and the button would drop the library.
                        eprintln!(
                            "opentidal: the profile fetch failed, staying signed in: {e:?}"
                        );
                        // The token did not change, so a pass waiting for a new one
                        // would say it syncs forever.
                        if let (Some(open), Auth::SignedIn { token }) = (self.library.as_mut(), &self.auth) {
                            open.passes.renewal_failed(&token.access_token, &e);
                        }
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
        changed |= self.drain_pages();
        self.enter_route();
        self.pump();
        changed |= self.drain_covers();
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
        if self.owes_disk()
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

/// Run `call` with a fresh token, refreshing it first when it is expired or when
/// the service rejects it. Returns the token actually used, so the caller can
/// persist a renewed one. A refresh the endpoint rejects outright (for example
/// `invalid_grant`) surfaces as [`ServiceError::InvalidGrant`], which the caller
/// maps to clearing the stored token. The profile fetch uses it.
///
/// The refresh is a parameter. A test needs to count the refreshes and make them
/// fail on demand, which the network call cannot do without a server. Production
/// passes the real one, so the logic below is the same code in both.
fn with_fresh_token<T>(
    refresh: &dyn Fn(&Token) -> Result<Token, ServiceError>,
    token: Token,
    call: &dyn Fn(&str) -> Result<T, ServiceError>,
) -> Result<(Token, T), ServiceError> {
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
    match call(&token.access_token) {
        Ok(answer) => Ok((token, answer)),
        // The one refresh-and-retry for a token that was still current. It is worth
        // trying once: the API rejects a token whose clock it disagrees with. A sync
        // rejected on page nine reads again from page one, which is safe because
        // nothing was applied.
        Err(ServiceError::Unauthorized) if !expired => {
            let fresh = refresh(&token)?;
            let answer = after_refresh(call(&fresh.access_token))?;
            Ok((fresh, answer))
        }
        Err(ServiceError::Unauthorized) => {
            Err(ServiceError::InvalidGrant("a fresh token was rejected".to_string()))
        }
        Err(e) => Err(e),
    }
}

/// Refresh first, then make the call with the fresh token. A rejected page
/// forces this, because the token it carried was not expired by its clock. A
/// fresh token the service rejects too is a dead grant, through `after_refresh`.
fn renew<T>(
    refresh: &dyn Fn(&Token) -> Result<Token, ServiceError>,
    token: Token,
    call: &dyn Fn(&str) -> Result<T, ServiceError>,
) -> Result<(Token, T), ServiceError> {
    let fresh = refresh(&token)?;
    let answer = after_refresh(call(&fresh.access_token))?;
    Ok((fresh, answer))
}

/// Read the answer to a request made with a freshly minted token. A token minted a
/// moment ago and rejected anyway means this account cannot be reached with it, not
/// that the network is down, so it is reported as a dead grant. Left as
/// `Unauthorized` it would read as retryable, and the session would stay signed in
/// with a token that can never work.
fn after_refresh<T>(result: Result<T, ServiceError>) -> Result<T, ServiceError> {
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
            token_store(&env_or("OPENTIDAL_TOKENS", &file.tokens))
        } else {
            Arc::new(auth::MemoryStore::default())
        };
        let oauth = OAuthConfig::tidal(
            env_or("TIDAL_CLIENT_ID", &file.client_id),
            env_or("TIDAL_CLIENT_SECRET", &file.client_secret),
            env_or("TIDAL_REDIRECT_URI", &file.redirect_uri),
            env_or("TIDAL_SCOPE", &file.scope),
        );
        let catalogue = Catalogue::seed();
        App {
            core: State::new(&catalogue),
            catalogue,
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
            profile_renews: false,
            refresh,
            pages: Latest::default(),
            next_pass: 0,
            revalidate_on_entry: live,
            on_disk_revision: 0,
            images: None,
            cover_fetch: Latest::default(),
            cover_root_override: None,
            wait_for_decode: false,
        }
    }
}

/// A borrow of the open account's covers for one frame.
#[derive(Clone, Copy)]
pub struct Covers<'a> {
    open: Option<&'a AccountCovers>,
    waker: Option<&'a Waker>,
}

impl Covers<'_> {
    /// The cover of an album. A new artwork id is recorded as wanted, and the
    /// loop is woken once so `drain` starts a batch. A known id costs one map
    /// lookup.
    pub fn album(self, album: &Album) -> Cover {
        self.cover(album.cover_request(), album.id.as_str())
    }

    pub fn playlist(self, playlist: &CataloguePlaylist) -> Cover {
        self.cover(playlist.cover_request(), playlist.id.as_str())
    }

    pub fn artist(self, artist: &Artist) -> Cover {
        self.cover(artist.cover_request(), artist.id.as_str())
    }

    fn cover(self, request: Option<CoverRequest>, seed: &str) -> Cover {
        let image = match (self.open, request) {
            (Some(open), Some(request)) => {
                let mut states = open.states.borrow_mut();
                match states.get(&request.artwork) {
                    Some(CoverState::Shown(image)) => Some(image.clone()),
                    Some(_) => None,
                    None => {
                        states.insert(request.artwork, CoverState::Wanted(request.owner));
                        if let Some(waker) = self.waker {
                            waker();
                        }
                        None
                    }
                }
            }
            _ => None,
        };
        Cover { image, seed: seed.to_string() }
    }
}

/// What a screen draws: a decoded image, or the gradient its seed picks.
pub struct Cover {
    image: Option<Image>,
    seed: String,
}

impl Cover {
    /// The image only when it decoded with no error.
    pub fn ready(&self) -> Option<&Image> {
        self.image.as_ref().filter(|image| image.is_ready() && image.error().is_none())
    }

    /// The text the placeholder gradient is picked from: the owner's id.
    pub fn seed(&self) -> &str {
        &self.seed
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
    tokens: String,
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

fn wants_keychain(choice: &str) -> bool {
    choice.trim() == "keychain"
}

fn token_store(choice: &str) -> Arc<dyn TokenStore> {
    token_store_with(choice, auth::FileStore::from_env(), keychain_store)
}

/// The keychain store, when this platform has one. Called only when chosen.
fn keychain_store() -> Option<Arc<dyn TokenStore>> {
    #[cfg(target_os = "macos")]
    {
        Some(Arc::new(auth::KeychainStore::new("opentidal", "tidal")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("opentidal: keychain tokens are only available on macOS. Using the token file.");
        None
    }
}

/// Both stores are passed in so a test can prove the default writes a file
/// without ever opening the real keychain.
fn token_store_with(
    choice: &str,
    file: Option<auth::FileStore>,
    keychain: fn() -> Option<Arc<dyn TokenStore>>,
) -> Arc<dyn TokenStore> {
    if wants_keychain(choice) && let Some(store) = keychain() {
        return store;
    }
    match file {
        Some(store) => Arc::new(store),
        None => {
            eprintln!("opentidal: no token file path is available. Tokens stay in memory.");
            Arc::new(auth::MemoryStore::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{Catalogue, TrackId};

    /// A seed track. Ids compare by text, so this equals the id the app's own
    /// catalogue mints.
    fn track(n: usize) -> TrackId {
        Catalogue::seed().track_id(&format!("seed-track-{n}")).unwrap()
    }

    #[test]
    fn the_default_token_store_is_the_file() {
        // Check the choice before opening a store, so a wrong default cannot
        // reach the real keychain from a test.
        assert!(!wants_keychain(""));
        assert!(!wants_keychain("file"));
        assert!(!wants_keychain("unknown"));
        assert!(wants_keychain("keychain"));
        assert!(wants_keychain(" keychain "));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.toml");
        let store = token_store_with("", Some(auth::FileStore::new(path.clone())), || None);
        assert!(!store.is_memory());
        store.save(&auth::Token {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_in: 1,
        });
        let token = auth::FileStore::new(path).load().unwrap();
        assert_eq!(token.access_token, "a");
    }

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
            revalidate_on_entry: false,
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
        first.core.lib.add_track(id, track(3));
        first.flush_library();

        // A second run of the app, built through the same fixture so this test
        // cannot reach the real keychain when live mode is on.
        let mut second = app_without_a_session();
        second.open_library("173210380", &path);
        assert_eq!(second.core.lib.name, "Ada");
        assert_eq!(second.core.lib.playlist(id).unwrap().name, "Road trip");
        assert_eq!(second.core.lib.playlist(id).unwrap().tracks, vec![track(3)]);
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
        app.core.lib.toggle_like(track(1));
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
        assert_eq!(
            app.cover_root_override.as_deref(),
            Some(dir.join("covers").as_path()),
            "the scene's covers would land in the user's cache"
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
                after_refresh::<User>(Err(ServiceError::Unauthorized)),
                Err(ServiceError::InvalidGrant(_))
            ),
            "a rejected fresh token was left retryable"
        );
        // Every other answer is passed through: a network error must stay a network
        // error, and a good answer must stay good.
        assert!(matches!(
            after_refresh::<User>(Err(ServiceError::Http("offline".to_string()))),
            Err(ServiceError::Http(_))
        ));
        assert!(matches!(
            after_refresh::<User>(Err(ServiceError::InvalidGrant("revoked".to_string()))),
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
            fn page(
                &self,
                _token: &str,
                _user: &str,
                _of: &crate::library::RemoteList,
                _after: Option<&tidal::Cursor>,
            ) -> Result<tidal::Page, ServiceError> {
                Err(ServiceError::Unauthorized)
            }
            fn cover(&self, _token: &str, _cover: &crate::catalogue::CoverRequest) -> Result<Vec<u8>, ServiceError> {
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
        let result = with_fresh_token(&refresh, expired, &|access| service.current_user(access));

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
        let result = with_fresh_token(&refresh, current, &|access| service.current_user(access));
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
            reopened.load("173210380", &mut Catalogue::seed()).unwrap().is_some(),
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
        crate::state::open_detail(&mut app.core, &app.catalogue, crate::state::Route::UserPlaylist(id));
        assert_eq!(app.core.stack.last(), Some(&crate::state::Route::UserPlaylist(id)));
        assert!(crate::state::invariant(&app.core, &app.catalogue), "the fixture is already invalid");

        app.forget_session();
        assert!(
            crate::state::invariant(&app.core, &app.catalogue),
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

        app.core.lib.toggle_like(track(3));
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
            reopened.load("999", &mut Catalogue::seed()).unwrap().is_some(),
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
        seeded.core.lib.toggle_like(track(0));
        seeded.core.lib.toggle_like(track(1));
        seeded.flush_library();
        drop(seeded);

        let conn = rusqlite::Connection::open(&path).unwrap();
        // Foreign keys off, because the damage stands for a catalogue row that a
        // later build no longer resolves, which no write can produce.
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             UPDATE \"like\" SET track_id = '999' WHERE track_id = 'seed-track-1' AND user_id = '173210380';",
        )
        .unwrap();
        drop(conn);

        let mut app = app_without_a_session();
        app.open_library("173210380", &path);
        assert!(app.library.is_some(), "one unknown item closed the database");
        assert_eq!(app.core.lib.name, "Ada");
        assert_eq!(app.core.lib.liked.len(), 1, "the good like was lost");

        app.core.lib.toggle_like(track(2));
        app.flush_library();
        let mut reread = app_without_a_session();
        reread.open_library("173210380", &path);
        assert_eq!(reread.core.lib.liked.len(), 2, "the new like was not saved");

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
        first.core.lib.toggle_like(track(0));
        first.flush_library();
        first.sign_out();
        first.open_library("222", &path);

        assert!(first.core.lib.liked.is_empty(), "account B inherited A's likes");
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
                .load("173210380", &mut Catalogue::seed())
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
        app.core.lib.toggle_like(track(5));
        app.quit();

        let mut reread = app_without_a_session();
        reread.open_library("173210380", &path);
        assert_eq!(reread.core.lib.name, "Ada");
        assert!(reread.core.lib.is_liked(&track(5)));

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

        app.core.lib.toggle_like(track(3));
        app.wake_if_dirty();
        assert_eq!(count.load(Ordering::SeqCst), 1, "a change did not wake the loop");

    }

    /// Drain until `done` holds, or give up after a second. The workers run on
    /// other threads, so a result lands some time after the request.
    fn drain_until(app: &mut App, done: impl Fn(&App) -> bool) {
        for _ in 0..1_000 {
            app.drain();
            if done(app) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    type Pages = Vec<(RemoteList, Vec<Result<tidal::Page, ServiceError>>)>;

    /// One page of seed ids with no catalogue rows. The fake sets `next`.
    fn page(members: &[&str]) -> Result<tidal::Page, ServiceError> {
        Ok(tidal::Page {
            rows: crate::catalogue::CatalogueRows::default(),
            members: members.iter().map(|m| m.to_string()).collect(),
            next: None,
        })
    }

    /// One liked track and one private playlist, for the account the name says.
    fn tidal_pages(name: &str) -> Pages {
        use crate::catalogue::{CatalogueRows, PlaylistRow};
        let private = format!("{name}-private");
        let rows = CatalogueRows {
            playlists: vec![PlaylistRow {
                id: private.clone(),
                name: format!("{name} secret"),
                description: String::new(),
                tracks: vec!["seed-track-0".into()],
                cover: String::new(),
            }],
            ..CatalogueRows::default()
        };
        vec![
            (RemoteList::Liked, vec![page(&["seed-track-0"])]),
            (RemoteList::Playlists, vec![Ok(tidal::Page { rows, members: vec![private], next: None })]),
        ]
    }

    /// A fake service that records every page call's token, counts the profile
    /// fetches, rejects the page tokens `rejects` names, and can hold each page
    /// until a test releases it.
    struct Recorded {
        inner: tidal::FakeService,
        tokens: std::sync::Mutex<Vec<String>>,
        users: std::sync::atomic::AtomicUsize,
        rejects: fn(&str) -> bool,
        hold: Option<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
    }

    impl Recorded {
        fn new(pages: Pages) -> Recorded {
            Recorded {
                inner: tidal::FakeService { pages: pages.into_iter().collect(), ..tidal::FakeService::default() },
                tokens: std::sync::Mutex::new(Vec::new()),
                users: std::sync::atomic::AtomicUsize::new(0),
                rejects: |_| false,
                hold: None,
            }
        }

        /// Hold every page until the returned sender releases it.
        fn held(pages: Pages) -> (Recorded, std::sync::mpsc::Sender<()>) {
            let (release, wait) = std::sync::mpsc::channel();
            (Recorded { hold: Some(std::sync::Mutex::new(wait)), ..Recorded::new(pages) }, release)
        }

        fn tokens(&self) -> Vec<String> {
            self.tokens.lock().unwrap().clone()
        }

        fn users(&self) -> usize {
            self.users.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl MusicService for Recorded {
        fn current_user(&self, token: &str) -> Result<User, ServiceError> {
            self.users.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.current_user(token)
        }
        fn page(
            &self,
            token: &str,
            user: &str,
            of: &RemoteList,
            after: Option<&tidal::Cursor>,
        ) -> Result<tidal::Page, ServiceError> {
            if let Some(hold) = &self.hold {
                let _ = hold.lock().unwrap().recv();
            }
            self.tokens.lock().unwrap().push(token.to_string());
            if (self.rejects)(token) {
                return Err(ServiceError::Unauthorized);
            }
            self.inner.page(token, user, of, after)
        }
        fn cover(&self, token: &str, cover: &crate::catalogue::CoverRequest) -> Result<Vec<u8>, ServiceError> {
            self.inner.cover(token, cover)
        }
    }

    /// An app signed in to `user` on the file at `path`, reading through `service`.
    fn signed_in_with(user: &str, path: &std::path::Path, service: Arc<dyn MusicService>) -> App {
        let mut app = app_without_a_session();
        app.service = service;
        app.open_library(user, path);
        app.auth = Auth::SignedIn { token: fake_token() };
        app
    }

    /// An app signed in to `user` on the file at `path`, whose fake service
    /// answers `pages`.
    fn signed_in(user: &str, path: &std::path::Path, pages: Pages) -> App {
        signed_in_with(user, path, Arc::new(Recorded::new(pages)))
    }

    const MIRRORED: [RemoteList; 4] = [RemoteList::Liked, RemoteList::Albums, RemoteList::Artists, RemoteList::Playlists];

    /// No page is in flight and no mirrored list is still reading.
    fn settled(app: &App) -> bool {
        !app.pages.is_pending() && MIRRORED.iter().all(|list| app.pass_status(list.clone()) != PassStatus::Loading)
    }

    /// The library a fresh run would load for `user` from `path`.
    fn on_disk(user: &str, path: &std::path::Path) -> Library {
        Store::open(path).unwrap().load(user, &mut Catalogue::seed()).unwrap().unwrap().lib
    }

    fn tracks(ns: &[usize]) -> Vec<TrackId> {
        ns.iter().map(|&n| track(n)).collect()
    }

    /// Account A's private playlist names never reach account B. The catalogue
    /// keeps every playlist any account synced, in memory and on disk, so the
    /// Home shelf and the back stack are the two places a name could leak.
    #[test]
    fn account_b_never_sees_account_a_playlist_names() {
        use crate::state::Route;
        let dir = TempDir::new("cross-account-names");
        let path = dir.path();

        let mut app = signed_in("account-a", &path, tidal_pages("A"));
        app.sync();
        drain_until(&mut app, settled);
        let private = app.catalogue.playlist_id("A-private").expect("the fixture did not sync");
        crate::state::open_detail(&mut app.core, &app.catalogue, Route::CataloguePlaylist(private.clone()));
        assert_eq!(app.core.stack.last(), Some(&Route::CataloguePlaylist(private.clone())));
        app.sign_out();

        assert!(
            !app.core.stack.contains(&Route::CataloguePlaylist(private.clone())),
            "account A's playlist screen stayed one Back away after sign-out"
        );
        app.open_library("account-b", &path);
        let home: Vec<&str> = crate::screens::home_playlists(&app.catalogue).map(|p| p.name.as_str()).collect();
        assert!(!home.contains(&"A secret"), "account B's Home shows account A's playlist: {home:?}");
        assert!(app.core.lib.tidal_playlists.is_empty(), "account B holds account A's playlist");

        // A later run for account B reads every stored catalogue row, A's too.
        let mut next_run = app_without_a_session();
        next_run.open_library("account-b", &path);
        assert!(next_run.catalogue.playlist(&private).is_some(), "the fixture does not exercise the leak");
        let home: Vec<&str> =
            crate::screens::home_playlists(&next_run.catalogue).map(|p| p.name.as_str()).collect();
        assert!(!home.contains(&"A secret"), "a restart shows account A's playlist on B's Home: {home:?}");
        assert_eq!(home.len(), 3, "the seed playlists are gone from Home");
    }

    /// A page read for one account and landing after another opened in the
    /// same session is dropped. A dead refresh token closes the library without
    /// a new session, so the session alone cannot tell the two apart.
    #[test]
    fn a_page_for_another_account_is_dropped() {
        let dir = TempDir::new("page-other-account");
        let path = dir.path();
        let (service, release) = Recorded::held(tidal_pages("A"));
        let mut app = signed_in_with("account-a", &path, Arc::new(service));
        let session = app.session;
        app.sync();
        app.drain();
        assert!(app.pages.is_pending(), "the fixture started no page");
        app.open_library("account-b", &path);
        assert_eq!(app.session, session, "the fixture bumped the session, so it tests the wrong guard");
        release.send(()).unwrap();
        drain_until(&mut app, |a| !a.pages.is_pending());
        app.drain();

        assert!(app.core.lib.liked.is_empty(), "account A's page landed in account B's library");
        assert!(on_disk("account-b", &path).liked.is_empty(), "account A's page reached account B's rows");
    }

    /// Two accounts follow one Tidal playlist. Account B never sees the tracks
    /// account A merged into it, in memory or on disk, and sees its own once its
    /// page lands. The seed playlists keep their tracks across the switch.
    #[test]
    fn account_b_never_sees_account_a_playlist_tracks() {
        use crate::catalogue::{CatalogueRows, PlaylistRow};
        let dir = TempDir::new("cross-account-tracks");
        let path = dir.path();
        let shared = |tracks: &[&str]| CatalogueRows {
            playlists: vec![PlaylistRow {
                id: "shared".into(),
                name: "Shared".into(),
                description: String::new(),
                tracks: tracks.iter().map(|t| t.to_string()).collect(),
                cover: String::new(),
            }],
            ..CatalogueRows::default()
        };
        let mut app = signed_in("account-b", &path, Vec::new());
        app.catalogue.absorb(shared(&[]));
        let id = app.catalogue.playlist_id("shared").unwrap();
        app.core.lib.tidal_playlists.insert(id.clone());
        assert!(app.flush_library());

        app.open_library("account-a", &path);
        app.catalogue.absorb(shared(&["seed-track-1", "seed-track-2"]));
        app.core.lib.tidal_playlists.insert(id.clone());
        assert!(app.flush_library());
        assert_eq!(app.catalogue.playlist(&id).unwrap().tracks.to_vec(), tracks(&[1, 2]), "account A merged nothing");
        let seeds = |cat: &Catalogue| -> Vec<Vec<TrackId>> { cat.seed_playlists().map(|p| p.tracks.to_vec()).collect() };
        let seeded = seeds(&Catalogue::seed());

        let list = RemoteList::Playlist(id.clone());
        let (service, release) = Recorded::held(vec![(list.clone(), vec![page(&["seed-track-3"])])]);
        app.service = Arc::new(service);
        app.revalidate_on_entry = true;
        app.open_library("account-b", &path);
        crate::state::open_detail(&mut app.core, &app.catalogue, crate::state::Route::CataloguePlaylist(id.clone()));
        app.drain();
        assert!(app.pages.is_pending(), "the fixture started no page for account B");
        assert!(app.catalogue.playlist(&id).unwrap().tracks.is_empty(), "account B sees account A's tracks");
        assert_eq!(seeds(&app.catalogue), seeded, "the switch emptied a seed playlist");
        let mut stored = Catalogue::seed();
        Store::open(&path).unwrap().load("account-b", &mut stored).unwrap();
        assert!(stored.playlist(&id).unwrap().tracks.is_empty(), "account A's tracks stayed on disk");

        release.send(()).unwrap();
        drain_until(&mut app, |a| !a.pages.is_pending() && a.pass_status(list.clone()) == PassStatus::Complete);
        assert_eq!(app.catalogue.playlist(&id).unwrap().tracks.to_vec(), tracks(&[3]), "account B's page did not land alone");
        assert_eq!(seeds(&app.catalogue), seeded, "account B's page emptied a seed playlist");
    }

    /// No page starts while the profile fetch is pending. That fetch may
    /// refresh the token, and two refreshes of one refresh token race.
    #[test]
    fn a_page_waits_for_the_profile_fetch() {
        let dir = TempDir::new("page-waits");
        let path = dir.path();
        let service = Arc::new(Recorded::new(Vec::new()));
        let mut app = signed_in_with("account-a", &path, service.clone());

        let (release, wait) = std::sync::mpsc::channel::<()>();
        let session = app.session;
        app.profile.request((), move |_| {
            let _ = wait.recv();
            (session, Err(ServiceError::Http("offline".to_string())))
        });
        app.sync();
        for _ in 0..50 {
            app.drain();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!app.pages.is_pending(), "a page started while the profile fetch was pending");
        assert!(service.tokens().is_empty(), "a page was read while the profile fetch was pending");

        release.send(()).unwrap();
        drain_until(&mut app, |a| !a.profile.is_pending() && settled(a));
        assert_eq!(service.tokens().len(), 4, "the passes did not read once the profile landed");
    }

    /// A later page that fails keeps every page before it, in memory and on
    /// disk. A pass applies each page as it lands, so a halt loses nothing read.
    #[test]
    fn a_failed_page_keeps_the_earlier_pages() {
        let dir = TempDir::new("page-fails-late");
        let path = dir.path();
        let limited = Err(ServiceError::RateLimited("429 Too Many Requests".to_string()));
        let pages = vec![(RemoteList::Liked, vec![page(&["seed-track-1", "seed-track-2"]), page(&["seed-track-3"]), limited])];
        let mut app = signed_in("account-a", &path, pages);

        app.sync();
        drain_until(&mut app, settled);

        assert!(matches!(app.pass_status(RemoteList::Liked), PassStatus::Stalled(_)), "the fixture did not halt");
        assert_eq!(app.core.lib.liked.to_vec(), tracks(&[1, 2, 3]), "the pages before the failure were lost");
        assert_eq!(on_disk("account-a", &path).liked.to_vec(), tracks(&[1, 2, 3]), "the earlier pages are not on disk");
    }

    /// A pass that halts removes nothing the cache holds, whether it halts on
    /// its first page or a later one. Only the last page may remove a row.
    #[test]
    fn a_halted_pass_removes_nothing() {
        let dir = TempDir::new("page-halts");
        let path = dir.path();
        let limited = || Err(ServiceError::RateLimited("429 Too Many Requests".to_string()));
        let pages = vec![
            (RemoteList::Liked, vec![page(&["seed-track-1"]), limited()]),
            (RemoteList::Albums, vec![limited()]),
        ];
        let mut app = signed_in("account-a", &path, pages);
        app.core.lib.toggle_like(track(5));
        app.core.lib.toggle_like(track(4));
        let album = app.catalogue.album_id("seed-album-1").unwrap();
        app.core.lib.toggle_save_album(album.clone());
        app.flush_library();

        app.sync();
        drain_until(&mut app, settled);

        assert_eq!(app.core.lib.liked.to_vec(), tracks(&[1, 4, 5]), "a halted pass removed a cached like");
        assert_eq!(app.core.lib.saved_albums.to_vec(), [album], "a pass that failed on page one removed a cached album");
        assert_eq!(on_disk("account-a", &path), app.core.lib, "the disk disagrees with the library");
    }

    /// A pass removes a cached row Tidal no longer lists only when its last
    /// page lands. A first page alone keeps the row, because it may sit on a
    /// later page.
    #[test]
    fn only_a_complete_pass_removes_a_row() {
        let dir = TempDir::new("page-complete");
        let path = dir.path();
        let pages = vec![(RemoteList::Liked, vec![page(&["seed-track-1"]), page(&["seed-track-4"])])];
        let mut app = signed_in("account-a", &path, pages);
        app.revalidate_on_entry = true;
        app.core.lib.toggle_like(track(5));
        app.core.lib.toggle_like(track(4));
        app.flush_library();

        crate::state::open_section(&mut app.core, crate::state::Route::LikedSongs);
        drain_until(&mut app, |a| settled(a) && a.core.lib.is_liked(&track(1)));
        assert_eq!(app.core.lib.liked.to_vec(), tracks(&[1, 4, 5]), "a first page removed a row");

        app.sync();
        drain_until(&mut app, |a| settled(a) && a.pass_status(RemoteList::Liked) == PassStatus::Complete);
        assert_eq!(app.core.lib.liked.to_vec(), tracks(&[1, 4]), "a complete pass kept a row Tidal does not list");
        assert_eq!(on_disk("account-a", &path).liked.to_vec(), tracks(&[1, 4]));
    }

    /// The same pages read twice leave the library, the catalogue and the
    /// file as the first read left them.
    #[test]
    fn a_page_applied_twice_changes_nothing() {
        let dir = TempDir::new("page-twice");
        let path = dir.path();
        let mut pages = tidal_pages("A");
        pages[0].1 = vec![page(&["seed-track-1", "seed-track-2"]), page(&["seed-track-3"])];
        let mut app = signed_in("account-a", &path, pages);
        app.core.lib.toggle_like(track(6));

        app.sync();
        drain_until(&mut app, settled);
        let (once, revision) = (app.core.lib.clone(), app.catalogue.revision());
        assert_eq!(once.liked.to_vec(), tracks(&[1, 2, 3]), "the fixture did not land");

        app.sync();
        drain_until(&mut app, |a| settled(a) && MIRRORED.iter().all(|l| a.pass_status(l.clone()) == PassStatus::Complete));
        assert_eq!(app.core.lib, once, "reading the same pages again changed the library");
        assert_eq!(app.catalogue.revision(), revision, "reading the same pages again changed the catalogue");
        assert_eq!(on_disk("account-a", &path), once);
    }

    /// A page Tidal rejects forces one renewal: one refresh, one profile fetch,
    /// and the pass resumes with the fresh token. No page reads with the
    /// rejected token again.
    #[test]
    fn an_unauthorized_page_forces_one_refresh() {
        let dir = TempDir::new("page-renews");
        let path = dir.path();
        let service = Arc::new(Recorded { rejects: |token| token == "fake-access", ..Recorded::new(tidal_pages("A")) });
        let mut app = signed_in_with("account-a", &path, service.clone());
        app.refresh = |_, token| Ok(Token { access_token: "fresh".into(), refresh_token: token.refresh_token.clone(), expires_in: 3600 });

        app.sync();
        drain_until(&mut app, |a| !a.profile.is_pending() && settled(a) && a.core.lib.is_liked(&track(0)));

        assert_eq!(service.users(), 1, "the rejected page did not renew exactly once");
        assert_eq!(service.tokens(), ["fake-access", "fresh", "fresh", "fresh", "fresh"]);
        assert_eq!(app.tokens.load().map(|t| t.access_token), Some("fresh".to_string()), "the fresh token was not kept");
        assert!(matches!(app.auth, Auth::SignedIn { .. }));
    }

    /// A renewal that fails without ending the grant stalls the pass that
    /// waited on it, so the list stops saying it syncs. A later scroll retries
    /// it, and a second rejection renews again instead of ending the session.
    #[test]
    fn a_failed_renewal_stalls_the_pass_until_a_scroll_retries() {
        let dir = TempDir::new("page-renewal-fails");
        let path = dir.path();
        let pages = vec![(RemoteList::Liked, vec![page(&["seed-track-1"]), page(&["seed-track-2"])])];
        let service = Arc::new(Recorded { rejects: |token| token == "fake-access", ..Recorded::new(pages) });
        let mut app = signed_in_with("account-a", &path, service.clone());
        app.revalidate_on_entry = true;
        app.refresh = |_, _| Err(ServiceError::Http("offline".into()));

        crate::state::open_section(&mut app.core, crate::state::Route::LikedSongs);
        drain_until(&mut app, |a| !a.profile.is_pending() && settled(a));
        assert!(matches!(app.pass_status(RemoteList::Liked), PassStatus::Stalled(_)), "a failed renewal left the pass syncing");
        assert!(matches!(app.auth, Auth::SignedIn { .. }), "a renewal that could not reach Tidal ended the session");

        app.refresh = |_, token| Ok(Token { access_token: "fresh".into(), refresh_token: token.refresh_token.clone(), expires_in: 3600 });
        app.want(RemoteList::Liked, 1);
        drain_until(&mut app, |a| !a.profile.is_pending() && settled(a) && a.core.lib.is_liked(&track(1)));
        assert!(matches!(app.auth, Auth::SignedIn { .. }), "the retry after a failed renewal ended the session");
        assert!(app.core.lib.is_liked(&track(1)), "a scroll did not retry the stalled pass");
        assert_eq!(service.tokens(), ["fake-access", "fake-access", "fresh"]);
    }

    /// A renewed token that Tidal rejects too ends the session, the same rule
    /// the profile fetch keeps.
    #[test]
    fn a_renewed_token_rejected_again_ends_the_session() {
        let dir = TempDir::new("page-dead");
        let path = dir.path();
        let service = Arc::new(Recorded { rejects: |_| true, ..Recorded::new(tidal_pages("A")) });
        let mut app = signed_in_with("account-a", &path, service.clone());
        app.refresh = |_, token| Ok(Token { access_token: "fresh".into(), refresh_token: token.refresh_token.clone(), expires_in: 3600 });
        app.tokens.save(&fake_token());

        app.sync();
        drain_until(&mut app, |a| a.library.is_none());

        assert_eq!(service.tokens(), ["fake-access", "fresh"], "the pass did not resume once, with the fresh token");
        assert!(app.library.is_none(), "a rejected fresh token left the library writable");
        assert!(matches!(app.auth, Auth::Failed { .. }));
        assert_eq!(app.tokens.load(), None, "the dead token stayed stored");
    }

    /// A page that changes only the catalogue, such as a Tidal playlist's
    /// tracks, still reaches disk. The library is unchanged, so only the
    /// catalogue revision says a write is owed.
    #[test]
    fn a_catalogue_only_change_reaches_disk() {
        use crate::catalogue::{CatalogueRows, PlaylistRow};
        let dir = TempDir::new("page-catalogue");
        let path = dir.path();
        let mut app = signed_in("account-a", &path, Vec::new());
        app.catalogue.absorb(CatalogueRows {
            playlists: vec![PlaylistRow {
                id: "x-playlist".into(),
                name: "Evening".into(),
                description: String::new(),
                tracks: vec!["seed-track-0".into()],
                cover: String::new(),
            }],
            ..CatalogueRows::default()
        });
        let id = app.catalogue.playlist_id("x-playlist").unwrap();
        app.core.lib.tidal_playlists.insert(id.clone());
        app.flush_library();
        let list = RemoteList::Playlist(id.clone());
        app.service = Arc::new(Recorded::new(vec![(list.clone(), vec![page(&["seed-track-1", "seed-track-2"])])]));
        app.revalidate_on_entry = true;

        crate::state::open_detail(&mut app.core, &app.catalogue, crate::state::Route::CataloguePlaylist(id.clone()));
        drain_until(&mut app, |a| !a.pages.is_pending() && a.pass_status(list.clone()) == PassStatus::Complete);
        assert_eq!(app.core.lib, app.on_disk, "the fixture changed the library, so it tests the wrong dirty check");

        let mut next_run = app_without_a_session();
        next_run.open_library("account-a", &path);
        assert_eq!(next_run.catalogue.playlist(&id).unwrap().tracks.to_vec(), tracks(&[1, 2]), "the new tracks are not on disk");
    }

    /// Entering a list's route reads its first page only. A view that shows
    /// more rows than the pass read asks for the next page.
    #[test]
    fn a_larger_want_reads_the_next_page() {
        let dir = TempDir::new("page-want");
        let path = dir.path();
        let pages = vec![(RemoteList::Liked, vec![page(&["seed-track-1"]), page(&["seed-track-2"])])];
        let service = Arc::new(Recorded::new(pages));
        let mut app = signed_in_with("account-a", &path, service.clone());
        app.revalidate_on_entry = true;

        crate::state::open_section(&mut app.core, crate::state::Route::LikedSongs);
        drain_until(&mut app, |a| settled(a) && a.core.lib.is_liked(&track(1)));
        for _ in 0..20 {
            app.drain();
        }
        assert!(!app.pages.is_pending(), "entering the route asked for a second page");
        assert_eq!(service.tokens().len(), 1, "entering the route read past its first page");

        app.want(RemoteList::Liked, 2);
        drain_until(&mut app, |a| settled(a) && a.pass_status(RemoteList::Liked) == PassStatus::Complete);
        assert_eq!(app.core.lib.liked.to_vec(), tracks(&[1, 2]), "a larger want did not read the next page");
    }

    /// Entering a list's route starts one pass. Staying on it, or entering it
    /// again while it shows, starts none.
    #[test]
    fn entering_a_route_starts_one_pass_and_reentering_does_not() {
        use crate::state::Route;
        let dir = TempDir::new("page-enter");
        let path = dir.path();
        let service = Arc::new(Recorded::new(Vec::new()));
        let mut app = signed_in_with("account-a", &path, service.clone());
        app.revalidate_on_entry = true;

        crate::state::open_section(&mut app.core, Route::LikedSongs);
        drain_until(&mut app, |a| settled(a) && a.pass_status(RemoteList::Liked) == PassStatus::Complete);
        crate::state::open_section(&mut app.core, Route::LikedSongs);
        for _ in 0..20 {
            app.drain();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!app.pages.is_pending(), "entering the same route again asked for a page");
        assert_eq!(service.tokens().len(), 1, "entering the same route again read it again");

        crate::state::open_section(&mut app.core, Route::Albums);
        drain_until(&mut app, |a| settled(a) && a.pass_status(RemoteList::Albums) == PassStatus::Complete);
        assert_eq!(service.tokens().len(), 2, "entering another list's route did not read it once");
    }

    /// A sync that lands is on disk after the next frame, with the playlist name
    /// a restart can show.
    #[test]
    fn a_sync_lands_on_disk() {
        let dir = TempDir::new("sync-lands");
        let path = dir.path();
        let mut app = signed_in("account-a", &path, tidal_pages("A"));
        app.sync();
        drain_until(&mut app, settled);

        let mut next_run = app_without_a_session();
        next_run.open_library("account-a", &path);
        assert!(next_run.core.lib.is_liked(&track(0)), "the synced like is not on disk");
        let names: Vec<String> = next_run
            .core
            .lib
            .tidal_playlists
            .iter()
            .map(|id| next_run.catalogue.playlist(id).unwrap().name.clone())
            .collect();
        assert_eq!(names, ["A secret"], "the synced playlist is not on disk");
    }

    /// A page that lands after a sign-out and a new sign-in to the same account
    /// belongs to the old session and is dropped.
    #[test]
    fn a_sync_for_an_earlier_session_is_dropped() {
        let dir = TempDir::new("sync-earlier-session");
        let path = dir.path();
        let (service, release) = Recorded::held(tidal_pages("A"));
        let mut app = signed_in_with("account-a", &path, Arc::new(service));
        app.sync();
        app.drain();
        assert!(app.pages.is_pending(), "the fixture started no page");
        app.sign_out();
        app.open_library("account-a", &path);
        app.auth = Auth::SignedIn { token: fake_token() };
        release.send(()).unwrap();
        drain_until(&mut app, |a| !a.pages.is_pending());
        app.drain();

        assert!(app.core.lib.liked.is_empty(), "a page from the earlier session landed");
    }

    /// A page that lands after a dead refresh token closed the library is
    /// dropped, so it cannot fill a library that has nowhere to be written.
    #[test]
    fn a_sync_for_a_closed_library_is_dropped() {
        let dir = TempDir::new("sync-closed");
        let path = dir.path();
        let (service, release) = Recorded::held(tidal_pages("A"));
        let mut app = signed_in_with("account-a", &path, Arc::new(service));
        app.sync();
        app.drain();
        assert!(app.pages.is_pending(), "the fixture started no page");
        app.end_dead_session(ServiceError::InvalidGrant("revoked".to_string()));
        release.send(()).unwrap();
        drain_until(&mut app, |a| !a.pages.is_pending());
        app.drain();

        assert!(app.library.is_none());
        assert_eq!(app.core.lib, Library::default(), "a page filled a closed library");
    }

    /// An edit made while a page is in flight survives it. A page merges only
    /// the one list it names, never the whole library.
    ///
    /// The first version drained while it waited, so the edit was written before
    /// the result landed, and a sync that replaced the library with the disk copy
    /// still passed. The edit now stays unwritten until the page is applied.
    #[test]
    fn an_edit_during_a_sync_survives_it() {
        let dir = TempDir::new("sync-edit");
        let path = dir.path();
        let (service, release) = Recorded::held(tidal_pages("A"));
        let mut app = signed_in_with("account-a", &path, Arc::new(service));
        app.sync();
        app.drain();
        assert!(app.pages.is_pending(), "the fixture started no page");
        let mine = app.core.lib.create_playlist("Made while syncing".to_string()).unwrap();
        app.core.lib.toggle_download(Catalogue::seed().album_id("seed-album-3").unwrap());
        release.send(()).unwrap();
        for _ in 0..1_000 {
            if !app.pages.is_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_ne!(on_disk("account-a", &path), app.core.lib, "the edit was written before the page landed");
        app.drain();

        assert_eq!(app.core.lib.playlist(mine).map(|p| p.name.as_str()), Some("Made while syncing"));
        assert_eq!(app.core.lib.downloaded.len(), 1, "the page dropped a download made while it ran");
        assert!(app.core.lib.is_liked(&track(0)), "the page did not land");
        assert_eq!(on_disk("account-a", &path), app.core.lib);
    }

    /// A dead refresh token found by a page ends the session the same way the
    /// profile fetch does: no library, no token.
    #[test]
    fn a_dead_refresh_token_during_sync_closes_the_library() {
        let dir = TempDir::new("sync-dead-token");
        let path = dir.path();
        let dead = Err(ServiceError::InvalidGrant("refresh token revoked".to_string()));
        let mut app = signed_in("account-a", &path, vec![(RemoteList::Liked, vec![dead])]);
        app.tokens.save(&fake_token());
        app.core.lib.toggle_like(track(1));

        app.sync();
        drain_until(&mut app, |a| a.library.is_none());

        assert!(app.library.is_none(), "a dead token left the library writable");
        assert_eq!(app.core.lib, Library::default(), "the library stayed in memory");
        assert_eq!(app.tokens.load(), None, "the dead token stayed stored");
        assert!(matches!(app.auth, Auth::Failed { .. }));
        assert!(on_disk("account-a", &path).is_liked(&track(1)), "the last edit was not written first");
    }

    /// A landed profile fetch starts a sync in live mode, with the token the
    /// fetch proved, and never in the fake build. A fake scene or test that
    /// fetches the profile must not have its library replaced by surprise.
    #[test]
    fn a_landed_profile_syncs_only_in_live_mode() {
        for live in [false, true] {
            let dir = TempDir::new(if live { "trigger-live" } else { "trigger-fake" });
            let path = dir.path();
            let mut app = signed_in("account-a", &path, tidal_pages("A"));
            app.live = live;
            let session = app.session;
            let user = User { id: crate::tidal::UserId("account-a".to_string()), name: "Ada".to_string() };
            app.profile.request((), move |_| (session, Ok((fake_token(), user))));
            drain_until(&mut app, |a| !a.profile.is_pending() && a.core.lib.name == "Ada");
            if live {
                drain_until(&mut app, |a| settled(a) && a.core.lib.is_liked(&track(0)));
            } else {
                // Long enough for a sync the fake build must not start to land.
                for _ in 0..50 {
                    app.drain();
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }

            assert_eq!(app.core.lib.is_liked(&track(0)), live, "live mode is {live}, and the sync disagreed");
            assert_eq!(app.pass_status(RemoteList::Liked) == PassStatus::Complete, live);
        }
    }

    /// An app signed in to `user` on the file at `path`, with covers under `root`.
    /// Its catalogue holds `n` albums with covers `art-0` and up, and its fake
    /// service answers each with the magenta PNG.
    fn covered_app(user: &str, path: &std::path::Path, root: &std::path::Path, n: usize) -> (App, Arc<tidal::FakeService>) {
        use crate::catalogue::{AlbumRow, CatalogueRows};
        let mut app = app_without_a_session();
        app.setup(Arc::new(|| ()));
        app.cover_root_override = Some(root.to_path_buf());
        let service = Arc::new(tidal::FakeService {
            covers: (0..n).map(|i| (format!("art-{i}"), Ok(crate::covers::tests::magenta()))).collect(),
            ..tidal::FakeService::default()
        });
        app.service = service.clone();
        app.catalogue.absorb(CatalogueRows {
            albums: (0..n)
                .map(|i| AlbumRow {
                    id: format!("album-{i}"),
                    title: format!("Album {i}"),
                    artist: "seed-artist-0".into(),
                    cover: format!("art-{i}"),
                })
                .collect(),
            ..CatalogueRows::default()
        });
        app.open_library(user, path);
        app.auth = Auth::SignedIn { token: fake_token() };
        (app, service)
    }

    /// Draw every album cover the catalogue names, the way a screen does.
    fn want_covers(app: &App) {
        for album in app.catalogue.albums().filter(|a| a.cover.is_some()) {
            app.covers().album(album);
        }
    }

    fn shown(app: &App) -> usize {
        app.library
            .as_ref()
            .and_then(|open| open.covers.as_ref())
            .map_or(0, |c| c.states.borrow().values().filter(|s| matches!(s, CoverState::Shown(_))).count())
    }

    fn cover_calls(service: &tidal::FakeService) -> usize {
        service.cover_calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// More covers than one batch fetches all arrive. A cover past the fetch
    /// cap waits for the next batch instead of being dropped.
    #[test]
    fn every_wanted_cover_is_fetched_across_batches() {
        let dir = TempDir::new("covers-batches");
        let wanted = crate::covers::MAX_FETCHES + 6;
        let (mut app, service) = covered_app("173210380", &dir.path(), &dir.0.join("covers"), wanted);
        want_covers(&app);
        drain_until(&mut app, |a| shown(a) == wanted);

        assert_eq!(shown(&app), wanted, "a wanted cover never arrived");
        assert_eq!(cover_calls(&service), wanted, "a cover was fetched twice or not at all");
    }

    /// A rate-limited cover goes back to wanted, so the next batch asks for it
    /// again. An unavailable cover would wait for the next page instead.
    #[test]
    fn a_rate_limited_cover_is_asked_for_again() {
        let dir = TempDir::new("covers-limited");
        let (mut app, _) = covered_app("173210380", &dir.path(), &dir.0.join("covers"), 1);
        let service = Arc::new(tidal::FakeService {
            covers: BTreeMap::from([("art-0".to_string(), Err(ServiceError::RateLimited("429".to_string())))]),
            ..tidal::FakeService::default()
        });
        app.service = service.clone();
        want_covers(&app);
        drain_until(&mut app, |_| cover_calls(&service) >= 2);
        assert!(cover_calls(&service) >= 2, "a rate-limited cover was not asked for again");
    }

    /// A service whose cover call waits for a release, so a test can act while
    /// a batch is in flight.
    struct Gated {
        inner: Arc<tidal::FakeService>,
        gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl MusicService for Gated {
        fn current_user(&self, token: &str) -> Result<User, ServiceError> {
            self.inner.current_user(token)
        }
        fn page(
            &self,
            token: &str,
            user: &str,
            of: &crate::library::RemoteList,
            after: Option<&tidal::Cursor>,
        ) -> Result<tidal::Page, ServiceError> {
            self.inner.page(token, user, of, after)
        }
        fn cover(&self, token: &str, cover: &crate::catalogue::CoverRequest) -> Result<Vec<u8>, ServiceError> {
            let _ = self.gate.lock().unwrap().recv();
            self.inner.cover(token, cover)
        }
    }

    fn gate(app: &mut App, inner: Arc<tidal::FakeService>) -> std::sync::mpsc::Sender<()> {
        let (release, wait) = std::sync::mpsc::channel();
        app.service = Arc::new(Gated { inner, gate: std::sync::Mutex::new(wait) });
        release
    }

    /// A cover the view asks for while a batch runs is fetched by the next one.
    #[test]
    fn a_cover_wanted_during_a_batch_is_fetched_next() {
        let dir = TempDir::new("covers-during");
        let (mut app, service) = covered_app("173210380", &dir.path(), &dir.0.join("covers"), 2);
        let release = gate(&mut app, service.clone());
        let first = app.catalogue.album(&app.catalogue.album_id("album-0").unwrap()).unwrap().clone();
        let second = app.catalogue.album(&app.catalogue.album_id("album-1").unwrap()).unwrap().clone();
        app.covers().album(&first);
        app.drain();
        assert!(app.cover_fetch.is_pending(), "the fixture started no batch");
        app.covers().album(&second);
        app.drain();
        release.send(()).unwrap();
        release.send(()).unwrap();
        drain_until(&mut app, |a| shown(a) == 2);
        assert_eq!(shown(&app), 2, "the cover wanted during a batch never arrived");
    }

    /// A batch started for one account and landing after another opened is
    /// dropped. Account B shows none of account A's covers, and B's directory
    /// holds none of A's files.
    #[test]
    fn a_cover_batch_from_another_account_is_dropped() {
        let dir = TempDir::new("covers-other-account");
        let root = dir.0.join("covers");
        let (mut app, service) = covered_app("account-a", &dir.path(), &root, 1);
        let release = gate(&mut app, service.clone());
        want_covers(&app);
        app.drain();
        assert!(app.cover_fetch.is_pending(), "the fixture started no batch");
        let session = app.session;
        app.open_library("account-b", &dir.path());
        assert_eq!(app.session, session, "the fixture bumped the session, so it tests the wrong guard");
        release.send(()).unwrap();
        drain_until(&mut app, |a| !a.cover_fetch.is_pending());
        app.drain();

        assert_eq!(shown(&app), 0, "account A's cover landed in account B");
        let b_dir = root.join("account-b");
        assert!(std::fs::read_dir(&b_dir).map_or(true, |mut d| d.next().is_none()), "account B's cache holds a file");
    }

    /// Sign-out drops every shown cover with the library that owned it.
    #[test]
    fn sign_out_forgets_the_shown_covers() {
        let dir = TempDir::new("covers-sign-out");
        let (mut app, _) = covered_app("173210380", &dir.path(), &dir.0.join("covers"), 1);
        want_covers(&app);
        drain_until(&mut app, |a| shown(a) == 1);
        assert_eq!(shown(&app), 1, "the fixture showed no cover");

        app.sign_out();
        let album = app.catalogue.album(&app.catalogue.album_id("album-0").unwrap()).unwrap().clone();
        assert!(app.covers().album(&album).ready().is_none(), "a cover outlived sign-out");
        app.open_library("173210380", &dir.path());
        assert_eq!(shown(&app), 0, "the next session inherited the shown covers");
    }

    /// A second run over the same cover root shows every cover with no fetch.
    #[test]
    fn a_cover_restart_fetches_nothing() {
        let dir = TempDir::new("covers-restart");
        let root = dir.0.join("covers");
        let (mut first, _) = covered_app("173210380", &dir.path(), &root, 3);
        want_covers(&first);
        drain_until(&mut first, |a| shown(a) == 3);
        assert_eq!(shown(&first), 3);
        drop(first);

        let (mut second, service) = covered_app("173210380", &dir.path(), &root, 3);
        want_covers(&second);
        drain_until(&mut second, |a| shown(a) == 3);
        assert_eq!(shown(&second), 3, "a restart lost a cover");
        assert_eq!(cover_calls(&service), 0, "a restart fetched a cover it had on disk");
    }

    /// A cover batch does not start while the profile fetch is pending, the
    /// same guard a page has.
    #[test]
    fn a_cover_batch_waits_for_the_profile_fetch() {
        let dir = TempDir::new("covers-wait-profile");
        let (mut app, _) = covered_app("173210380", &dir.path(), &dir.0.join("covers"), 1);
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let session = app.session;
        app.profile.request((), move |_| {
            let _ = wait.recv();
            (session, Err(ServiceError::Http("offline".to_string())))
        });
        want_covers(&app);
        app.drain();
        assert!(!app.cover_fetch.is_pending(), "a cover batch started while the profile fetch was pending");

        release.send(()).unwrap();
        drain_until(&mut app, |a| shown(a) == 1);
        assert_eq!(shown(&app), 1, "the cover batch did not start once the profile landed");
    }

    /// Restoring a session in `setup` does no cover work: no fetch, no batch,
    /// and no cover directory, until a screen draws a cover.
    #[test]
    fn no_cover_work_runs_at_setup() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let dir = TempDir::new("covers-setup");
        let root = dir.0.join("covers");
        let service = Arc::new(tidal::FakeService::default());
        let mut app = app_without_a_session();
        app.service = service.clone();
        app.db_override = Some(dir.path());
        app.cover_root_override = Some(root.clone());
        let payload = URL_SAFE_NO_PAD.encode(br#"{"uid":173210380,"exp":4102444800}"#);
        app.tokens.save(&Token { access_token: format!("h.{payload}.sig"), refresh_token: "r".into(), expires_in: 0 });

        app.setup(Arc::new(|| ()));
        assert!(app.library.is_some(), "the fixture restored no session");
        app.drain();

        assert!(!app.cover_fetch.is_pending(), "setup started a cover batch");
        assert_eq!(cover_calls(&service), 0);
        assert!(!root.exists(), "setup touched the cover root");
    }

    /// The bytes a fetch puts decode through Rune to the fixture's pixels. The
    /// app and the cover scene show covers through this same call.
    #[test]
    fn a_ready_cover_decodes_to_its_pixels() {
        let root = tempfile::tempdir().unwrap();
        let cache = CoverCache::for_account(root.path(), "173210380").unwrap();
        let path = cache.put(&crate::covers::tests::artwork("art-0"), &crate::covers::tests::magenta()).unwrap();
        let image = ImageCache::new(Arc::new(|| ())).file(&path, COVER_PT, COVER_SCALE);
        assert_eq!(image.wait(), Ok([16, 16]));
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
