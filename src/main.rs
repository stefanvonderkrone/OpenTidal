//! OpenTidal, a Tidal-like music app built on Rune.
//! `cargo run` opens the window.
//! `cargo run -- --snap out.png --scene default` renders headless.

mod app;
mod data;
mod screens;
mod state;

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

const SCENES: [(&str, fn(&mut App)); 4] = [
    ("default", |_| {}),
    ("accent-blue", |a| a.core.accent = 0),
    ("accent-pink", |a| a.core.accent = 2),
    ("paused", |a| a.core.playing = false),
];

fn view(app: &App, _cx: &mut Cx<App>) -> El {
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
                .child(sidebar(s, t, accent))
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
        .on_press(move |a: &mut App| state::open_section(&mut a.core, route))
        .child(
            text(name)
                .key("label")
                .font(type_::BODY)
                .color(if selected { accent } else { t.text_secondary }),
        )
}

fn sidebar(s: &State, t: tok::Theme, accent: Color) -> El {
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

    col()
        .key("sidebar")
        .w(240.0)
        .h_full()
        .child(
            text("TIDAL")
                .key("logo")
                .font(type_::TITLE)
                .color(t.text)
                .px(space::S6)
                .pt(space::S6)
                .pb(space::S3),
        )
        .child(primary)
        .child(library.grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }))
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
        .on_press(|a: &mut App| state::back(&mut a.core))
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
        .on_press(|a: &mut App| state::open_section(&mut a.core, Route::Search))
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
        .on_press(|a: &mut App| state::open_section(&mut a.core, Route::Settings))
        .child(text("Settings").key("label").font(type_::BODY).color(t.text_secondary))
}

fn top_bar(s: &State, t: tok::Theme) -> El {
    let mut left = row().gap(space::S3).center_y();
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
        Route::LikedSongs => screens::liked_songs(t),
        Route::Albums => screens::albums(t),
        Route::Artists => screens::artists(t),
        Route::Playlists => screens::playlists(t),
        Route::Collections => screens::collections(t),
        Route::Local => screens::local(t),
        Route::History => screens::history(t),
        Route::Stats => screens::stats(t),
        Route::Downloads => screens::downloads(t),
        Route::Search => screens::search(t, &app.search),
        Route::Settings => screens::settings(t),
        Route::SettingsPage(p) => screens::settings_page(p, t),
        route => screens::detail(route, accent, t),
    }
}

fn now_playing(s: &State, t: tok::Theme, accent: Color) -> El {
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
            col().gap(2.0)
                .child(text("Blinding Lights").key("track").font(type_::BODY).color(t.text))
                .child(text("The Weeknd").key("artist").font(type_::LABEL).color(t.text_secondary)),
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
    rune::run(AppSpec::<App> {
        scenes: &SCENES,
        on_key: Some(|app, ev, _tx| {
            if let Key::Text(k) = &ev.key {
                return state::handle_key(&mut app.core, k.as_str());
            }
            false
        }),
        ..AppSpec::new("OpenTidal", (1280.0, 800.0), App::default, view)
    });
}
