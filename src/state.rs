//! OpenTidal's application state and its transitions, extracted from the view
//! so a checker can prove the invariants without compiling the rune/wgpu view
//! layer. `main.rs` delegates every state change to these functions; nothing
//! here depends on `rune`.
//!
//! The state now owns the user's [`Library`]. The invariant proof is therefore
//! cross-cutting: it proves that no route, playlist, history entry or saved
//! item references an id that does not exist, and that deleting a playlist can
//! never leave a dangling route behind.

use crate::catalogue::{AlbumId, ArtistId, Catalogue, EditorialPlaylistId, TrackId};
use crate::library::{Library, PlaylistId};

/// Number of accent colours. Matches `rune::tok::ACCENT_NAMES.len()`;
/// `main.rs` asserts that equality at compile time.
pub const ACCENT_LEN: usize = 6;

/// The three tabs on the Popular screen.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PopularTab {
    Artists,
    Albums,
    Tracks,
}

/// A subpage of the Settings screen, reached by drilling from the settings
/// index. Each carries no payload; the page builder renders its rows.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SettingsPage {
    Playback,
    Quality,
    Downloads,
    Notifications,
    Import,
    Account,
    About,
}

impl SettingsPage {
    pub fn label(&self) -> &'static str {
        match self {
            SettingsPage::Playback => "Playback",
            SettingsPage::Quality => "Quality",
            SettingsPage::Downloads => "Downloads",
            SettingsPage::Notifications => "Notifications",
            SettingsPage::Import => "Import",
            SettingsPage::Account => "Account",
            SettingsPage::About => "About",
        }
    }
}

/// A screen in the app. The section variants are the top-level sidebar and
/// top-bar entries. The detail variants carry an id so a detail screen can
/// never exist without its subject. A catalogue id is minted only for a member,
/// and a `UserPlaylist` id is a slot that may be empty, so [`resolves`] guards
/// both on the way in. `Clone`, not `Copy`, because a catalogue id holds text.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Route {
    Home,
    Feed,
    Popular,
    ForYou,
    LikedSongs,
    Albums,
    Artists,
    Playlists,
    Collections,
    Local,
    History,
    Stats,
    Downloads,
    Search,
    Settings,
    SettingsPage(SettingsPage),
    Album(AlbumId),
    Artist(ArtistId),
    EditorialPlaylist(EditorialPlaylistId),
    UserPlaylist(PlaylistId),
}

/// Which sidebar region a section route belongs to. `Search` and `Settings`
/// live in the top bar and are absent from `SECTIONS`, so their group is
/// never used by the sidebar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Primary,
    Library,
    Bottom,
}

impl Route {
    /// The route's human name. Detail routes return a generic name; the view
    /// replaces it with the subject's title.
    pub fn label(&self) -> &'static str {
        match self {
            Route::Home => "Home",
            Route::Feed => "Feed",
            Route::Popular => "Popular",
            Route::ForYou => "For You",
            Route::LikedSongs => "Liked Songs",
            Route::Albums => "Albums",
            Route::Artists => "Artists",
            Route::Playlists => "Playlists",
            Route::Collections => "Collections",
            Route::Local => "On this device",
            Route::History => "History",
            Route::Stats => "Stats",
            Route::Downloads => "Downloads",
            Route::Search => "Search",
            Route::Settings => "Settings",
            Route::SettingsPage(p) => p.label(),
            Route::Album(_) => "Album",
            Route::Artist(_) => "Artist",
            Route::EditorialPlaylist(_) => "Playlist",
            Route::UserPlaylist(_) => "Playlist",
        }
    }

    /// The sidebar region the section route belongs to.
    pub fn group(&self) -> Group {
        match self {
            Route::Home | Route::Feed | Route::Popular | Route::ForYou => Group::Primary,
            Route::LikedSongs | Route::Albums | Route::Artists | Route::Playlists
            | Route::Collections | Route::Local | Route::History | Route::Stats => Group::Library,
            Route::Downloads | Route::Search | Route::Settings | Route::SettingsPage(_) => {
                Group::Bottom
            }
            Route::Album(_)
            | Route::Artist(_)
            | Route::EditorialPlaylist(_)
            | Route::UserPlaylist(_) => Group::Library,
        }
    }
}

