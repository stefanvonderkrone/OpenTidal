//! The section and detail screens, built from shared card, shelf, grid, and
//! track-row helpers over the catalogue and the user's library. The library is
//! real state now: every screen reads it and every action mutates it through
//! the proven operations.

use rune::tok::{self, radius, space, type_};
use rune::{Color, El, ScrollSpec, TextEdit, col, el, oklch, row, text, text_input};
use crate::app::App;
use crate::catalog::{self, Album, Artist, EditorialPlaylistId, Track, TrackId};
use crate::library::{self, PlaylistId};
use crate::state::{self, PopularTab, Route, SettingsPage, State};

/// A colourful vertical gradient for a placeholder cover.
pub fn cover(i: usize) -> (Color, Color) {
    let hue = (i as f32 * 37.0 + 14.0) % 360.0;
    (oklch(0.62, 0.16, hue), oklch(0.34, 0.15, hue))
}

// --- Cards ---------------------------------------------------------------

/// A square media card that drills into a detail route on press.
fn card(key: &str, seed: usize, title: &str, subtitle: &str, route: Route, t: tok::Theme) -> El {
    let (top, bottom) = cover(seed);
    col().key(key)
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

fn album_card(a: Album, t: tok::Theme) -> El {
    card(&format!("album-{}", a.id.0), a.id.0 as usize, a.title, catalog::artist(a.artist).name, Route::Album(a.id), t)
}

fn artist_card(a: Artist, t: tok::Theme) -> El {
    let id = a.id;
    let (top, bottom) = cover(a.id.0 as usize + 3);
    col().key(format!("artist-{}", a.id.0))
        .w(120.0)
        .gap(space::S2)
        .on_press(move |app: &mut App| state::open_detail(&mut app.core, Route::Artist(id)))
        .child(
            el().key("cover")
                .size(120.0, 120.0)
                .radius(60.0)
                .gradient(top, bottom)
                .shadow(2.0, 12.0, t.shadow_soft),
        )
        .child(text(a.name).key("name").font(type_::BODY).color(t.text).center())
}

fn editorial_card(id: EditorialPlaylistId, t: tok::Theme) -> El {
    let p = catalog::editorial_playlist(id);
    card(&format!("editorial-{}", id.0), id.0 as usize + 6, p.name, p.description, Route::EditorialPlaylist(id), t)
}

fn playlist_card(id: PlaylistId, name: &str, t: tok::Theme) -> El {
    card(&format!("playlist-{}", id.0), id.0 as usize + 10, name, "Your playlist", Route::UserPlaylist(id), t)
}

// --- Shelves and grids ---------------------------------------------------

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

// --- Track rows ----------------------------------------------------------

fn track_row(tr: Track, selected: bool, t: tok::Theme) -> El {
    row().key(format!("track-{}", tr.id.0))
        .checked(selected)
        .fill(if selected { t.hover } else { Color::TRANSPARENT })
        .focusable(true)
        .on_press(move |a: &mut App| state::select_track(&mut a.core, tr.id))
        .h(44.0)
        .px(space::S2)
        .radius(radius::MD)
        .center_y()
        .gap(space::S3)
        .hover(t.hover)
        .child(
            col().grow(1.0).gap(2.0)
                .child(text(tr.title).key("title").font(type_::BODY).color(t.text))
                .child(text(catalog::artist(tr.artist).name).key("track-artist").font(type_::LABEL).color(t.text_secondary)),
        )
        .child(text(tr.duration).key("duration").font(type_::LABEL).color(t.text_secondary))
}

fn track_list(tracks: Vec<Track>, selected: Option<TrackId>, t: tok::Theme) -> El {
    let mut l = col().key("tracks").gap(space::S1);
    for tr in tracks {
        l = l.child(track_row(tr, selected == Some(tr.id), t));
    }
    l
}

fn empty(msg: &str, t: tok::Theme) -> El {
    text(msg).key("empty").font(type_::BODY).color(t.text_secondary)
}

/// A track row inside a user playlist: selectable, with Remove, Up and Down.
fn playlist_track_row(pid: PlaylistId, pos: usize, len: usize, tr: Track, selected: bool, t: tok::Theme) -> El {
    let mut actions = row().gap(space::S1);
    actions = actions.child(
        el().key(format!("up-{}-{}", pid.0, pos)).label("Move up")
            .size(28.0, 28.0).radius(radius::SM).center().hover(t.hover).focusable(true)
            .on_press(move |a: &mut App| {
                a.core.lib.move_track(pid, pos, pos.saturating_sub(1));
            })
            .child(text("↑").key("label").font(type_::BODY).color(t.text)),
    );
    actions = actions.child(
        el().key(format!("down-{}-{}", pid.0, pos)).label("Move down")
            .size(28.0, 28.0).radius(radius::SM).center().hover(t.hover).focusable(true)
            .on_press(move |a: &mut App| {
                let target = if pos + 1 < len { pos + 1 } else { pos };
                a.core.lib.move_track(pid, pos, target);
            })
            .child(text("↓").key("label").font(type_::BODY).color(t.text)),
    );
    actions = actions.child(
        el().key(format!("rm-{}-{}", pid.0, pos)).label("Remove")
            .h(28.0).px(space::S2).radius(radius::SM).center_y().hover(t.hover).focusable(true)
            .on_press(move |a: &mut App| {
                a.core.lib.remove_track(pid, tr.id);
            })
            .child(text("Remove").key("label").font(type_::LABEL).color(t.text_secondary)),
    );

    row().key(format!("track-{}", tr.id.0))
        .checked(selected)
        .fill(if selected { t.hover } else { Color::TRANSPARENT })
        .focusable(true)
        .on_press(move |a: &mut App| state::select_track(&mut a.core, tr.id))
        .h(44.0)
        .px(space::S2)
        .radius(radius::MD)
        .center_y()
        .gap(space::S3)
        .hover(t.hover)
        .child(
            col().grow(1.0).gap(2.0)
                .child(text(tr.title).key("title").font(type_::BODY).color(t.text))
                .child(text(catalog::artist(tr.artist).name).key("track-artist").font(type_::LABEL).color(t.text_secondary)),
        )
        .child(text(tr.duration).key("duration").font(type_::LABEL).color(t.text_secondary))
        .child(actions)
}

// --- Page scaffolding ----------------------------------------------------

fn header(title: &str, subtitle: &str, t: tok::Theme) -> El {
    col().px(space::S6).pt(space::S6).pb(space::S3).gap(space::S1)
        .child(text(title).key("title").font(type_::TITLE).color(t.text))
        .child(text(subtitle).key("subtitle").font(type_::BODY).color(t.text_secondary))
}

/// A full page: a header over a scrollable body.
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

fn all_albums_grid(t: tok::Theme) -> El {
    let cards: Vec<El> = catalog::ALBUMS.iter().map(|a| album_card(*a, t)).collect();
    grid(cards, 5)
}

fn all_artists_grid(t: tok::Theme) -> El {
    let cards: Vec<El> = catalog::ARTISTS.iter().map(|a| artist_card(*a, t)).collect();
    grid(cards, 6)
}

fn all_tracks() -> Vec<Track> {
    catalog::TRACKS.to_vec()
}

// --- Section screens -----------------------------------------------------

pub fn home(t: tok::Theme) -> El {
    let rec: Vec<El> = catalog::ALBUMS[0..5].iter().map(|a| album_card(*a, t)).collect();
    let edits: Vec<El> = catalog::EDITORIAL_PLAYLISTS.iter().map(|p| editorial_card(p.id, t)).collect();
    page("Home", "Your music, your way", t,
        col().gap(space::S6)
            .child(shelf("Recommended for you", t, rec))
            .child(shelf("Editor's picks", t, edits)))
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
        PopularTab::Artists => all_artists_grid(t),
        PopularTab::Albums => all_albums_grid(t),
        PopularTab::Tracks => track_list(all_tracks(), s.now_playing, t),
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
    let a: Vec<El> = catalog::ALBUMS.iter().take(5).map(|x| album_card(*x, t)).collect();
    let b: Vec<El> = catalog::ALBUMS.iter().skip(5).map(|x| album_card(*x, t)).collect();
    page("For You", "Built from what you play", t,
        col().gap(space::S6)
            .child(shelf("New releases for you", t, a))
            .child(shelf("From your genres", t, b)))
}

pub fn liked_songs(s: &State, t: tok::Theme) -> El {
    let tracks: Vec<Track> = catalog::TRACKS.iter().filter(|tr| s.lib.is_liked(tr.id)).copied().collect();
    let body = if tracks.is_empty() {
        empty("Nothing liked yet. Tap the heart on the now-playing bar.", t)
    } else {
        track_list(tracks, s.now_playing, t)
    };
    page("Liked Songs", "Tracks you liked", t, body)
}

pub fn albums(s: &State, t: tok::Theme) -> El {
    let saved: Vec<Album> = catalog::ALBUMS.iter().filter(|a| s.lib.is_saved_album(a.id)).copied().collect();
    let body = if saved.is_empty() {
        empty("No saved albums yet. Open an album and tap Save.", t)
    } else {
        grid(saved.into_iter().map(|a| album_card(a, t)).collect(), 5)
    };
    page("Albums", "Albums you saved", t, body)
}

pub fn artists(s: &State, t: tok::Theme) -> El {
    let followed: Vec<Artist> = catalog::ARTISTS.iter().filter(|a| s.lib.is_following(a.id)).copied().collect();
    let body = if followed.is_empty() {
        empty("No artists yet. Open an artist and tap Follow.", t)
    } else {
        grid(followed.into_iter().map(|a| artist_card(a, t)).collect(), 6)
    };
    page("Artists", "Artists you follow", t, body)
}

pub fn playlists(s: &State, t: tok::Theme) -> El {
    let new_btn = el().key("new-playlist").label("New playlist")
        .h(40.0).px(space::S3).radius(radius::MD).center_y().focusable(true)
        .fill(t.text)
        .on_press(|a: &mut App| {
            let n = a.core.lib.playlists.iter().filter(|p| p.is_some()).count() + 1;
            a.core.lib.create_playlist(format!("Playlist {n}"));
        })
        .child(text("New playlist").key("label").font(type_::BODY).color(t.bg));
    let cards: Vec<El> = s.lib.playlists.iter()
        .enumerate()
        .filter_map(|(i, p)| p.as_ref().map(|p| (PlaylistId(i as u8), p.name.clone())))
        .map(|(id, name)| playlist_card(id, &name, t))
        .collect();
    let body = if cards.is_empty() {
        col().gap(space::S3).child(new_btn).child(empty("No playlists yet. Create one.", t))
    } else {
        col().gap(space::S3).child(new_btn).child(grid(cards, 5))
    };
    page("Playlists", "Your playlists", t, body)
}

pub fn collections(s: &State, t: tok::Theme) -> El {
    let new_btn = el().key("new-collection").label("New collection")
        .h(40.0).px(space::S3).radius(radius::MD).center_y().focusable(true)
        .fill(t.text)
        .on_press(|a: &mut App| {
            let n = a.core.lib.collections.len() + 1;
            a.core.lib.create_collection(format!("Collection {n}"));
        })
        .child(text("New collection").key("label").font(type_::BODY).color(t.bg));

    let mut body = col().gap(space::S3).child(new_btn);
    if s.lib.collections.is_empty() {
        body = body.child(empty("No collections yet. Create one.", t));
    }
    for (idx, c) in s.lib.collections.iter().enumerate() {
        let mut block = col().gap(space::S1);
        let mut head = row().center_y().gap(space::S2);
        head = head
            .child(text(&c.name).key("col-name").font(type_::SECTION).color(t.text).grow(1.0))
            .child(el().key(format!("add-to-col-{idx}")).label("Add playlist")
                .h(28.0).px(space::S2).radius(radius::SM).center_y().hover(t.hover).focusable(true)
                .on_press(move |a: &mut App| {
                    for i in 0..library::MAX_PLAYLISTS {
                        let pid = PlaylistId(i as u8);
                        let in_col = a.core.lib.collections.get(idx)
                            .map(|c| c.playlists.contains(&pid)).unwrap_or(false);
                        if a.core.lib.playlist(pid).is_some() && !in_col {
                            a.core.lib.add_to_collection(idx, pid);
                            break;
                        }
                    }
                })
                .child(text("Add playlist").key("label").font(type_::LABEL).color(t.text_secondary)))
            .child(el().key(format!("del-col-{idx}")).label("Delete")
                .h(28.0).px(space::S2).radius(radius::SM).center_y().hover(t.hover).focusable(true)
                .on_press(move |a: &mut App| {
                    a.core.lib.delete_collection(idx);
                })
                .child(text("Delete").key("label").font(type_::LABEL).color(t.text_secondary)));
        block = block.child(head);
        for (pos, &pid) in c.playlists.iter().enumerate() {
            let name = s.lib.playlist(pid).map(|p| p.name.clone()).unwrap_or_else(|| "Playlist".into());
            block = block.child(
                row().key(format!("col-item-{idx}-{pos}")).h(36.0).px(space::S2).radius(radius::MD).center_y().gap(space::S2).hover(t.hover)
                    .child(text(name).key("name").font(type_::BODY).color(t.text).grow(1.0))
                    .child(el().key(format!("rm-from-col-{idx}-{pos}")).label("Remove")
                        .h(24.0).px(space::S2).radius(radius::SM).center_y().hover(t.hover).focusable(true)
                        .on_press(move |a: &mut App| {
                            a.core.lib.remove_from_collection(idx, pid);
                        })
                        .child(text("Remove").key("label").font(type_::LABEL).color(t.text_secondary))),
            );
        }
        body = body.child(block);
    }
    page("Collections", "Folders of playlists", t, body)
}

pub fn local(s: &State, t: tok::Theme) -> El {
    let mut tracks: Vec<Track> = Vec::new();
    for a in catalog::ALBUMS {
        if s.lib.is_downloaded(a.id) {
            tracks.extend(catalog::tracks_of_album(a.id));
        }
    }
    let body = if tracks.is_empty() {
        empty("Nothing on this device. Download an album first.", t)
    } else {
        track_list(tracks, s.now_playing, t)
    };
    page("On this device", "Music you downloaded", t, body)
}

pub fn history(s: &State, t: tok::Theme) -> El {
    let tracks: Vec<Track> = s.lib.history.iter().map(|&id| catalog::track(id)).collect();
    let body = if tracks.is_empty() {
        empty("Nothing played yet.", t)
    } else {
        track_list(tracks, s.now_playing, t)
    };
    page("History", "Recently played", t, body)
}

pub fn stats(s: &State, t: tok::Theme) -> El {
    let (top_artist, top_track) = top_artist_and_track(s);
    let playlists = s.lib.playlists.iter().filter(|p| p.is_some()).count();
    let stats: [(&str, String); 6] = [
        ("Liked tracks", s.lib.liked.count_ones().to_string()),
        ("Saved albums", s.lib.saved_albums.count_ones().to_string()),
        ("Followed artists", s.lib.followed.count_ones().to_string()),
        ("Playlists", playlists.to_string()),
        ("Top artist", top_artist),
        ("Top track", top_track),
    ];
    let mut g = row().gap(space::S3);
    for (label, value) in stats {
        g = g.child(
            col().key(format!("stat-{label}")).w(200.0).px(space::S3).pt(space::S3).pb(space::S3).gap(space::S1)
                .fill(t.card).radius(radius::MD)
                .child(text(label).key("label").font(type_::LABEL).color(t.text_secondary))
                .child(text(value).key("value").font(type_::TITLE).color(t.text)),
        );
    }
    page("Stats", "Your library at a glance", t, g)
}

fn top_artist_and_track(s: &State) -> (String, String) {
    use std::collections::HashMap;
    let mut artists: HashMap<catalog::ArtistId, usize> = HashMap::new();
    let mut tracks: HashMap<TrackId, usize> = HashMap::new();
    for &id in &s.lib.history {
        let tr = catalog::track(id);
        *artists.entry(tr.artist).or_insert(0) += 1;
        *tracks.entry(id).or_insert(0) += 1;
    }
    let top_artist = artists.into_iter().max_by_key(|&(_, c)| c)
        .map(|(a, _)| catalog::artist(a).name.to_string())
        .unwrap_or_else(|| "—".into());
    let top_track = tracks.into_iter().max_by_key(|&(_, c)| c)
        .map(|(id, _)| catalog::track(id).title.to_string())
        .unwrap_or_else(|| "—".into());
    (top_artist, top_track)
}

pub fn downloads(s: &State, t: tok::Theme) -> El {
    let downloaded: Vec<Album> = catalog::ALBUMS.iter().filter(|a| s.lib.is_downloaded(a.id)).copied().collect();
    let mut rows = col().gap(space::S1);
    if downloaded.is_empty() {
        rows = rows.child(empty("No downloads yet. Open an album and tap Download.", t));
    }
    for a in downloaded {
        let id = a.id;
        rows = rows.child(
            row().key(format!("dl-{}", a.id.0)).h(44.0).px(space::S2).radius(radius::MD).center_y().gap(space::S3).hover(t.hover)
                .child(text(a.title).key("title").font(type_::BODY).color(t.text).grow(1.0))
                .child(text(catalog::artist(a.artist).name).key("artist").font(type_::LABEL).color(t.text_secondary))
                .child(text("Done").key("state").font(type_::LABEL).color(t.text_secondary))
                .child(el().key(format!("rm-dl-{}", a.id.0)).label("Remove")
                    .h(28.0).px(space::S2).radius(radius::SM).center_y().hover(t.hover).focusable(true)
                    .on_press(move |app: &mut App| {
                        app.core.lib.remove_download(id);
                    })
                    .child(text("Remove").key("label").font(type_::LABEL).color(t.text_secondary))),
        );
    }
    page("Downloads", "Your download queue", t, rows)
}

pub fn search(t: tok::Theme, edit: &TextEdit) -> El {
    let query = edit.text.trim();
    let results = if query.is_empty() {
        all_albums_grid(t)
    } else {
        let q = query.to_lowercase();
        let albums: Vec<El> = catalog::ALBUMS.iter()
            .filter(|a| a.title.to_lowercase().contains(&q) || catalog::artist(a.artist).name.to_lowercase().contains(&q))
            .map(|a| album_card(*a, t)).collect();
        let artists: Vec<El> = catalog::ARTISTS.iter()
            .filter(|a| a.name.to_lowercase().contains(&q))
            .map(|a| artist_card(*a, t)).collect();
        let tracks: Vec<El> = catalog::TRACKS.iter()
            .filter(|tr| tr.title.to_lowercase().contains(&q) || catalog::artist(tr.artist).name.to_lowercase().contains(&q))
            .map(|tr| track_row(*tr, false, t)).collect();
        if albums.is_empty() && artists.is_empty() && tracks.is_empty() {
            text(format!("No results for \"{query}\""))
                .key("search-empty").font(type_::BODY).color(t.text_secondary)
        } else {
            let mut results = col().gap(space::S3);
            if !albums.is_empty() {
                results = results
                    .child(text("Albums").key("result-Albums").font(type_::SECTION).color(t.text))
                    .child(grid(albums, 5));
            }
            if !artists.is_empty() {
                results = results
                    .child(text("Artists").key("result-Artists").font(type_::SECTION).color(t.text))
                    .child(grid(artists, 6));
            }
            if !tracks.is_empty() {
                let mut list = col().gap(space::S1);
                for tr in tracks {
                    list = list.child(tr);
                }
                results = results
                    .child(text("Tracks").key("result-Tracks").font(type_::SECTION).color(t.text))
                    .child(list);
            }
            results
        }
    };
    let field = text_input(edit, type_::BODY)
        .key("search-box")
        .w_full()
        .px(space::S3)
        .py(space::S2)
        .radius(radius::MD)
        .fill(t.hover)
        .single_line()
        .on_edit(|a: &mut App| &mut a.search)
        .placeholder("Search artists, albums, tracks", t.text_secondary);
    page("Search", "Find anything in the catalogue", t,
        col().gap(space::S3).child(field).child(results))
}

pub fn settings(t: tok::Theme) -> El {
    let mut rows = col().gap(space::S1);
    let items: [SettingsPage; 7] = [
        SettingsPage::Playback,
        SettingsPage::Quality,
        SettingsPage::Downloads,
        SettingsPage::Notifications,
        SettingsPage::Import,
        SettingsPage::Account,
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

fn settings_row(key: String, label: &str, value: &str, t: tok::Theme) -> El {
    row().key(key).h(44.0).px(space::S3).radius(radius::MD).center_y().justify_between()
        .child(text(label).key("label").font(type_::BODY).color(t.text))
        .child(text(value).key("value").font(type_::BODY).color(t.text_secondary))
}

pub fn settings_page(p: SettingsPage, app: &App, t: tok::Theme) -> El {
    match p {
        SettingsPage::Account => account_page(app, t),
        _ => {
            let items: &[(&str, &str)] = match p {
                SettingsPage::Playback => &[("Crossfade", "Off"), ("Gapless playback", "On"), ("Normalize volume", "On")],
                SettingsPage::Quality => &[("Streaming quality", "High"), ("Download quality", "High")],
                SettingsPage::Downloads => &[("Download location", "On this device"), ("Wi-Fi only", "On")],
                SettingsPage::Notifications => &[("New releases", "On"), ("Concert alerts", "Off")],
                SettingsPage::Import => &[("Import from Spotify", ">"), ("Import from Last.fm", ">")],
                SettingsPage::About => &[("Version", "0.1.0"), ("OpenTidal", "")],
                SettingsPage::Account => &[],
            };
            let mut rows = col().gap(space::S1);
            for (i, (label, value)) in items.iter().enumerate() {
                rows = rows.child(settings_row(format!("opt-{i}"), label, value, t));
            }
            page(p.label(), "Settings", t, rows)
        }
    }
}

fn account_page(app: &App, t: tok::Theme) -> El {
    let name = app.core.lib.name.clone();
    let field = text_input(&app.rename, type_::BODY)
        .key("name-box")
        .w_full()
        .px(space::S3)
        .py(space::S2)
        .radius(radius::MD)
        .fill(t.hover)
        .single_line()
        .on_edit(|a: &mut App| &mut a.rename)
        .placeholder("Your name", t.text_secondary);
    let save_btn = el().key("save-name").label("Save name")
        .h(40.0).px(space::S3).radius(radius::MD).center_y().focusable(true)
        .fill(t.text)
        .on_press(|a: &mut App| {
            let name = a.rename.text.trim().to_string();
            if !name.is_empty() {
                a.core.lib.set_name(name);
                a.rename.clear();
            }
        })
        .child(text("Save name").key("label").font(type_::BODY).color(t.bg));
    page("Account", "Your profile", t,
        col().gap(space::S3)
            .child(settings_row("acct-name".into(), "Name", &name, t))
            .child(field)
            .child(save_btn))
}

// --- Detail screens ------------------------------------------------------

fn action_button<F: Fn(&mut App) + 'static>(key: &str, label: &str, accent: Color, filled: bool, t: tok::Theme, f: F) -> El {
    el().key(key)
        .label(label)
        .h(40.0)
        .px(space::S3)
        .radius(radius::MD)
        .center_y()
        .fill(if filled { accent } else { t.hover })
        .focusable(true)
        .on_press(f)
        .child(text(label).key("label").font(type_::BODY).color(if filled { t.bg } else { t.text }))
}

fn hero(cover_el: El, title: &str, subtitle: &str, t: tok::Theme, actions: Vec<El>) -> El {
    let mut btns = row().gap(space::S2).pt(space::S3);
    for b in actions {
        btns = btns.child(b);
    }
    row().px(space::S6).pt(space::S6).pb(space::S3).gap(space::S6)
        .child(cover_el)
        .child(
            col().gap(space::S2)
                .child(text(title).key("hero-title").font(type_::TITLE).color(t.text))
                .child(text(subtitle).key("hero-subtitle").font(type_::BODY).color(t.text_secondary))
                .child(btns),
        )
}

fn detail_body(body: El) -> El {
    col().key("scroll")
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false })
        .px(space::S6)
        .pb(space::S6)
        .child(body)
}

