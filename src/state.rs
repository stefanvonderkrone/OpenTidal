//! OpenTidal's application state and its transitions, extracted from the view
//! so a checker can prove the state-machine invariant without compiling the
//! rune/wgpu view layer. `main.rs` delegates every state change to these
//! functions; nothing here depends on `rune`.

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
    About,
}

impl SettingsPage {
    pub fn label(self) -> &'static str {
        match self {
            SettingsPage::Playback => "Playback",
            SettingsPage::Quality => "Quality",
            SettingsPage::Downloads => "Downloads",
            SettingsPage::Notifications => "Notifications",
            SettingsPage::Import => "Import",
            SettingsPage::About => "About",
        }
    }
}

/// A screen in the app. The section variants are the top-level sidebar and
/// top-bar entries. The detail variants carry a catalog id so a detail screen
/// can never exist without its subject.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
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
    Album(usize),
    Artist(usize),
    Playlist(usize),
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
    /// replaces it with the catalog entry's title.
    pub fn label(self) -> &'static str {
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
            Route::Playlist(_) => "Playlist",
        }
    }

    /// The sidebar region the section route belongs to.
    pub fn group(self) -> Group {
        match self {
            Route::Home | Route::Feed | Route::Popular | Route::ForYou => Group::Primary,
            Route::LikedSongs | Route::Albums | Route::Artists | Route::Playlists
            | Route::Collections | Route::Local | Route::History | Route::Stats => Group::Library,
            Route::Downloads | Route::Search | Route::Settings | Route::SettingsPage(_) => {
                Group::Bottom
            }
            Route::Album(_) | Route::Artist(_) | Route::Playlist(_) => Group::Library,
        }
    }
}

/// The sidebar sections in order (tideway's primary block, then "Your
/// Library", then Downloads). `Search` and `Settings` live in the top bar and
/// are absent here.
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

/// The app's state. The navigation stack is never empty; its last element is
/// the current screen.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    pub stack: Vec<Route>,
    pub playing: bool,
    pub accent: usize,
    pub collapsed: bool,
    pub popular_tab: PopularTab,
    pub now_playing: Option<usize>,
}

impl Default for State {
    fn default() -> Self {
        State {
            stack: vec![Route::Home],
            playing: true,
            accent: 0,
            collapsed: false,
            popular_tab: PopularTab::Artists,
            now_playing: Some(0),
        }
    }
}

/// The safety invariant the verification proves: every reachable state has
/// `accent < ACCENT_LEN`, so the `accent + 1` in [`handle_key`] can never
/// overflow `usize`, and a non-empty stack, so [`current`] always names a
/// screen.
pub fn invariant(s: &State) -> bool {
    s.accent < ACCENT_LEN && !s.stack.is_empty()
}

/// The current screen. Falls back to Home on an empty stack, which the
/// invariant says never happens.
pub fn current(s: &State) -> Route {
    *s.stack.last().unwrap_or(&Route::Home)
}

/// Open a top-level section, replacing any detail depth.
pub fn open_section(s: &mut State, route: Route) {
    s.stack.clear();
    s.stack.push(route);
}

/// Drill into a detail screen, stacking it on the current one. Reopening the
/// same route is a no-op, which keeps the stack finite.
pub fn open_detail(s: &mut State, route: Route) {
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

/// Select the now-playing track by index into `data::TRACKS`. The only writer
/// of `now_playing`, so the index is in range by construction: `count == 0`
/// yields `None`, otherwise `i` wraps into `[0, count)`.
pub fn select_track(s: &mut State, i: usize, count: usize) {
    s.now_playing = if count == 0 { None } else { Some(i % count) };
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
/// `State::default()`.
pub const SCENES: [(&str, fn(&mut State)); 4] = [
    ("default", |_| {}),
    ("accent-blue", |s| s.accent = 0),
    ("accent-pink", |s| s.accent = 2),
    ("paused", |s| s.playing = false),
];

/// Every initial state the app can start from: each scene applied to `default`.
pub fn inits() -> Vec<State> {
    SCENES
        .iter()
        .map(|(_, f)| {
            let mut s = State::default();
            f(&mut s);
            s
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashSet, VecDeque};

    /// The stack depth the exhaustive check explores. The shell never drills
    /// deeper than section -> detail -> detail, so three covers real use.
    const MAX_DEPTH: usize = 3;

    /// The detail routes the check exercises, with a fixed id set so the state
    /// space stays finite. Settings subpages are detail routes too.
    const DETAILS: [Route; 6] = [
        Route::Album(0),
        Route::Album(1),
        Route::Artist(0),
        Route::Playlist(0),
        Route::SettingsPage(SettingsPage::Playback),
        Route::SettingsPage(SettingsPage::About),
    ];

    fn applying(f: impl Fn(&mut State), s: &State) -> State {
        let mut t = s.clone();
        f(&mut t);
        t
    }

    /// The states reachable in one event: every section open, every detail
    /// drill, back, the play tap, the two handled keys, and the new toggles.
    /// The toggles leave `accent` and `stack` alone, so they exercise the new
    /// fields without changing the invariant's subject.
    fn successors(s: &State) -> Vec<State> {
        let mut out = Vec::new();
        for r in SECTIONS.iter().chain([Route::Search, Route::Settings].iter()) {
            out.push(applying(|st| open_section(st, *r), s));
        }
        for r in DETAILS {
            out.push(applying(|st| open_detail(st, r), s));
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
        out.push(applying(|st| select_track(st, 3, 10), s));
        out
    }

    /// Exhaustively explores every reachable state up to `MAX_DEPTH` and
    /// asserts the invariant at each. The accent dimension is explored in full
    /// (all six accents and both playing states are reached at depth one), so
    /// the accent bound holds at every depth, not only up to the bound.
    #[test]
    fn every_reachable_state_satisfies_invariant() {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        for init in inits() {
            assert!(invariant(&init), "initial state breaks the invariant: {init:?}");
            if seen.insert(init.clone()) {
                queue.push_back(init);
            }
        }
        while let Some(s) = queue.pop_front() {
            assert!(invariant(&s), "reachable state breaks the invariant: {s:?}");
            for next in successors(&s) {
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
        let mut s = State::default();
        back(&mut s);
        assert!(!s.stack.is_empty(), "back from the root emptied the stack");
        open_detail(&mut s, Route::Album(0));
        back(&mut s);
        assert_eq!(s.stack.len(), 1, "back from a detail did not return to the root");
    }

    /// `select_track` is the only writer of `now_playing`, and it clamps the
    /// index into `[0, count)` for the real catalog length, so the view can
    /// index `data::TRACKS` without an out-of-range panic.
    #[test]
    fn track_index_stays_in_bounds() {
        let n = crate::data::TRACKS.len();
        assert!(n > 0, "the catalog must not be empty");
        let mut s = State::default();
        for i in [0, 3, n - 1, n, usize::MAX] {
            select_track(&mut s, i, n);
            assert!(s.now_playing.unwrap() < n, "selected {i} out of range for {n}");
        }
        let mut empty = State::default();
        select_track(&mut empty, 0, 0);
        assert_eq!(empty.now_playing, None, "an empty catalog must select nothing");
    }
}
