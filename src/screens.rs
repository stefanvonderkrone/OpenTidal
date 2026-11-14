//! The section and detail screens, built from shared card, shelf, grid, and
//! track-row builders over the placeholder catalog in `data`.

use rune::tok::{self, radius, space, type_};
use rune::{Color, El, ScrollSpec, TextEdit, col, el, oklch, row, text, text_input};
use crate::app::App;
use crate::data::{self, Album, Artist, Playlist, Track};
use crate::state::{self, PopularTab, Route, SettingsPage, State};

/// A colourful vertical gradient for a placeholder cover.
pub fn cover(i: usize) -> (Color, Color) {
    let hue = (i as f32 * 37.0 + 14.0) % 360.0;
    (oklch(0.62, 0.16, hue), oklch(0.34, 0.15, hue))
}

fn album_at(i: usize) -> Album {
    data::ALBUMS.get(i).copied().unwrap_or(Album { title: "Album", artist: "Unknown" })
}

fn artist_at(i: usize) -> Artist {
    data::ARTISTS.get(i).copied().unwrap_or(Artist { name: "Artist" })
}

fn playlist_at(i: usize) -> Playlist {
    data::PLAYLISTS.get(i).copied().unwrap_or(Playlist { name: "Playlist", description: "" })
}

/// A square media card that drills into a detail route on press.
fn card(i: usize, title: &str, subtitle: &str, route: Route, t: tok::Theme) -> El {
    let (top, bottom) = cover(i);
    col().key(format!("card-{i}"))
        .w(168.0)
        .gap(space::S2)
        .on_press(move |a: &mut App| state::open_detail(&mut a.core, route))
        .child(
            el().key("cover")
                .size(168.0, 168.0)
                .radius(radius::MD)
                .gradient(top, bottom)
                .shadow(2.0, 12.0, t.shadow_soft),
        )
        .child(text(title).key("title").font(type_::BODY).color(t.text))
        .child(text(subtitle).key("subtitle").font(type_::LABEL).color(t.text_secondary))
}

fn album_card(i: usize, a: Album, t: tok::Theme) -> El {
    card(i, a.title, a.artist, Route::Album(i), t)
}

fn artist_card(i: usize, a: Artist, t: tok::Theme) -> El {
    let (top, bottom) = cover(i + 3);
    col().key(format!("artist-{i}"))
        .w(120.0)
        .gap(space::S2)
        .on_press(move |a: &mut App| state::open_detail(&mut a.core, Route::Artist(i)))
        .child(
            el().key("cover")
                .size(120.0, 120.0)
                .radius(60.0)
                .gradient(top, bottom)
                .shadow(2.0, 12.0, t.shadow_soft),
        )
        .child(text(a.name).key("name").font(type_::BODY).color(t.text).center())
}

fn playlist_card(i: usize, p: Playlist, t: tok::Theme) -> El {
    card(i, p.name, p.description, Route::Playlist(i), t)
}

/// A horizontal shelf: a section title over one scrollable row of cards.
fn shelf(title: &str, t: tok::Theme, cards: Vec<El>) -> El {
    let mut r = row().gap(space::S3);
    for c in cards {
        r = r.child(c);
    }
    col().gap(space::S2)
        .child(text(title).key("shelf-title").font(type_::SECTION).color(t.text))
        .child(r.scroll_x())
}

/// A wrapping grid of cards, `per` per row.
fn grid(cards: Vec<El>, per: usize) -> El {
    let mut g = col().gap(space::S3);
    let mut r = row().gap(space::S3);
    let mut count = 0;
    for c in cards {
        r = r.child(c);
        count += 1;
        if count % per == 0 {
            g = g.child(r);
            r = row().gap(space::S3);
        }
    }
    if count % per != 0 {
        g = g.child(r);
    }
    g
}

