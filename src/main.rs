//! OpenTidal, a Tidal-like music app built on Rune.
//! `cargo run` opens the window.
//! `cargo run -- --snap out.png --scene default` renders headless.

mod app;
mod atomic;
mod auth;
mod catalogue;
mod covers;
mod library;
mod pager;
mod screens;
mod state;
mod store;
mod tidal;

use rune::tok::{self, radius, space, type_};
use rune::{AppSpec, Color, Cx, El, Icon, Key, ScrollSpec, col, el, icon, row, text};
use app::App;
use state::{Group, Route, State};

// The accent count must match the rune palette the app draws from.
const _: () = assert!(state::ACCENT_LEN == tok::ACCENT_NAMES.len());

const PLAY: Icon = Icon { name: "play", view: 24.0, body: "<path d=\"M8 5v14l11-7z\"/>" };
const PAUSE: Icon = Icon { name: "pause", view: 24.0, body: "<path d=\"M7 5h4v14H7zM13 5h4v14h-4z\"/>" };
const PREV: Icon = Icon { name: "prev", view: 24.0, body: "<path d=\"M6 5h2v14H6zM20 5v14l-9-7z\"/>" };
const NEXT: Icon = Icon { name: "next", view: 24.0, body: "<path d=\"M4 5l9 7-9 7zM16 5h2v14h-2z\"/>" };
const HEART: Icon = Icon { name: "heart", view: 24.0, body: "<path d=\"M12 21s-6.7-4.35-9.33-8.11C.6 9.9 2.08 5.9 5.5 5.9c2.07 0 3.44 1.17 4.5 2.56 1.06-1.39 2.43-2.56 4.5-2.56 3.42 0 4.9 4 2.83 6.99C18.7 16.65 12 21 12 21z\"/>" };