fn cover_el(seed: usize, size: f32, round: f32, t: tok::Theme) -> El {
    let (top, bottom) = cover(seed);
    el().key("hero-cover").size(size, size).radius(round).gradient(top, bottom).shadow(2.0, 12.0, t.shadow_soft)
}

/// A detail screen. The route variant selects the subject; the back affordance
/// lives in the top bar, not here.
pub fn detail(route: Route, app: &App, accent: Color, t: tok::Theme) -> El {
    let s = &app.core;
    match route {
        Route::Album(id) => album_detail(id, s, accent, t),
        Route::Artist(id) => artist_detail(id, s, accent, t),
        Route::EditorialPlaylist(id) => editorial_detail(id, s, accent, t),
        Route::UserPlaylist(id) => user_playlist_detail(id, app, accent, t),
        _ => page("Detail", "", t, el()),
    }
}

fn album_detail(id: catalog::AlbumId, s: &State, accent: Color, t: tok::Theme) -> El {
    let a = catalog::album(id);
    let saved = s.lib.is_saved_album(id);
    let downloaded = s.lib.is_downloaded(id);
    let artist = catalog::artist(a.artist).name;
    let tracks: Vec<Track> = catalog::tracks_of_album(id).collect();

    let actions = vec![
        action_button("play", "Play", accent, true, t, move |a: &mut App| {
            if let Some(first) = catalog::tracks_of_album(id).next() {
                state::select_track(&mut a.core, first.id);
            }
        }),
        action_button("save", if saved { "Remove" } else { "Save" }, accent, false, t, move |a: &mut App| {
            a.core.lib.toggle_save_album(id);
        }),
        action_button("download", if downloaded { "Remove download" } else { "Download" }, accent, false, t, move |a: &mut App| {
            a.core.lib.toggle_download(id);
        }),
    ];

    col().grow(1.0).min_h(0.0)
        .child(hero(cover_el(id.0 as usize, 200.0, radius::MD, t), a.title, &format!("Album by {artist}"), t, actions))
        .child(detail_body(track_list(tracks, s.now_playing, t)))
}