fn track_row(i: usize, tr: Track, selected: bool, t: tok::Theme) -> El {
    row().key(format!("track-{i}"))
        .checked(selected)
        .fill(if selected { t.hover } else { Color::TRANSPARENT })
        .focusable(true)
        .on_press(move |a: &mut App| state::select_track(&mut a.core, i, data::TRACKS.len()))
        .h(44.0)
        .px(space::S2)
        .radius(radius::MD)
        .center_y()
        .gap(space::S3)
        .hover(t.hover)
        .child(text(format!("{:02}", i + 1)).key("num").font(type_::LABEL).color(t.text_secondary).w(24.0))
        .child(
            col().grow(1.0).gap(2.0)
                .child(text(tr.title).key("title").font(type_::BODY).color(t.text))
                .child(text(tr.artist).key("track-artist").font(type_::LABEL).color(t.text_secondary)),
        )
        .child(text(tr.duration).key("duration").font(type_::LABEL).color(t.text_secondary))
}

fn track_list(selected: Option<usize>, t: tok::Theme) -> El {
    let mut l = col().key("tracks").gap(space::S1);
    for (i, tr) in data::TRACKS.iter().enumerate() {
        l = l.child(track_row(i, *tr, selected == Some(i), t));
    }
    l
}

fn header(title: &str, subtitle: &str, t: tok::Theme) -> El {
    col().px(space::S6).pt(space::S6).pb(space::S3).gap(space::S1)
        .child(text(title).key("title").font(type_::TITLE).color(t.text))
        .child(text(subtitle).key("subtitle").font(type_::BODY).color(t.text_secondary))
}

/// A full page: a header over a scrollable body. Fills the space the top bar
/// leaves.
fn page(title: &str, subtitle: &str, t: tok::Theme, body: El) -> El {
    col().grow(1.0).min_h(0.0)
        .child(header(title, subtitle, t))
        .child(
            col().key("scroll")
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .px(space::S6)
                .pb(space::S6)
                .child(body),
        )
}

fn albums_grid(t: tok::Theme) -> El {
    let cards: Vec<El> = data::ALBUMS.iter().enumerate().map(|(i, a)| album_card(i, *a, t)).collect();
    grid(cards, 5)
}

fn artists_grid(t: tok::Theme) -> El {
    let cards: Vec<El> = data::ARTISTS.iter().enumerate().map(|(i, a)| artist_card(i, *a, t)).collect();
    grid(cards, 6)
}

fn playlists_grid(t: tok::Theme) -> El {
    let cards: Vec<El> = data::PLAYLISTS.iter().enumerate().map(|(i, p)| playlist_card(i, *p, t)).collect();
    grid(cards, 5)
}

// --- Section screens -----------------------------------------------------

pub fn home(t: tok::Theme) -> El {
    let rec: Vec<El> = data::ALBUMS[0..5].iter().enumerate().map(|(i, a)| album_card(i, *a, t)).collect();
    let new: Vec<El> = data::ALBUMS[5..].iter().enumerate().map(|(i, a)| album_card(i + 5, *a, t)).collect();
    page("Home", "Your music, your way", t,
        col().gap(space::S6)
            .child(shelf("Recommended for you", t, rec))
            .child(shelf("New releases", t, new)))
}

pub fn feed(t: tok::Theme) -> El {
    let mut rows = col().gap(space::S1);
    let items: [&str; 6] = [
        "New single from Tame Impala",
        "The Weeknd added After Hours",
        "Kendrick Lamar released DAMN.",
        "Radiohead remastered In Rainbows",
        "Björk shared a new video",
        "Your friend liked Blonde",
    ];
    for (i, s) in items.iter().enumerate() {
        rows = rows.child(
            row().key(format!("feed-{i}")).h(44.0).px(space::S2).radius(radius::MD).center_y().hover(t.hover)
                .child(text(*s).key("item").font(type_::BODY).color(t.text)),
        );
    }
    page("Feed", "Activity from artists you follow", t, rows)
}

pub fn popular(s: &State, t: tok::Theme) -> El {
    let tabs = row().gap(space::S2).pb(space::S3)
        .child(tab("Artists", PopularTab::Artists, s, t))
        .child(tab("Albums", PopularTab::Albums, s, t))
        .child(tab("Tracks", PopularTab::Tracks, s, t));
    let body = match s.popular_tab {
        PopularTab::Artists => artists_grid(t),
        PopularTab::Albums => albums_grid(t),
        PopularTab::Tracks => track_list(s.now_playing, t),
    };
    page("Popular", "Global charts", t,
        col().gap(space::S3).child(tabs).child(body))
}