type Scene = (&'static str, fn(&mut App));

const SCENES: [Scene; 10] = [
    ("default", |_| {}),
    ("accent-blue", |a| a.core.accent = 0),
    ("accent-pink", |a| a.core.accent = 2),
    ("paused", |a| a.core.playing = false),
    ("signed-in", |a| {
        a.core.lib.set_name("Fake User".to_string());
        a.auth = auth::Auth::SignedIn {
            token: auth::Token { access_token: "fake".to_string(), refresh_token: "fake".to_string(), expires_in: 0 },
        };
    }),
    ("signed-in-stored", signed_in_stored_scene),
    ("signed-in-synced", signed_in_synced_scene),
    ("signed-in-covers", signed_in_covers_scene),
    ("signed-in-paged", signed_in_paged_scene),
    ("signed-in-details", signed_in_details_scene),
];

/// Signs in as a user the database can key on, so a driven scene proves the write
/// reaches disk. The token is a JWT because the key comes from its `uid` claim.
///
/// The scene writes a fabricated token and a fabricated library, so it owns both
/// stores. With live mode on, `App::default` hands the app the real keychain and the
/// real database path, and a drive would replace the developer's session with a fake.
/// That happened: an old binary in live mode replaced a real token. Forcing both here
/// is what stops it, and `the_stored_scene_owns_its_store_and_its_database` checks it.
fn signed_in_stored_scene(a: &mut App) {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let payload = URL_SAFE_NO_PAD.encode(br#"{"uid":173210380,"exp":4102444800}"#);
    let token = auth::Token {
        access_token: format!("h.{payload}.sig"),
        refresh_token: "fake-refresh".to_string(),
        expires_in: 0,
    };
    a.live = false;
    a.revalidate_on_entry = false;
    a.service = std::sync::Arc::new(tidal::FakeService::default());
    a.tokens = std::sync::Arc::new(auth::MemoryStore::default());
    // A scratch database, so the library this scene writes cannot land in the real
    // one. `OPENTIDAL_DB` names the file outright, which is the contract `db_path`
    // keeps: a run that pointed two instances at one file needs exactly that file.
    // Without it, a directory of this run's own, named for the process.
    // A path already set on the app is kept: a test points the scene at its own file,
    // and the scene must write there rather than somewhere of its own choosing.
    let db = a.db_override.clone().or_else(|| {
        std::env::var("OPENTIDAL_DB")
            .ok()
            .filter(|p| !p.is_empty())
            .map(std::path::PathBuf::from)
    }).unwrap_or_else(|| {
            std::env::temp_dir()
                .join("opentidal-scenes")
                .join(format!("run-{}", std::process::id()))
                .join("scene.db")
    });
    if let Some(dir) = db.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    // The covers this scene fetches go beside its database, never into the
    // user's cache, unless a test already named a root.
    if a.cover_root_override.is_none() {
        a.cover_root_override = db.parent().map(|dir| dir.join("covers"));
    }
    a.db_override = Some(db);
    // A driven scene must never write a fabricated token into the real keychain. The
    // assignment above is the fix; this refuses the drive if it ever stops working,
    // rather than replacing someone's session in silence.
    assert!(
        a.tokens.is_memory(),
        "the stored scene has a persistent token store, so a drive would overwrite a real session"
    );
    a.tokens.save(&token);
    a.setup(std::sync::Arc::new(|| ()));
    a.core.lib.set_name("Stored User".to_string());
}

/// The stored scene, with a fake service whose library holds one liked track and
/// one Tidal playlist. Nothing syncs by itself: the Sync with Tidal button on the
/// Account screen starts it, the way the deterministic build always does.
fn signed_in_synced_scene(a: &mut App) {
    use catalogue::{CatalogueRows, PlaylistRow};
    signed_in_stored_scene(a);
    const EVENING: &str = "f6b9f0a1-6c8e-4d24-9c71-1d0e2f3a4b5c";
    let evening = PlaylistRow {
        id: EVENING.to_string(),
        name: "Evening Drive".to_string(),
        description: "A playlist from Tidal".to_string(),
        tracks: vec!["seed-track-0".to_string(), "seed-track-1".to_string()],
        cover: String::new(),
    };
    let pages = [
        (library::RemoteList::Liked, one_page(CatalogueRows::default(), "seed-track-3")),
        (
            library::RemoteList::Playlists,
            one_page(CatalogueRows { playlists: vec![evening], ..CatalogueRows::default() }, EVENING),
        ),
    ];
    a.service = std::sync::Arc::new(tidal::FakeService { pages: pages.into(), ..tidal::FakeService::default() });
}

/// A list that is one last page holding one member.
fn one_page(rows: catalogue::CatalogueRows, member: &str) -> Vec<Result<tidal::Page, tidal::ServiceError>> {
    vec![Ok(tidal::Page { rows, members: vec![member.to_string()], next: None })]
}

/// The stored scene, with a fake service whose library saves one album with a
/// cover, and whose cover is a solid magenta PNG. The covers land beside the
/// scene's database, so a second run with the same `OPENTIDAL_DB` reads them
/// back with no fetch. Each decode is waited on, so it lands before the snap.
fn signed_in_covers_scene(a: &mut App) {
    use catalogue::{AlbumRow, CatalogueRows};
    signed_in_stored_scene(a);
    let rows = CatalogueRows {
        albums: vec![AlbumRow {
            id: "fixture-album-1".to_string(),
            title: "Fixture Cover".to_string(),
            artist: "seed-artist-0".to_string(),
            cover: "fixture-cover-1".to_string(),
        }],
        ..CatalogueRows::default()
    };
    let magenta = include_bytes!("../tests/fixtures/covers/magenta.png").to_vec();
    a.service = std::sync::Arc::new(tidal::FakeService {
        pages: [(library::RemoteList::Albums, one_page(rows, "fixture-album-1"))].into(),
        covers: [("fixture-cover-1".to_string(), Ok(magenta))].into(),
        ..tidal::FakeService::default()
    });
    a.wait_for_decode = true;
}

/// The stored scene, with a fake service whose saved albums run to 145 over
/// eight pages: five on the first, so they leave room below, then 20 a page.
/// Entering a list reads its first page, and only scrolling reads past what is
/// in view, as a live session does.
fn signed_in_paged_scene(a: &mut App) {
    use catalogue::{AlbumRow, CatalogueRows};
    signed_in_stored_scene(a);
    a.revalidate_on_entry = true;
    let ids: Vec<String> = (0..145).map(|n| format!("paged-album-{n}")).collect();
    let mut pages = Vec::new();
    let mut start = 0;
    for size in std::iter::once(5).chain(std::iter::repeat_n(20, 7)) {
        let members = ids[start..start + size].to_vec();
        let albums = members
            .iter()
            .map(|id| AlbumRow { id: id.clone(), title: format!("Album {}", &id[12..]), artist: "seed-artist-0".to_string(), cover: String::new() })
            .collect();
        pages.push(Ok(tidal::Page { rows: CatalogueRows { albums, ..CatalogueRows::default() }, members, next: None }));
        start += size;
    }
    a.service = std::sync::Arc::new(tidal::FakeService {
        pages: [(library::RemoteList::Albums, pages)].into(),
        ..tidal::FakeService::default()
    });
}

/// The stored scene, with a fake service whose account follows one artist.
/// Opening the artist reads its three albums, and opening the second album
/// reads its two tracks. Only the artist read brings the first and third
/// albums, and only the album read brings the tracks. The artist's profile art
/// is the solid magenta PNG, and each decode is waited on, so it lands before
/// the snap.
fn signed_in_details_scene(a: &mut App) {
    use catalogue::{AlbumRow, ArtistRow, CatalogueRows, TrackRow};
    signed_in_stored_scene(a);
    a.revalidate_on_entry = true;
    let artist = || ArtistRow { id: "detail-artist-1".to_string(), name: "Detail Artist".to_string() };
    let album = |n: usize, title: &str| AlbumRow {
        id: format!("detail-album-{n}"),
        title: title.to_string(),
        artist: "detail-artist-1".to_string(),
        cover: String::new(),
    };
    let track = |n: usize, title: &str| TrackRow {
        id: format!("detail-track-{n}"),
        title: title.to_string(),
        artist: "detail-artist-1".to_string(),
        album: "detail-album-2".to_string(),
        duration: "PT3M".to_string(),
    };
    a.catalogue.absorb(CatalogueRows { artists: vec![artist()], ..CatalogueRows::default() });
    let artist_id = a.catalogue.artist_id("detail-artist-1").expect("the scene artist");
    let followed = one_page(CatalogueRows { artists: vec![artist()], ..CatalogueRows::default() }, "detail-artist-1");
    let discography = vec![Ok(tidal::Page {
        rows: CatalogueRows {
            artists: vec![artist()],
            albums: vec![album(1, "First Album"), album(2, "Second Album"), album(3, "Third Album")],
            ..CatalogueRows::default()
        },
        members: (1..=3).map(|n| format!("detail-album-{n}")).collect(),
        next: None,
    })];
    let mut pages: std::collections::BTreeMap<_, _> =
        [(library::RemoteList::Artists, followed), (library::RemoteList::Artist(artist_id), discography)].into();
    // The album read is keyed by an id only the catalogue mints, so the second
    // album is absorbed here to name it. It is the first card of the artist's
    // grid, because the grid keeps catalogue order.
    a.catalogue.absorb(CatalogueRows { albums: vec![album(2, "Second Album")], ..CatalogueRows::default() });
    let album_id = a.catalogue.album_id("detail-album-2").expect("the scene album");
    let tracks = vec![Ok(tidal::Page {
        rows: CatalogueRows {
            tracks: vec![track(1, "Detail Track One"), track(2, "Detail Track Two")],
            ..CatalogueRows::default()
        },
        members: vec!["detail-track-1".to_string(), "detail-track-2".to_string()],
        next: None,
    })];
    pages.insert(library::RemoteList::Album(album_id), tracks);
    let magenta = include_bytes!("../tests/fixtures/covers/magenta.png").to_vec();
    a.service = std::sync::Arc::new(tidal::FakeService {
        pages,
        covers: [("artist-detail-artist-1".to_string(), Ok(magenta))].into(),
        ..tidal::FakeService::default()
    });
    a.wait_for_decode = true;
}

fn view(app: &App, _cx: &mut Cx<App>) -> El {
    // The library may have changed during this frame's events. Ask for one more
    // frame so `drain` writes it; a window at rest draws none.
    app.wake_if_dirty();
    let s = &app.core;
    let t = tok::theme(true);
    let accent = tok::accent(s.accent, true);
    let cat = &app.catalogue;

    col()
        .key("app")
        .w_full()
        .h_full()
        .fill(t.bg)
        .child(
            row().key("body").grow(1.0).min_h(0.0)
                .child(sidebar(app, t, accent))
                .child(main_col(app, t, accent)),
        )
        .child(now_playing(s, cat, t, accent))
}

fn nav_item(route: Route, selected: bool, t: tok::Theme, accent: Color) -> El {
    let name = route.label();
    el().key(format!("nav-{name}"))
        .label(name)
        .checked(selected)
        .h(34.0)
        .px(space::S3)
        .radius(radius::MD)
        .center_y()
        .fill(if selected { t.hover } else { Color::TRANSPARENT })
        .hover(t.hover)
        .focusable(true)
        .on_press(move |a: &mut App| {
            a.rename.clear();
            state::open_section(&mut a.core, route.clone())
        })
        .child(
            text(name)
                .key("label")
                .font(type_::BODY)
                .color(if selected { accent } else { t.text_secondary }),
        )
}

fn sidebar(app: &App, t: tok::Theme, accent: Color) -> El {
    let s = &app.core;
    let sidebar = col().key("sidebar")
        .w(if s.collapsed { 64.0 } else { 240.0 })
        .h_full()
        .child(
            text(if s.collapsed { "T" } else { "TIDAL" })
                .key("logo")
                .font(type_::TITLE)
                .color(t.text)
                .px(if s.collapsed { space::S3 } else { space::S6 })
                .pt(space::S6)
                .pb(space::S3),
        );
    if s.collapsed {
        return sidebar;
    }
    let mut primary = col().key("primary").gap(space::S1).px(space::S2).pt(space::S2);
    let mut library = col().key("library").gap(space::S1).px(space::S2);
    let mut bottom = col().key("bottom").gap(space::S1).px(space::S2).pb(space::S2).pt(space::S3);

    library = library.child(
        text("Your Library")
            .key("lib-header")
            .font(type_::SECTION)
            .color(t.text_secondary)
            .px(space::S3)
            .pt(space::S3)
            .pb(space::S1),
    );

    for route in state::SECTIONS {
        let selected = state::current(s) == &route;
        let group = route.group();
        let item = nav_item(route, selected, t, accent);
        match group {
            Group::Primary => primary = primary.child(item),
            Group::Library => library = library.child(item),
            Group::Bottom => bottom = bottom.child(item),
        }
    }

    let name = s.lib.name.clone();
    let user = text(&name)
        .key("user-name")
        .font(type_::LABEL)
        .color(t.text_secondary)
        .px(space::S3)
        .pt(space::S2);

    sidebar
        .child(primary)
        .child(library.grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }))
        .child(user)
        .child(bottom)
}

