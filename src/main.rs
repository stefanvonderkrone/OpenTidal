//! OpenTidal, a Tidal-like music app built on Rune.
//! `cargo run` opens the window.
//! `cargo run -- --snap out.png --scene default` renders headless.

mod app;
mod atomic;
mod auth;
mod catalogue;
mod library;
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

const SCENES: [Scene; 7] = [
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
    let library = library::LibraryRows {
        catalogue: CatalogueRows {
            playlists: vec![PlaylistRow {
                id: "f6b9f0a1-6c8e-4d24-9c71-1d0e2f3a4b5c".to_string(),
                name: "Evening Drive".to_string(),
                description: "A playlist from Tidal".to_string(),
                tracks: vec!["seed-track-0".to_string(), "seed-track-1".to_string()],
                cover: String::new(),
            }],
            ..CatalogueRows::default()
        },
        liked: vec!["seed-track-3".to_string()],
        tidal_playlists: vec!["f6b9f0a1-6c8e-4d24-9c71-1d0e2f3a4b5c".to_string()],
        ..library::LibraryRows::default()
    };
    a.service = std::sync::Arc::new(tidal::FakeService { library: Ok(library), ..tidal::FakeService::default() });
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
    match state::current(s) {
        Route::Home => screens::home(cat, t),
        Route::Feed => screens::feed(t),
        Route::Popular => screens::popular(s, cat, t),
        Route::ForYou => screens::for_you(cat, t),
        Route::LikedSongs => screens::liked_songs(s, cat, t),
        Route::Albums => screens::albums(s, cat, t),
        Route::Artists => screens::artists(s, cat, t),
        Route::Playlists => screens::playlists(s, cat, t),
        Route::Collections => screens::collections(app, t),
        Route::Local => screens::local(s, cat, t),
        Route::History => screens::history(s, cat, t),
        Route::Stats => screens::stats(s, cat, t),
        Route::Downloads => screens::downloads(s, cat, t),
        Route::Search => screens::search(cat, t, &app.search),
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