fn tab(label: &str, tab: PopularTab, s: &State, t: tok::Theme) -> El {
    let active = s.popular_tab == tab;
    el().key(format!("tab-{label}"))
        .label(label)
        .h(34.0)
        .px(space::S3)
        .radius(17.0)
        .center_y()
        .checked(active)
        .fill(if active { t.text } else { t.hover })
        .on_press(move |a: &mut App| state::set_popular_tab(&mut a.core, tab))
        .child(text(label).key("label").font(type_::BODY).color(if active { t.bg } else { t.text }))
}

pub fn for_you(t: tok::Theme) -> El {
    let a: Vec<El> = data::ALBUMS.iter().take(5).enumerate().map(|(i, x)| album_card(i, *x, t)).collect();
    let b: Vec<El> = data::ALBUMS.iter().skip(5).enumerate().map(|(i, x)| album_card(i + 5, *x, t)).collect();
    page("For You", "Built from what you play", t,
        col().gap(space::S6)
            .child(shelf("New releases for you", t, a))
            .child(shelf("From your genres", t, b)))
}

pub fn liked_songs(s: &State, t: tok::Theme) -> El {
    page("Liked Songs", "Tracks you saved", t, track_list(s.now_playing, t))
}

pub fn albums(t: tok::Theme) -> El {
    page("Albums", "Saved albums", t, albums_grid(t))
}

pub fn artists(t: tok::Theme) -> El {
    page("Artists", "Artists you follow", t, artists_grid(t))
}

pub fn playlists(t: tok::Theme) -> El {
    page("Playlists", "Your playlists", t, playlists_grid(t))
}

pub fn collections(t: tok::Theme) -> El {
    page("Collections", "Folders and collections", t, playlists_grid(t))
}

pub fn local(s: &State, t: tok::Theme) -> El {
    page("On this device", "Music you downloaded", t, track_list(s.now_playing, t))
}

pub fn history(s: &State, t: tok::Theme) -> El {
    page("History", "Recently played", t, track_list(s.now_playing, t))
}

pub fn stats(t: tok::Theme) -> El {
    let mut g = row().gap(space::S3);
    let stats: [(&str, &str); 4] = [("Top artist", "The Weeknd"), ("Top track", "Blinding Lights"), ("Scrobbles", "1,284"), ("Minutes", "4,210")];
    for (label, value) in stats {
        g = g.child(
            col().key(format!("stat-{label}")).w(200.0).px(space::S3).pt(space::S3).pb(space::S3).gap(space::S1)
                .fill(t.card).radius(radius::MD)
                .child(text(label).key("label").font(type_::LABEL).color(t.text_secondary))
                .child(text(value).key("value").font(type_::TITLE).color(t.text)),
        );
    }
    page("Stats", "Your listening at a glance", t, g)
}

pub fn downloads(t: tok::Theme) -> El {
    let mut rows = col().gap(space::S1);
    for (i, a) in data::ALBUMS.iter().take(5).enumerate() {
        rows = rows.child(
            row().key(format!("dl-{i}")).h(44.0).px(space::S2).radius(radius::MD).center_y().gap(space::S3).hover(t.hover)
                .child(text(a.title).key("title").font(type_::BODY).color(t.text).grow(1.0))
                .child(text(a.artist).key("artist").font(type_::LABEL).color(t.text_secondary))
                .child(text("Done").key("state").font(type_::LABEL).color(t.text_secondary)),
        );
    }
    page("Downloads", "Your download queue", t, rows)
}

pub fn search(t: tok::Theme, edit: &TextEdit) -> El {
    let field = text_input(edit, type_::BODY)
        .key("search-box")
        .h(40.0)
        .w_full()
        .px(space::S3)
        .radius(radius::MD)
        .fill(t.hover)
        .single_line()
        .on_edit(|a: &mut App| &mut a.search)
        .placeholder("Search artists, albums, tracks", t.text_secondary);
    page("Search", "Find anything in the catalogue", t,
        col().gap(space::S3).child(field).child(albums_grid(t)))
}