fn back_button(t: tok::Theme) -> El {
    el().key("back")
        .label("Back")
        .h(32.0)
        .px(space::S3)
        .radius(radius::MD)
        .center_y()
        .hover(t.hover)
        .focusable(true)
        .on_press(|a: &mut App| {
            a.rename.clear();
            state::back(&mut a.core)
        })
        .child(text("Back").key("label").font(type_::BODY).color(t.text))
}

fn search_button(t: tok::Theme) -> El {
    el().key("search-go")
        .label("Search")
        .h(32.0)
        .px(space::S3)
        .radius(radius::MD)
        .center_y()
        .hover(t.hover)
        .focusable(true)
        .on_press(|a: &mut App| {
            a.rename.clear();
            state::open_section(&mut a.core, Route::Search)
        })
        .child(text("Search").key("label").font(type_::BODY).color(t.text_secondary))
}

fn settings_button(t: tok::Theme) -> El {
    el().key("settings-go")
        .label("Settings")
        .h(32.0)
        .px(space::S3)
        .radius(radius::MD)
        .center_y()
        .hover(t.hover)
        .focusable(true)
        .on_press(|a: &mut App| {
            a.rename.clear();
            state::open_section(&mut a.core, Route::Settings)
        })
        .child(text("Settings").key("label").font(type_::BODY).color(t.text_secondary))
}

