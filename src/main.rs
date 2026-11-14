//! OpenTidal, a Tidal-like music app built on Rune.
//! `cargo run` opens the window.
//! `cargo run -- --snap out.png --scene default` renders headless.

mod state;

use rune::tok::{self, radius, space, type_};
use rune::{AppSpec, Color, Cx, El, Icon, Key, ScrollSpec, col, el, icon, oklch, row, text};
use state::State;

// The accent count must match the rune palette the app draws from.
const _: () = assert!(state::ACCENT_LEN == tok::ACCENT_NAMES.len());

const NAV: [&str; state::NAV_LEN] =
    ["Home", "Explore", "My Collection", "Radio", "My Mixes", "Favorites"];
const PLAYLISTS: [&str; 4] = ["Chill", "Workout", "Focus", "Road Trip"];
const ALBUMS: [(&str, &str); 10] = [
    ("Currents", "Tame Impala"),
    ("Blonde", "Frank Ocean"),
    ("AM", "Arctic Monkeys"),
    ("RAM", "Daft Punk"),
    ("After Hours", "The Weeknd"),
    ("DAMN.", "Kendrick Lamar"),
    ("Vespertine", "Björk"),
    ("In Rainbows", "Radiohead"),
    ("Golden Hour", "Kacey Musgraves"),
    ("Seat at the Table", "Solange"),
];

const PLAY: Icon = Icon { name: "play", view: 24.0, body: "<path d=\"M8 5v14l11-7z\"/>" };
const PAUSE: Icon = Icon { name: "pause", view: 24.0, body: "<path d=\"M7 5h4v14H7zM13 5h4v14h-4z\"/>" };
const PREV: Icon = Icon { name: "prev", view: 24.0, body: "<path d=\"M6 5h2v14H6zM20 5v14l-9-7z\"/>" };
const NEXT: Icon = Icon { name: "next", view: 24.0, body: "<path d=\"M4 5l9 7-9 7zM16 5h2v14h-2z\"/>" };

/// A colourful vertical gradient for a placeholder album cover.
fn cover(i: usize) -> (Color, Color) {
    let hue = (i as f32 * 37.0 + 14.0) % 360.0;
    (oklch(0.62, 0.16, hue), oklch(0.34, 0.15, hue))
}

fn view(s: &State, _cx: &mut Cx<State>) -> El {
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
                .child(main_pane(t)),
        )
        .child(now_playing(s, t, accent))
}

fn sidebar(s: &State, t: tok::Theme, accent: Color) -> El {
    let mut nav = col().key("nav").gap(space::S1).px(space::S3);
    for (i, name) in NAV.iter().enumerate() {
        let selected = i == s.nav;
        nav = nav.child(
            el().key(format!("nav-{name}"))
                .label(*name)
                .checked(selected)
                .h(40.0)
                .px(space::S3)
                .radius(radius::MD)
                .center_y()
                .fill(if selected { t.hover } else { Color::TRANSPARENT })
                .hover(t.hover)
                .focusable(true)
                .on_press(move |st: &mut State| state::tap_nav(st, i))
                .child(
                    text(*name)
                        .key("label")
                        .font(type_::BODY)
                        .color(if selected { accent } else { t.text_secondary }),
                ),
        );
    }

    let mut pl = col().key("playlists").gap(space::S1).pt(space::S6).px(space::S3);
    pl = pl.child(
        text("Playlists")
            .key("pl-header")
            .font(type_::SECTION)
            .color(t.text_secondary)
            .px(space::S3),
    );
    for name in PLAYLISTS.iter() {
        pl = pl.child(
            el().key(format!("pl-{name}"))
                .label(*name)
                .h(34.0)
                .px(space::S3)
                .radius(radius::MD)
                .center_y()
                .hover(t.hover)
                .focusable(true)
                .on_press(|_: &mut State| {})
                .child(text(*name).key("label").font(type_::BODY).color(t.text_secondary)),
        );
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
        .child(nav)
        .child(pl)
}

fn main_pane(t: tok::Theme) -> El {
    let mut grid = col().key("grid").gap(space::S6);
    let mut r = row().gap(space::S6);
    for (i, (title, artist)) in ALBUMS.iter().enumerate() {
        r = r.child(album_card(i, title, artist, t));
        if (i + 1) % 5 == 0 {
            grid = grid.child(r);
            r = row().gap(space::S6);
        }
    }

    col()
        .key("main")
        .grow(1.0)
        .min_h(0.0)
        .child(
            col().px(space::S6).pt(space::S6).pb(space::S3).gap(space::S1)
                .child(text("Home").key("title").font(type_::TITLE).color(t.text))
                .child(
                    text("Your music, your way")
                        .key("subtitle")
                        .font(type_::BODY)
                        .color(t.text_secondary),
                ),
        )
        .child(
            col()
                .key("scroll")
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .px(space::S6)
                .pb(space::S6)
                .child(grid),
        )
}

fn album_card(i: usize, title: &str, artist: &str, t: tok::Theme) -> El {
    let (top, bottom) = cover(i);
    col().key(format!("album-{i}"))
        .w(176.0)
        .gap(space::S2)
        .child(
            el().key("cover")
                .size(176.0, 176.0)
                .radius(radius::MD)
                .gradient(top, bottom)
                .shadow(2.0, 12.0, t.shadow_soft),
        )
        .child(text(title).key("title").font(type_::BODY).color(t.text))
        .child(
            text(artist)
                .key("artist")
                .font(type_::LABEL)
                .color(t.text_secondary),
        )
}

fn now_playing(s: &State, t: tok::Theme, accent: Color) -> El {
    let prev_btn = el().key("prev")
        .label("Previous")
        .size(36.0, 36.0)
        .radius(18.0)
        .center()
        .hover(t.hover)
        .focusable(true)
        .on_press(|_: &mut State| {})
        .child(icon(&PREV, 16.0).color(t.text));
    let play_btn = el().key("play")
        .label(if s.playing { "Pause" } else { "Play" })
        .size(44.0, 44.0)
        .radius(22.0)
        .center()
        .fill(accent)
        .focusable(true)
        .on_press(|st: &mut State| state::tap_play(st))
        .child(icon(if s.playing { &PAUSE } else { &PLAY }, 20.0).color(t.bg));
    let next_btn = el().key("next")
        .label("Next")
        .size(36.0, 36.0)
        .radius(18.0)
        .center()
        .hover(t.hover)
        .focusable(true)
        .on_press(|_: &mut State| {})
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
    rune::run(AppSpec::<State> {
        scenes: &state::SCENES,
        on_key: Some(|s, ev, _tx| {
            if let Key::Text(k) = &ev.key {
                return state::handle_key(s, k.as_str());
            }
            false
        }),
        ..AppSpec::new("OpenTidal", (1280.0, 800.0), State::default, view)
    });
}