pub fn settings(t: tok::Theme) -> El {
    let mut rows = col().gap(space::S1);
    let items: [SettingsPage; 6] = [
        SettingsPage::Playback,
        SettingsPage::Quality,
        SettingsPage::Downloads,
        SettingsPage::Notifications,
        SettingsPage::Import,
        SettingsPage::About,
    ];
    for (i, page) in items.iter().enumerate() {
        let p = *page;
        rows = rows.child(
            row().key(format!("set-{i}")).h(44.0).px(space::S3).radius(radius::MD).center_y().justify_between().hover(t.hover)
                .focusable(true)
                .on_press(move |a: &mut App| state::open_detail(&mut a.core, Route::SettingsPage(p)))
                .child(text(p.label()).key("label").font(type_::BODY).color(t.text))
                .child(text(">").key("chevron").font(type_::BODY).color(t.text_secondary)),
        );
    }
    page("Settings", "Tune OpenTidal", t, rows)
}

/// A settings subpage. The rows themselves are placeholders until a real
/// settings model exists; the navigation and back behaviour are real.
pub fn settings_page(p: SettingsPage, t: tok::Theme) -> El {
    page(p.label(), "Settings", t, col().gap(space::S3))
}

// --- Detail screens ------------------------------------------------------

fn action_button(label: &str, accent: Color, filled: bool, t: tok::Theme) -> El {
    el().key(format!("btn-{label}"))
        .label(label)
        .h(40.0)
        .px(space::S3)
        .radius(radius::MD)
        .center_y()
        .fill(if filled { accent } else { t.hover })
        .focusable(true)
        .on_press(|_: &mut App| {})
        .child(text(label).key("label").font(type_::BODY).color(if filled { t.bg } else { t.text }))
}

fn hero(cover_el: El, title: &str, subtitle: &str, accent: Color, t: tok::Theme) -> El {
    row().px(space::S6).pt(space::S6).pb(space::S3).gap(space::S6)
        .child(cover_el)
        .child(
            col().gap(space::S2)
                .child(text(title).key("hero-title").font(type_::TITLE).color(t.text))
                .child(text(subtitle).key("hero-subtitle").font(type_::BODY).color(t.text_secondary))
                .child(row().gap(space::S2).pt(space::S3)
                    .child(action_button("Play", accent, true, t))
                    .child(action_button("Shuffle", accent, false, t))),
        )
}

/// A detail screen: a hero over a track list. The back affordance lives in
/// the top bar, not here.
pub fn detail(route: Route, s: &State, accent: Color, t: tok::Theme) -> El {
    let (cover_el, title, subtitle) = match route {
        Route::Album(i) => {
            let a = album_at(i);
            let (top, bottom) = cover(i);
            (el().key("hero-cover").size(200.0, 200.0).radius(radius::MD).gradient(top, bottom).shadow(2.0, 12.0, t.shadow_soft), a.title.to_string(), format!("Album by {}", a.artist))
        }
        Route::Artist(i) => {
            let a = artist_at(i);
            let (top, bottom) = cover(i + 3);
            (el().key("hero-cover").size(200.0, 200.0).radius(100.0).gradient(top, bottom).shadow(2.0, 12.0, t.shadow_soft), a.name.to_string(), "Artist".to_string())
        }
        Route::Playlist(i) => {
            let p = playlist_at(i);
            let (top, bottom) = cover(i);
            (el().key("hero-cover").size(200.0, 200.0).radius(radius::MD).gradient(top, bottom).shadow(2.0, 12.0, t.shadow_soft), p.name.to_string(), p.description.to_string())
        }
        _ => {
            let (top, bottom) = cover(0);
            (el().key("hero-cover").size(200.0, 200.0).radius(radius::MD).gradient(top, bottom), "Detail".to_string(), String::new())
        }
    };

    col().grow(1.0).min_h(0.0)
        .child(hero(cover_el, &title, &subtitle, accent, t))
        .child(
            col().key("scroll")
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .px(space::S6)
                .pb(space::S6)
                .child(track_list(s.now_playing, t)),
        )
}
