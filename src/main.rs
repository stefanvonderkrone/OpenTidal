//! OpenTidal, a Tidal-like music app built on Rune.
//! `cargo run` opens the window.
//! `cargo run -- --snap out.png --scene default` renders headless.

mod app;
mod auth;
mod catalog;
mod library;
mod screens;
mod state;
mod store;
mod tidal;

use rune::tok::{self, radius, space, type_};
use rune::{AppSpec, Color, Cx, El, Icon, Key, ScrollSpec, col, el, icon, row, text};
use app::App;
use catalog::TrackId;
use state::{Group, Route, State};

// The accent count must match the rune palette the app draws from.
const _: () = assert!(state::ACCENT_LEN == tok::ACCENT_NAMES.len());

const PLAY: Icon = Icon { name: "play", view: 24.0, body: "<path d=\"M8 5v14l11-7z\"/>" };
const PAUSE: Icon = Icon { name: "pause", view: 24.0, body: "<path d=\"M7 5h4v14H7zM13 5h4v14h-4z\"/>" };
const PREV: Icon = Icon { name: "prev", view: 24.0, body: "<path d=\"M6 5h2v14H6zM20 5v14l-9-7z\"/>" };
const NEXT: Icon = Icon { name: "next", view: 24.0, body: "<path d=\"M4 5l9 7-9 7zM16 5h2v14h-2z\"/>" };
const HEART: Icon = Icon { name: "heart", view: 24.0, body: "<path d=\"M12 21s-6.7-4.35-9.33-8.11C.6 9.9 2.08 5.9 5.5 5.9c2.07 0 3.44 1.17 4.5 2.56 1.06-1.39 2.43-2.56 4.5-2.56 3.42 0 4.9 4 2.83 6.99C18.7 16.65 12 21 12 21z\"/>" };

type Scene = (&'static str, fn(&mut App));

const SCENES: [Scene; 6] = [
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
    // Signs in as a user the database can key on, so a driven scene proves the
    // write reaches the disk. The token is a JWT because the key comes from its
    // `uid` claim.
    ("signed-in-stored", |a| {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let payload = URL_SAFE_NO_PAD.encode(br#"{"uid":173210380,"exp":4102444800}"#);
        let token = auth::Token {
            access_token: format!("h.{payload}.sig"),
            refresh_token: "fake-refresh".to_string(),
            expires_in: 0,
        };
        a.store.save(&token);
        a.setup(std::sync::Arc::new(|| ()));
        a.core.lib.set_name("Stored User".to_string());
    }),
];

fn view(app: &App, _cx: &mut Cx<App>) -> El {
    // The library may have changed during this frame's events. Ask for one more
    // frame so `drain` writes it; a window at rest draws none.
    app.wake_if_dirty();
    let s = &app.core;
    let t = tok::theme(true);
    let accent = tok::accent(s.accent, true);

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
        .child(now_playing(s, t, accent))
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
            state::open_section(&mut a.core, route)
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
        let item = nav_item(route, state::current(s) == route, t, accent);
        match route.group() {
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
    match state::current(&app.core) {
        Route::Home => screens::home(t),
        Route::Feed => screens::feed(t),
        Route::Popular => screens::popular(&app.core, t),
        Route::ForYou => screens::for_you(t),
        Route::LikedSongs => screens::liked_songs(&app.core, t),
        Route::Albums => screens::albums(&app.core, t),
        Route::Artists => screens::artists(&app.core, t),
        Route::Playlists => screens::playlists(&app.core, t),
        Route::Collections => screens::collections(app, t),
        Route::Local => screens::local(&app.core, t),
        Route::History => screens::history(&app.core, t),
        Route::Stats => screens::stats(&app.core, t),
        Route::Downloads => screens::downloads(&app.core, t),
        Route::Search => screens::search(t, &app.search),
        Route::Settings => screens::settings(t),
        Route::SettingsPage(p) => screens::settings_page(p, app, t),
        route => screens::detail(route, app, accent, t),
    }
}

fn now_playing(s: &State, t: tok::Theme, accent: Color) -> El {
    let id = s.now_playing.unwrap_or(TrackId(0));
    let tr = catalog::track(id);
    let liked = s.lib.is_liked(id);

    let like_btn = el().key("like")
        .label(if liked { "Unlike" } else { "Like" })
        .size(36.0, 36.0)
        .radius(18.0)
        .center()
        .hover(t.hover)
        .focusable(true)
        .on_press(move |a: &mut App| {
            if let Some(tr) = a.core.now_playing {
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
                        .child(text(tr.title).key("track").font(type_::BODY).color(t.text))
                        .child(text(catalog::artist(tr.artist).name).key("artist").font(type_::LABEL).color(t.text_secondary)),
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
        .on_quit(|app, _tx| app.quit())
        .on_close(|app, _tx| app.quit())
        .run();
}