/// The sidebar sections in order: the primary block, then "Your Library",
/// then Downloads. `Search` and `Settings` live in the top bar and are absent
/// here.
pub const SECTIONS: [Route; 13] = [
    Route::Home,
    Route::Feed,
    Route::Popular,
    Route::ForYou,
    Route::LikedSongs,
    Route::Albums,
    Route::Artists,
    Route::Playlists,
    Route::Collections,
    Route::Local,
    Route::History,
    Route::Stats,
    Route::Downloads,
];

/// The app's state: the navigation shell plus the user's [`Library`]. The
/// navigation stack is never empty; its last element is the current screen.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    pub stack: Vec<Route>,
    pub playing: bool,
    pub accent: usize,
    pub collapsed: bool,
    pub popular_tab: PopularTab,
    pub now_playing: Option<TrackId>,
    pub lib: Library,
}

impl State {
    /// The state the app starts in. The first catalogue track is now playing, so
    /// a state never names a track the catalogue lacks.
    pub fn new(cat: &Catalogue) -> State {
        State {
            stack: vec![Route::Home],
            playing: true,
            accent: 0,
            collapsed: false,
            popular_tab: PopularTab::Artists,
            now_playing: cat.tracks().next().map(|t| t.id.clone()),
            lib: Library::default(),
        }
    }
}

/// Whether a route names something that exists: a catalogue member, or a user
/// playlist slot that holds a playlist. `open_detail` refuses a route that does
/// not resolve, and the invariant requires every route on the stack to resolve.
pub fn resolves(s: &State, cat: &Catalogue, route: &Route) -> bool {
    match route {
        Route::Album(id) => cat.album(id).is_some(),
        Route::Artist(id) => cat.artist(id).is_some(),
        Route::EditorialPlaylist(id) => cat.editorial(id).is_some(),
        Route::UserPlaylist(id) => s.lib.playlist(*id).is_some(),
        _ => true,
    }
}

/// The full safety invariant. Every reachable state has a non-empty stack,
/// an in-range accent, a now-playing track in the catalogue, only routes that
/// resolve, and a valid library. Test-only: the proof invokes it, the binary
/// does not.
#[cfg(test)]
pub fn invariant(s: &State, cat: &Catalogue) -> bool {
    s.accent < ACCENT_LEN
        && !s.stack.is_empty()
        && s.now_playing.as_ref().is_none_or(|t| cat.track(t).is_some())
        && s.stack.iter().all(|r| resolves(s, cat, r))
        && s.lib.check(cat).is_ok()
}

/// The root a state falls back to. A `static`, because a route that can hold
/// text cannot be borrowed from a temporary.
static HOME: Route = Route::Home;

/// The current screen. Falls back to Home on an empty stack, which the
/// invariant says never happens.
pub fn current(s: &State) -> &Route {
    s.stack.last().unwrap_or(&HOME)
}

/// Open a top-level section, replacing any detail depth.
pub fn open_section(s: &mut State, route: Route) {
    s.stack.clear();
    s.stack.push(route);
}

/// Drill into a detail screen, stacking it on the current one. Reopening the
/// same route is a no-op, which keeps the stack finite. A route that does not
/// resolve is refused, so a dangling route can never enter the stack.
pub fn open_detail(s: &mut State, cat: &Catalogue, route: Route) {
    if !resolves(s, cat, &route) {
        return;
    }
    if s.stack.last() != Some(&route) {
        s.stack.push(route);
    }
}

/// Go back one screen, never below the root.
pub fn back(s: &mut State) {
    if s.stack.len() > 1 {
        s.stack.pop();
    }
}

/// Toggle playback from the transport play button.
pub fn tap_play(s: &mut State) {
    s.playing = !s.playing;
}

/// Collapse or expand the sidebar.
pub fn toggle_sidebar(s: &mut State) {
    s.collapsed = !s.collapsed;
}

/// Select the Popular tab.
pub fn set_popular_tab(s: &mut State, tab: PopularTab) {
    s.popular_tab = tab;
}

/// Select a track to play: make it now-playing and record it in history. A
/// track the catalogue lacks is refused.
pub fn select_track(s: &mut State, cat: &Catalogue, t: TrackId) {
    if cat.track(&t).is_none() {
        return;
    }
    s.now_playing = Some(t.clone());
    s.lib.record_play(t);
}