fn top_bar(s: &State, t: tok::Theme) -> El {
    let label = if s.collapsed { "Expand sidebar" } else { "Collapse sidebar" };
    let toggle = el().key("toggle-sidebar").label(label)
        .h(32.0).px(space::S3).radius(radius::MD).center_y()
        .hover(t.hover).focusable(true)
        .on_press(|a: &mut App| state::toggle_sidebar(&mut a.core))
        .child(text(label).key("label").font(type_::BODY).color(t.text_secondary));
    let mut left = row().gap(space::S3).center_y().child(toggle);
    if s.stack.len() > 1 {
        left = left.child(back_button(t));
    }
    left = left.child(search_button(t));

    row().key("top-bar")
        .h(48.0)
        .px(space::S6)
        .center_y()
        .justify_between()
        .border(1.0, t.separator)
        .child(left)
        .child(settings_button(t))
}

fn main_col(app: &App, t: tok::Theme, accent: Color) -> El {
    col().key("main")
        .grow(1.0)
        .min_h(0.0)
        .child(top_bar(&app.core, t))
        .child(screen_body(app, t, accent))
}

fn screen_body(app: &App, t: tok::Theme, accent: Color) -> El {
    let s = &app.core;
    let cat = &app.catalogue;
    let covers = app.covers();
    match state::current(s) {
        Route::Home => screens::home(cat, covers, t),
        Route::Feed => screens::feed(t),
        Route::Popular => screens::popular(s, cat, covers, t),
        Route::ForYou => screens::for_you(cat, covers, t),
        Route::LikedSongs => screens::liked_songs(app, t),
        Route::Albums => screens::albums(app, t),
        Route::Artists => screens::artists(app, t),
        Route::Playlists => screens::playlists(app, t),
        Route::Collections => screens::collections(app, t),
        Route::Local => screens::local(s, cat, t),
        Route::History => screens::history(s, cat, t),
        Route::Stats => screens::stats(s, cat, t),
        Route::Downloads => screens::downloads(s, cat, t),
        Route::Search => screens::search(cat, covers, t, &app.search),
        Route::Settings => screens::settings(t),
        Route::SettingsPage(p) => screens::settings_page(*p, app, t),
        route => screens::detail(route, app, accent, t),
    }
}