fn artist_detail(id: catalog::ArtistId, s: &State, accent: Color, t: tok::Theme) -> El {
    let a = catalog::artist(id);
    let following = s.lib.is_following(id);
    let tracks: Vec<Track> = catalog::tracks_of_artist(id).collect();

    let actions = vec![
        action_button("play", "Play", accent, true, t, move |a: &mut App| {
            if let Some(first) = catalog::tracks_of_artist(id).next() {
                state::select_track(&mut a.core, first.id);
            }
        }),
        action_button("follow", if following { "Following" } else { "Follow" }, accent, false, t, move |a: &mut App| {
            a.core.lib.toggle_follow(id);
        }),
    ];

    col().grow(1.0).min_h(0.0)
        .child(hero(cover_el(id.0 as usize + 3, 200.0, 100.0, t), a.name, "Artist", t, actions))
        .child(detail_body(track_list(tracks, s.now_playing, t)))
}

fn editorial_detail(id: EditorialPlaylistId, s: &State, accent: Color, t: tok::Theme) -> El {
    let p = catalog::editorial_playlist(id);
    let tracks: Vec<Track> = p.tracks.iter().map(|&tid| catalog::track(tid)).collect();

    let actions = vec![
        action_button("play", "Play", accent, true, t, move |a: &mut App| {
            if let Some(&first) = catalog::editorial_playlist(id).tracks.first() {
                state::select_track(&mut a.core, first);
            }
        }),
    ];

    col().grow(1.0).min_h(0.0)
        .child(hero(cover_el(id.0 as usize + 6, 200.0, radius::MD, t), p.name, p.description, t, actions))
        .child(detail_body(track_list(tracks, s.now_playing, t)))
}