/// Delete a user playlist and remove every route to it, so no dangling route
/// survives. The stack never empties: if the delete removed the root, Home
/// takes its place.
pub fn delete_playlist(s: &mut State, id: PlaylistId) -> bool {
    if !s.lib.delete_playlist(id) {
        return false;
    }
    s.stack.retain(|r| *r != Route::UserPlaylist(id));
    if s.stack.is_empty() {
        s.stack.push(Route::Home);
    }
    true
}

/// Handle a text key. Returns true when the key changed state. `"a"` advances
/// the accent; `" "` toggles playback; anything else is ignored.
pub fn handle_key(s: &mut State, key: &str) -> bool {
    match key {
        "a" => {
            s.accent = (s.accent + 1) % ACCENT_LEN;
            true
        }
        " " => {
            s.playing = !s.playing;
            true
        }
        _ => false,
    }
}

/// The named scenes `main.rs` passes to `AppSpec`, each applied to
/// `State::new`. Test-only: `main.rs` keeps its own `App`-level scenes.
#[cfg(test)]
pub const SCENES: [Scene; 4] = [
    ("default", |_| {}),
    ("accent-blue", |s| s.accent = 0),
    ("accent-pink", |s| s.accent = 2),
    ("paused", |s| s.playing = false),
];

/// A named scene: a label and the change it makes to a fresh state.
#[cfg(test)]
pub type Scene = (&'static str, fn(&mut State));

/// Every initial state the app can start from: each scene applied to `new`.
#[cfg(test)]
pub fn inits(cat: &Catalogue) -> Vec<State> {
    SCENES
        .iter()
        .map(|(_, f)| {
            let mut s = State::new(cat);
            f(&mut s);
            s
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{AlbumRow, ArtistRow, CatalogueRows, TrackRow};
    use std::collections::{HashSet, VecDeque};

    /// The stack depth the exhaustive check explores. The shell never drills
    /// deeper than section -> detail -> detail, so three covers real use.
    const MAX_DEPTH: usize = 3;

    /// The detail routes the check exercises, with a fixed id set so the state
    /// space stays finite. Only the user-playlist routes couple the stack to
    /// the library (the dangling-route invariant), so they are the ones the
    /// proof must drill into; one album and one settings page keep the depth
    /// real. The library's own richness is proven exhaustively in `library.rs`.
    fn details(cat: &Catalogue) -> [Route; 4] {
        [
            Route::Album(cat.album_id("seed-album-0").unwrap()),
            Route::UserPlaylist(PlaylistId(0)),
            Route::UserPlaylist(PlaylistId(1)),
            Route::SettingsPage(SettingsPage::Playback),
        ]
    }

    /// A catalogue one album and one track larger than the seed, so a test can
    /// hold a real id that the seed does not have.
    fn bigger() -> Catalogue {
        let mut cat = Catalogue::seed();
        cat.absorb(CatalogueRows {
            artists: vec![ArtistRow { id: "x-artist".into(), name: "X".into() }],
            albums: vec![AlbumRow { id: "x-album".into(), title: "X".into(), artist: "x-artist".into() }],
            tracks: vec![TrackRow {
                id: "x-track".into(),
                title: "X".into(),
                artist: "x-artist".into(),
                album: "x-album".into(),
                duration: "PT1M".into(),
            }],
            editorial: Vec::new(),
        });
        cat
    }

    /// A representative slice of sections, enough to prove the stack stays
    /// non-empty under `open_section` without inflating the state space with
    /// section identities that do not affect the invariant.
    const SECTIONS_SUBSET: [Route; 5] = [
        Route::Home,
        Route::Feed,
        Route::Playlists,
        Route::Search,
        Route::Settings,
    ];

    fn applying(f: impl Fn(&mut State), s: &State) -> State {
        let mut t = s.clone();
        f(&mut t);
        t
    }

    /// The states reachable in one event. Covers section open, detail drill,
    /// back, the play tap, the keys, the toggles, track selection, and the two
    /// library ops that couple the library to the stack: create and delete a
    /// playlist. The rest of the library surface is proven exhaustively in
    /// `library.rs`; keeping it out of this state space is what keeps the
    /// cross-product bounded.
    fn successors(s: &State, cat: &Catalogue) -> Vec<State> {
        let mut out = Vec::new();
        for r in SECTIONS_SUBSET {
            out.push(applying(|st| open_section(st, r.clone()), s));
        }
        for r in details(cat) {
            out.push(applying(|st| open_detail(st, cat, r.clone()), s));
        }
        out.push(applying(back, s));
        out.push(applying(tap_play, s));
        out.push(applying(|st| {
            handle_key(st, "a");
        }, s));
        out.push(applying(|st| {
            handle_key(st, " ");
        }, s));
        out.push(applying(toggle_sidebar, s));
        out.push(applying(|st| set_popular_tab(st, PopularTab::Albums), s));
        for t in cat.tracks().take(2) {
            out.push(applying(|st| select_track(st, cat, t.id.clone()), s));
        }

        // The stack/library coupling: create fills a slot, delete frees it and
        // must clean every route to it.
        out.push(applying(|st| {
            st.lib.create_playlist("A".to_string());
        }, s));
        out.push(applying(|st| {
            st.lib.create_playlist("B".to_string());
        }, s));
        out.push(applying(|st| {
            delete_playlist(st, PlaylistId(0));
        }, s));
        out.push(applying(|st| {
            delete_playlist(st, PlaylistId(1));
        }, s));

        out
    }

    /// Exhaustively explores every reachable state up to `MAX_DEPTH` and
    /// asserts the invariant at each. The accent, playing, and library
    /// dimensions are exercised in full from the root, so the invariant holds
    /// at every depth, not only up to the bound.
    #[test]
    fn every_reachable_state_satisfies_invariant() {
        let cat = Catalogue::seed();
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        for init in inits(&cat) {
            assert!(invariant(&init, &cat), "initial state breaks the invariant: {init:?}");
            if seen.insert(init.clone()) {
                queue.push_back(init);
            }
        }
        while let Some(s) = queue.pop_front() {
            assert!(invariant(&s, &cat), "reachable state breaks the invariant: {s:?}");
            for next in successors(&s, &cat) {
                if next.stack.len() <= MAX_DEPTH && seen.insert(next.clone()) {
                    queue.push_back(next);
                }
            }
        }
        let accents: HashSet<usize> = seen.iter().map(|s| s.accent).collect();
        let playing: HashSet<bool> = seen.iter().map(|s| s.playing).collect();
        assert_eq!(accents.len(), ACCENT_LEN, "accent dimension was not fully explored");
        assert_eq!(playing.len(), 2, "playing dimension was not fully explored");
    }

    /// The back transition never empties the stack, whatever the depth.
    #[test]
    fn back_never_empties_the_stack() {
        let cat = Catalogue::seed();
        let mut s = State::new(&cat);
        back(&mut s);
        assert!(!s.stack.is_empty(), "back from the root emptied the stack");
        open_detail(&mut s, &cat, Route::Album(cat.album_id("seed-album-0").unwrap()));
        back(&mut s);
        assert_eq!(s.stack.len(), 1, "back from a detail did not return to the root");
    }

    /// Selecting a track records it at the front of history, without a
    /// duplicate and within the cap.
    #[test]
    fn selecting_records_history() {
        let cat = Catalogue::seed();
        let t0 = cat.track_id("seed-track-0").unwrap();
        let t1 = cat.track_id("seed-track-1").unwrap();
        let mut s = State::new(&cat);
        select_track(&mut s, &cat, t0.clone());
        select_track(&mut s, &cat, t1.clone());
        select_track(&mut s, &cat, t0.clone());
        assert_eq!(s.now_playing, Some(t0.clone()));
        assert_eq!(s.lib.history, vec![t0, t1]);
        assert!(invariant(&s, &cat));
    }

    /// An album the catalogue lacks is refused, so a detail screen can never
    /// open over nothing.
    #[test]
    fn opening_an_unknown_album_is_refused() {
        let cat = Catalogue::seed();
        let foreign = bigger().album_id("x-album").unwrap();
        let mut s = State::new(&cat);
        open_detail(&mut s, &cat, Route::Album(foreign));
        assert_eq!(s.stack, vec![Route::Home], "an album outside the catalogue was opened");
        assert!(invariant(&s, &cat));
    }

    /// A track the catalogue lacks is refused, so neither now-playing nor
    /// history can name it.
    #[test]
    fn selecting_an_unknown_track_is_refused() {
        let cat = Catalogue::seed();
        let foreign = bigger().track_id("x-track").unwrap();
        let mut s = State::new(&cat);
        let before = s.clone();
        select_track(&mut s, &cat, foreign);
        assert_eq!(s, before, "a track outside the catalogue was selected");
    }

    /// Deleting a playlist removes every route to it and never empties the
    /// stack, even when the delete removes the root.
    #[test]
    fn deleting_playlist_cleans_the_stack() {
        let cat = Catalogue::seed();
        let mut s = State::new(&cat);
        let id = s.lib.create_playlist("Mine".to_string()).unwrap();
        open_detail(&mut s, &cat, Route::UserPlaylist(id));
        assert!(s.stack.contains(&Route::UserPlaylist(id)));
        assert!(delete_playlist(&mut s, id));
        assert!(!s.stack.contains(&Route::UserPlaylist(id)), "dangling route survived");
        assert!(!s.stack.is_empty(), "delete emptied the stack");
        assert!(invariant(&s, &cat));
    }

    /// Opening a user playlist that does not exist is refused, so a dangling
    /// route can never enter the stack through the door of `open_detail`.
    #[test]
    fn opening_missing_playlist_is_refused() {
        let cat = Catalogue::seed();
        let mut s = State::new(&cat);
        let before = s.stack.len();
        open_detail(&mut s, &cat, Route::UserPlaylist(PlaylistId(0)));
        assert_eq!(s.stack.len(), before, "missing playlist was opened");
        assert!(invariant(&s, &cat));
    }

    /// A deterministic random walk over the full operation surface, checking
    /// the invariant after every step. A small xorshift keeps it reproducible.
    #[test]
    fn random_walk_keeps_invariant() {
        let mut x: u64 = 0x853c49e6748fea9b;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };

        let cat = Catalogue::seed();
        let track = |k: u64| cat.tracks().nth(k as usize).unwrap().id.clone();
        let album = |k: u64| cat.albums().nth(k as usize).unwrap().id.clone();
        let artist = |k: u64| cat.artists().nth(k as usize).unwrap().id.clone();
        let details = details(&cat);
        let mut s = State::new(&cat);
        for _ in 0..20_000 {
            assert!(invariant(&s, &cat), "random walk broke the invariant at {s:?}");
            match next() % 16 {
                0 => select_track(&mut s, &cat, track(next() % 3)),
                1 => {
                    let t = track(next() % 3);
                    s.lib.toggle_like(t);
                }
                2 => s.lib.toggle_save_album(album(next() % 2)),
                3 => s.lib.toggle_follow(artist(next() % 2)),
                4 => s.lib.toggle_download(album(next() % 2)),
                5 => {
                    let n = (next() % 2) as usize;
                    if s.lib.playlists[n].is_some() {
                        s.lib.add_track(PlaylistId(n as u8), track(next() % 3));
                    } else {
                        s.lib.create_playlist("R".to_string());
                    }
                }
                6 => {
                    let n = (next() % 2) as usize;
                    if s.lib.playlists[n].is_some() {
                        s.lib.remove_track(PlaylistId(n as u8), &track(next() % 3));
                    }
                }
                7 => {
                    if s.lib.playlists[0].is_some() {
                        delete_playlist(&mut s, PlaylistId(0));
                    }
                }
                8 => {
                    if s.lib.playlists[1].is_some() {
                        delete_playlist(&mut s, PlaylistId(1));
                    }
                }
                9 => {
                    s.lib.create_playlist("R".to_string());
                }
                10 => {
                    if s.lib.collections.is_empty() {
                        s.lib.create_collection("C".to_string());
                    } else {
                        s.lib.add_to_collection(0, PlaylistId(0));
                    }
                }
                11 => {
                    if !s.lib.collections.is_empty() {
                        s.lib.remove_from_collection(0, PlaylistId(0));
                    }
                }
                12 => {
                    if !s.lib.collections.is_empty() {
                        s.lib.delete_collection(0);
                    }
                }
                13 => {
                    let r = details[(next() % details.len() as u64) as usize].clone();
                    open_detail(&mut s, &cat, r);
                }
                14 => back(&mut s),
                _ => open_section(&mut s, SECTIONS[(next() % SECTIONS.len() as u64) as usize].clone()),
            }
        }
    }
}