fn now_playing(s: &State, cat: &catalogue::Catalogue, t: tok::Theme, accent: Color) -> El {
    let tr = s.now_playing.as_ref().and_then(|id| cat.track(id));
    let liked = s.now_playing.as_ref().is_some_and(|id| s.lib.is_liked(id));
    let title = tr.map_or("Nothing playing", |tr| tr.title.as_str());
    let artist = tr.and_then(|tr| cat.artist(&tr.artist)).map_or("", |a| a.name.as_str());

    let like_btn = el().key("like")
        .label(if liked { "Unlike" } else { "Like" })
        .size(36.0, 36.0)
        .radius(18.0)
        .center()
        .hover(t.hover)
        .focusable(true)
        .on_press(move |a: &mut App| {
            if let Some(tr) = a.core.now_playing.clone() {
                a.core.lib.toggle_like(tr);
            }
        })
        .child(icon(&HEART, 16.0).color(if liked { accent } else { t.text_secondary }));

    let prev_btn = el().key("prev")
        .label("Previous")
        .size(36.0, 36.0)
        .radius(18.0)
        .center()
        .hover(t.hover)
        .focusable(true)
        .on_press(|_: &mut App| {})
        .child(icon(&PREV, 16.0).color(t.text));
    let play_btn = el().key("play")
        .label(if s.playing { "Pause" } else { "Play" })
        .size(44.0, 44.0)
        .radius(22.0)
        .center()
        .fill(accent)
        .focusable(true)
        .on_press(|a: &mut App| state::tap_play(&mut a.core))
        .child(icon(if s.playing { &PAUSE } else { &PLAY }, 20.0).color(t.bg));
    let next_btn = el().key("next")
        .label("Next")
        .size(36.0, 36.0)
        .radius(18.0)
        .center()
        .hover(t.hover)
        .focusable(true)
        .on_press(|_: &mut App| {})
        .child(icon(&NEXT, 16.0).color(t.text));

    row()
        .key("now-playing")
        .h(80.0)
        .w_full()
        .px(space::S6)
        .center_y()
        .justify_between()
        .fill(t.card)
        .border(1.0, t.separator)
        .child(
            row().gap(space::S3).center_y()
                .child(like_btn)
                .child(
                    col().gap(2.0)
                        .child(text(title).key("track").font(type_::BODY).color(t.text))
                        .child(text(artist).key("artist").font(type_::LABEL).color(t.text_secondary)),
                ),
        )
        .child(
            col().key("transport").w(480.0).gap(space::S1)
                .child(row().center().gap(space::S6).child(prev_btn).child(play_btn).child(next_btn))
                .child(progress(t, accent)),
        )
        .child(volume(t, accent))
}