fn user_playlist_detail(id: PlaylistId, app: &App, accent: Color, t: tok::Theme) -> El {
    let s = &app.core;
    let Some(p) = s.lib.playlist(id) else {
        return page("Playlist", "", t, el());
    };
    let name = p.name.clone();
    let tracks: Vec<Track> = p.tracks.iter().map(|&tid| catalog::track(tid)).collect();
    let len = tracks.len();

    let rename_field = text_input(&app.rename, type_::BODY)
        .key("rename-box")
        .w_full()
        .px(space::S3)
        .py(space::S2)
        .radius(radius::MD)
        .fill(t.hover)
        .single_line()
        .on_edit(|a: &mut App| &mut a.rename)
        .placeholder("Rename this playlist", t.text_secondary);

    let actions = vec![
        action_button("add", "Add playing track", accent, true, t, move |a: &mut App| {
            if let Some(tr) = a.core.now_playing {
                a.core.lib.add_track(id, tr);
            }
        }),
        action_button("delete-playlist", "Delete", accent, false, t, move |a: &mut App| {
            state::delete_playlist(&mut a.core, id);
        }),
    ];

    let rename_btn = el().key("rename-playlist").label("Rename")
        .h(32.0).px(space::S3).radius(radius::MD).center_y().focusable(true)
        .fill(t.text)
        .on_press(move |a: &mut App| {
            let name = a.rename.text.trim().to_string();
            if !name.is_empty() {
                a.core.lib.rename_playlist(id, name);
                a.rename.clear();
            }
        })
        .child(text("Rename").key("label").font(type_::BODY).color(t.bg));

    let mut body = col().gap(space::S3).child(rename_field).child(rename_btn);
    if tracks.is_empty() {
        body = body.child(empty("No tracks yet. Select a track and tap Add playing track.", t));
    } else {
        let mut list = col().key("tracks").gap(space::S1);
        for (pos, tr) in tracks.iter().enumerate() {
            list = list.child(playlist_track_row(id, pos, len, *tr, s.now_playing == Some(tr.id), t));
        }
        body = body.child(list);
    }

    col().grow(1.0).min_h(0.0)
        .child(hero(cover_el(id.0 as usize + 10, 200.0, radius::MD, t), &name, "Your playlist", t, actions))
        .child(detail_body(body))
}