fn progress(t: tok::Theme, accent: Color) -> El {
    row().key("progress").center_y().gap(space::S2)
        .child(text("1:24").key("cur").font(type_::MONO_CAPTION).color(t.text_secondary))
        .child(
            el().key("track")
                .w(360.0)
                .h(3.0)
                .radius(1.5)
                .fill(t.control_off)
                .child(el().key("fill").absolute(0.0, 0.0).w(144.0).h(3.0).radius(1.5).fill(accent)),
        )
        .child(text("3:50").key("total").font(type_::MONO_CAPTION).color(t.text_secondary))
}

fn volume(t: tok::Theme, accent: Color) -> El {
    row().key("volume").center_y().gap(space::S2)
        .child(text("Volume").key("vol-label").font(type_::LABEL).color(t.text_secondary).a11y_hidden())
        .child(
            el().key("vol-track")
                .w(120.0)
                .h(3.0)
                .radius(1.5)
                .fill(t.control_off)
                .child(el().key("vol-fill").absolute(0.0, 0.0).w(72.0).h(3.0).radius(1.5).fill(accent)),
        )
}

fn main() {
    let spec = AppSpec::<App> {
        scenes: &SCENES,
        setup: Some(|app, waker| app.setup(waker)),
        poll: Some(|app, _t, _tx| app.drain()),
        on_key: Some(|app, ev, _tx| {
            if let Key::Text(k) = &ev.key {
                return state::handle_key(&mut app.core, k.as_str());
            }
            false
        }),
        ..AppSpec::new("OpenTidal", (1280.0, 800.0), App::default, view)
    };
    // A quit takes the fast path, which drops no state, so the last save must
    // happen in a window hook. `AppSpec` has no hook fields, so drive the app
    // through `App`, which does.
    rune::App::new(spec)
        .on_quit(|app, _tx| {
            app.quit();
        })
        .on_close(|app, _tx| {
            app.quit();
        })
        .run();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};

    use catalogue::CoverRequest;
    use library::RemoteList;
    use rune::{Env, Runtime};
    use tidal::{Cursor, MusicService, Page, ServiceError, User};

    /// The paged scene's fake service behind a gate. An open gate answers at
    /// once. A held gate makes each page wait for a verdict: `Ok` reads the
    /// fake page, `Err` answers that error instead.
    struct Gate {
        inner: Arc<dyn MusicService>,
        held: AtomicBool,
        verdicts: Mutex<mpsc::Receiver<Result<(), ServiceError>>>,
        pages: AtomicUsize,
    }

    impl MusicService for Gate {
        fn current_user(&self, token: &str) -> Result<User, ServiceError> {
            self.inner.current_user(token)
        }
        fn page(&self, token: &str, user: &str, of: &RemoteList, after: Option<&Cursor>) -> Result<Page, ServiceError> {
            self.pages.fetch_add(1, Ordering::SeqCst);
            if self.held.load(Ordering::SeqCst) {
                let verdict = self.verdicts.lock().expect("gate").recv().unwrap_or(Ok(()));
                verdict?;
            }
            self.inner.page(token, user, of, after)
        }
        fn cover(&self, token: &str, cover: &CoverRequest) -> Result<Vec<u8>, ServiceError> {
            self.inner.cover(token, cover)
        }
    }

    struct Paged {
        rt: Runtime<App>,
        gate: Arc<Gate>,
        verdicts: mpsc::Sender<Result<(), ServiceError>>,
    }

    impl Paged {
        /// The `signed-in-paged` scene on `db`, drawn in a window of the app's size.
        fn open(db: &std::path::Path, held: bool) -> Paged {
            Paged::open_scene(db, held, signed_in_paged_scene)
        }

        fn open_scene(db: &std::path::Path, held: bool, scene: fn(&mut App)) -> Paged {
            let mut app = App::default();
            app.db_override = Some(db.to_path_buf());
            scene(&mut app);
            let (verdicts, rx) = mpsc::channel();
            let gate = Arc::new(Gate {
                inner: app.service.clone(),
                held: AtomicBool::new(held),
                verdicts: Mutex::new(rx),
                pages: AtomicUsize::new(0),
            });
            app.service = gate.clone();
            let mut rt = Runtime::new(app, view, (1280.0, 800.0), Env::new(true, 1.0));
            rt.poll = Some(|app, _, _| app.drain());
            rt.frame(0.0);
            Paged { rt, gate, verdicts }
        }

        fn open_albums(&mut self) {
            self.rt.update(|a, _| state::open_section(&mut a.core, Route::Albums));
        }

        fn status(&self) -> PassStatusLine {
            let line = self.rt.elements().into_iter().find(|e| e.key.as_deref() == Some("pass-status")).and_then(|e| e.text);
            PassStatusLine(line)
        }

        fn shows(&self, key: &str) -> bool {
            self.rt.find(key).is_some()
        }

        /// Draws frames until `done`, giving the workers real time to answer.
        fn until(&mut self, what: &str, done: impl Fn(&Paged) -> bool) {
            for _ in 0..400 {
                self.rt.frame(1.0 / 60.0);
                if done(self) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            panic!("never {what}: status {:?}", self.status());
        }

        /// Frames until no page is in flight and nothing more is wanted.
        fn settle(&mut self) {
            self.until("settled", |p| p.rt.state.pass_status(RemoteList::Albums) != pager::PassStatus::Loading);
            for _ in 0..30 {
                self.rt.frame(1.0 / 60.0);
            }
        }

        /// Opens the gate, answers the page that waits on it, and settles.
        fn release(&mut self) {
            self.gate.held.store(false, Ordering::SeqCst);
            self.verdicts.send(Ok(())).expect("gate");
            self.settle();
        }

        fn scroll(&mut self, dy: f32) {
            self.rt.wheel(700.0, 400.0, dy);
            for _ in 0..30 {
                self.rt.frame(1.0 / 60.0);
            }
        }
    }

    #[derive(Debug, PartialEq)]
    struct PassStatusLine(Option<String>);

    impl PassStatusLine {
        fn says(&self, start: &str) -> bool {
            self.0.as_deref().is_some_and(|line| line.starts_with(start))
        }
    }

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("opentidal-paged-{name}-{}", std::process::id()));
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(&dir).expect("scratch directory");
            Scratch(dir)
        }
        fn db(&self) -> std::path::PathBuf {
            self.0.join("scene.db")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// Opening an artist reads its albums, and opening one of them reads its
    /// tracks. Neither read changes a library list.
    #[test]
    fn opening_an_artist_and_an_album_reads_their_details() {
        let scratch = Scratch::new("details");
        let mut p = Paged::open_scene(&scratch.db(), false, signed_in_details_scene);
        let artist = p.rt.state.catalogue.artist_id("detail-artist-1").expect("the scene artist");
        let album = p.rt.state.catalogue.album_id("detail-album-2").expect("the scene album");
        // The stored scene starts a profile fetch before this test holds the
        // service. Let it land first, or a name change from that fetch is blamed
        // on the detail read. This failed on Linux and passed on Mac.
        p.until("settled the profile", |p| p.rt.state.core.lib.name == "Fake User");
        let before = p.rt.state.core.lib.clone();
        p.rt.update(|a, _| state::open_detail(&mut a.core, &a.catalogue, Route::Artist(artist.clone())));
        p.until("showed the artist's albums", |p| p.shows("album-2"));
        assert!(!p.shows("detail-track-1"), "a track showed before its album was opened");
        p.rt.update(|a, _| state::open_detail(&mut a.core, &a.catalogue, Route::Album(album.clone())));
        p.until("showed the album's tracks", |p| p.shows("detail-track-1") && p.shows("track-1"));
        assert_eq!(p.rt.state.core.lib, before, "a detail read changed the library");
    }

    /// The Artists grid draws each artist's profile art, not only the gradient.
    #[test]
    fn an_artist_card_shows_the_artist_picture() {
        let scratch = Scratch::new("artist-card");
        let mut p = Paged::open_scene(&scratch.db(), false, signed_in_details_scene);
        p.rt.update(|a, _| state::open_section(&mut a.core, Route::Artists));
        p.until("showed the artist's picture on its card", |p| p.shows("artist-0/cover-image"));
        assert!(!p.shows("artist-0/cover"), "the card kept its gradient beside the picture");
    }

    /// The first page holds five albums, so the rows in view want more, and
    /// album 50 sits on a page only a scroll asks for.
    #[test]
    fn a_scroll_reads_the_page_its_rows_need() {
        let scratch = Scratch::new("scroll");
        let mut p = Paged::open(&scratch.db(), false);
        p.open_albums();
        p.settle();
        assert!(p.shows("paged-album-0"), "the first page did not show");
        assert!(!p.shows("paged-album-50"), "a page no row needs was read");
        p.scroll(-900.0);
        p.settle();
        assert!(p.shows("paged-album-50"), "the scroll did not read the next page, status {:?}", p.status());
    }

    /// A restart shows the albums the file holds while page one is still out.
    #[test]
    fn cached_rows_show_before_the_first_page_lands() {
        let scratch = Scratch::new("cached");
        let mut first = Paged::open(&scratch.db(), false);
        first.open_albums();
        first.settle();
        assert!(first.rt.state.quit(), "the first run did not write its library");
        drop(first);

        let mut p = Paged::open(&scratch.db(), true);
        p.open_albums();
        p.until("asked for page one", |p| p.gate.pages.load(Ordering::SeqCst) == 1);
        for _ in 0..10 {
            p.rt.frame(1.0 / 60.0);
        }
        assert!(p.shows("paged-album-0") && p.shows("paged-album-10"), "the cached rows waited for the page");
        assert_eq!(p.status(), PassStatusLine(Some("Loading from Tidal…".to_string())));
        p.release();
        assert!(p.shows("paged-album-10"), "page one dropped the cached rows");
    }

    /// Coming back to a list read to its end reads page one again, behind the
    /// rows already shown.
    #[test]
    fn reentering_a_cached_screen_shows_rows_before_the_refresh_lands() {
        let scratch = Scratch::new("reenter");
        let mut p = Paged::open(&scratch.db(), false);
        p.open_albums();
        p.rt.update(|a, _| a.sync());
        p.until("read to the end", |p| p.rt.state.pass_status(RemoteList::Albums) == pager::PassStatus::Complete);
        p.rt.update(|a, _| state::open_section(&mut a.core, Route::Home));
        p.settle();
        p.gate.held.store(true, Ordering::SeqCst);
        let before = p.gate.pages.load(Ordering::SeqCst);
        p.open_albums();
        p.until("asked for the refresh", |p| p.gate.pages.load(Ordering::SeqCst) == before + 1);
        for _ in 0..10 {
            p.rt.frame(1.0 / 60.0);
        }
        assert!(p.shows("paged-album-0") && p.shows("paged-album-10"), "reentry hid the rows until the refresh");
        assert!(p.status().says("Loading"), "the refresh is not shown: {:?}", p.status());
        p.release();
    }

    /// A failed page says so, and a scroll that wants more rows reads it again.
    #[test]
    fn a_stalled_pass_says_so_and_a_larger_scroll_retries_it() {
        let scratch = Scratch::new("stall");
        let mut p = Paged::open(&scratch.db(), false);
        p.open_albums();
        p.settle();
        p.gate.held.store(true, Ordering::SeqCst);
        let before = p.gate.pages.load(Ordering::SeqCst);
        p.scroll(-700.0);
        p.until("asked for the next page", |p| p.gate.pages.load(Ordering::SeqCst) == before + 1);
        p.verdicts.send(Err(ServiceError::Http("500".to_string()))).expect("gate");
        p.until("stalled", |p| p.status().says("Tidal did not send the rest"));
        assert!(!p.shows("paged-album-50"));

        p.verdicts.send(Ok(())).expect("gate");
        p.scroll(-900.0);
        p.until("read the page again", |p| p.gate.pages.load(Ordering::SeqCst) >= before + 2);
        p.release();
        assert!(p.shows("paged-album-50"), "the retry did not land, status {:?}", p.status());
    }
}
