//! The section and detail screens, built from shared card, shelf, grid, and
//! track-row helpers over the catalogue and the user's library. The library is
//! real state now: every screen reads it and every action mutates it through
//! the proven operations.

use rune::tok::{self, radius, space, type_};
use rune::{Color, El, ScrollSpec, TextEdit, col, el, oklch, row, text, text_input};
use crate::app::{App, Cover, Covers};
use crate::catalogue::{Album, AlbumId, Artist, ArtistId, Catalogue, CataloguePlaylist, CataloguePlaylistId, Track, TrackId};
use crate::library::{self, PlaylistId};
use crate::state::{self, PopularTab, Route, SettingsPage, State};

/// A colourful vertical gradient, the placeholder for an item with no cover.
fn placeholder(i: usize) -> (Color, Color) {
    let hue = (i as f32 * 37.0 + 14.0) % 360.0;
    (oklch(0.62, 0.16, hue), oklch(0.34, 0.15, hue))
}

/// A placeholder seed from an id, so an item has one colour on every screen
/// and across restarts. FNV-1a, because the std hasher may change between
/// releases.
fn placeholder_seed(id: &str) -> usize {
    let hash = id.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    (hash % 360) as usize
}

/// A duration in seconds as the screen shows it, such as `3:26`.
fn clock(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// An artist's name, or nothing for an id the catalogue lacks, which the
/// invariant says never happens.
fn artist_name<'a>(cat: &'a Catalogue, id: &ArtistId) -> &'a str {
    cat.artist(id).map_or("", |a| a.name.as_str())
}

// --- Cards ---------------------------------------------------------------

/// The placeholder gradient keyed `key`.
fn placeholder_art(key: &str, seed: usize, size: f32, round: f32, t: tok::Theme) -> El {
    let (top, bottom) = placeholder(seed);
    el().key(key).size(size, size).radius(round).gradient(top, bottom).shadow(2.0, 12.0, t.shadow_soft)
}

/// The cover when its pixels decoded with no error, keyed `<key>-image`, else
/// the placeholder keyed `key`. A tree assertion on `cover-image` therefore
/// cannot pass on a placeholder or on an image still decoding.
fn cover_art(key: &str, cover: &Cover, size: f32, round: f32, t: tok::Theme) -> El {
    match cover.ready() {
        Some(image) => el()
            .key(format!("{key}-image"))
            .size(size, size)
            .radius(round)
            .pixels(image.pixels())
            .shadow(2.0, 12.0, t.shadow_soft),
        None => placeholder_art(key, placeholder_seed(cover.seed()), size, round, t),
    }
}

/// A square media card that drills into a detail route on press.
fn card(key: &str, art: El, title: &str, subtitle: &str, route: Route, t: tok::Theme) -> El {
    col().key(key)
        .w(168.0)
        .gap(space::S2)
        .on_press(move |a: &mut App| {
            a.rename.clear();
            state::open_detail(&mut a.core, &a.catalogue, route.clone())
        })
        .child(art)
        .child(text(title).key("title").font(type_::BODY).color(t.text))
        .child(text(subtitle).key("subtitle").font(type_::LABEL).color(t.text_secondary))
}

/// Keys are positions on the screen, so a script taps the same visible card
/// whatever the catalogue holds.
fn album_card(pos: usize, a: &Album, cat: &Catalogue, covers: Covers<'_>, t: tok::Theme) -> El {
    let art = cover_art("cover", &covers.album(a), 168.0, radius::MD, t);
    card(&format!("album-{pos}"), art, &a.title, artist_name(cat, &a.artist), Route::Album(a.id.clone()), t)
}

fn artist_card(pos: usize, a: &Artist, t: tok::Theme) -> El {
    let id = a.id.clone();
    col().key(format!("artist-{pos}"))
        .w(120.0)
        .gap(space::S2)
        .on_press(move |app: &mut App| {
            app.rename.clear();
            state::open_detail(&mut app.core, &app.catalogue, Route::Artist(id.clone()))
        })
        .child(placeholder_art("cover", placeholder_seed(a.id.as_str()), 120.0, 60.0, t))
        .child(text(&a.name).key("name").font(type_::BODY).color(t.text).center())
}

fn catalogue_playlist_card(key: String, p: &CataloguePlaylist, subtitle: &str, covers: Covers<'_>, t: tok::Theme) -> El {
    let art = cover_art("cover", &covers.playlist(p), 168.0, radius::MD, t);
    card(&key, art, &p.name, subtitle, Route::CataloguePlaylist(p.id.clone()), t)
}

fn playlist_card(id: PlaylistId, name: &str, t: tok::Theme) -> El {
    let art = placeholder_art("cover", id.0 as usize + 10, 168.0, radius::MD, t);
    card(&format!("playlist-{}", id.0), art, name, "Your playlist", Route::UserPlaylist(id), t)
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

fn track_row(pos: usize, tr: &Track, selected: bool, cat: &Catalogue, t: tok::Theme) -> El {
    let id = tr.id.clone();
    row().key(format!("track-{pos}"))
        .checked(selected)
        .fill(if selected { t.hover } else { Color::TRANSPARENT })
        .focusable(true)
        .on_press(move |a: &mut App| state::select_track(&mut a.core, &a.catalogue, id.clone()))
        .h(44.0)
        .px(space::S2)
        .radius(radius::MD)
        .center_y()
        .gap(space::S3)
        .hover(t.hover)
        .child(
            col().grow(1.0).gap(2.0)
                .child(text(&tr.title).key("title").font(type_::BODY).color(t.text))
                .child(text(artist_name(cat, &tr.artist)).key("track-artist").font(type_::LABEL).color(t.text_secondary)),
        )
        .child(text(clock(tr.duration)).key("duration").font(type_::LABEL).color(t.text_secondary))
}

fn track_list(tracks: Vec<&Track>, selected: Option<&TrackId>, cat: &Catalogue, t: tok::Theme) -> El {
    let mut l = col().key("tracks").gap(space::S1);
    for (pos, tr) in tracks.into_iter().enumerate() {
        l = l.child(track_row(pos, tr, selected == Some(&tr.id), cat, t));
    }
    l
}

fn empty(msg: &str, t: tok::Theme) -> El {
    text(msg).key("empty").font(type_::BODY).color(t.text_secondary)
}

/// A track row inside a user playlist: selectable, with Remove, Up and Down.
fn playlist_track_row(pid: PlaylistId, pos: usize, len: usize, tr: &Track, selected: bool, cat: &Catalogue, t: tok::Theme) -> El {
    let id = tr.id.clone();
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
            .on_press({
                let id = id.clone();
                move |a: &mut App| {
                    a.core.lib.remove_track(pid, &id);
                }
            })
            .child(text("Remove").key("label").font(type_::LABEL).color(t.text_secondary)),
    );

    row().key(format!("track-{pos}"))
        .checked(selected)
        .fill(if selected { t.hover } else { Color::TRANSPARENT })
        .focusable(true)
        .on_press(move |a: &mut App| state::select_track(&mut a.core, &a.catalogue, id.clone()))
        .h(44.0)
        .px(space::S2)
        .radius(radius::MD)
        .center_y()
        .gap(space::S3)
        .hover(t.hover)
        .child(
            col().grow(1.0).gap(2.0)
                .child(text(&tr.title).key("title").font(type_::BODY).color(t.text))
                .child(text(artist_name(cat, &tr.artist)).key("track-artist").font(type_::LABEL).color(t.text_secondary)),
        )
        .child(text(clock(tr.duration)).key("duration").font(type_::LABEL).color(t.text_secondary))
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

fn all_albums_grid(cat: &Catalogue, covers: Covers<'_>, t: tok::Theme) -> El {
    let cards: Vec<El> = cat.albums().enumerate().map(|(i, a)| album_card(i, a, cat, covers, t)).collect();
    grid(cards, 5)
}

fn all_artists_grid(cat: &Catalogue, t: tok::Theme) -> El {
    let cards: Vec<El> = cat.artists().enumerate().map(|(i, a)| artist_card(i, a, t)).collect();
    grid(cards, 6)
}

// --- Section screens -----------------------------------------------------

/// The playlists the Home shelf shows: the seed's only. The catalogue holds the
/// Tidal playlists of every account that synced in this run or wrote to this
/// file, so listing them all would show one account's private names to another.
pub fn home_playlists(cat: &Catalogue) -> impl Iterator<Item = &CataloguePlaylist> {
    cat.seed_playlists()
}

pub fn home(cat: &Catalogue, covers: Covers<'_>, t: tok::Theme) -> El {
    let rec: Vec<El> = cat.albums().take(5).enumerate().map(|(i, a)| album_card(i, a, cat, covers, t)).collect();
    let edits: Vec<El> = home_playlists(cat)
        .enumerate()
        .map(|(i, p)| catalogue_playlist_card(format!("seed-playlist-{i}"), p, &p.description, covers, t))
        .collect();
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

pub fn popular(s: &State, cat: &Catalogue, covers: Covers<'_>, t: tok::Theme) -> El {
    let tabs = row().gap(space::S2).pb(space::S3)
        .child(tab("Artists", PopularTab::Artists, s, t))
        .child(tab("Albums", PopularTab::Albums, s, t))
        .child(tab("Tracks", PopularTab::Tracks, s, t));
    let body = match s.popular_tab {
        PopularTab::Artists => all_artists_grid(cat, t),
        PopularTab::Albums => all_albums_grid(cat, covers, t),
        PopularTab::Tracks => track_list(cat.tracks().collect(), s.now_playing.as_ref(), cat, t),
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

pub fn for_you(cat: &Catalogue, covers: Covers<'_>, t: tok::Theme) -> El {
    // One position count across both shelves, so no two cards share a key.
    let mut cards: Vec<El> = cat.albums().enumerate().map(|(i, x)| album_card(i, x, cat, covers, t)).collect();
    let b = cards.split_off(cards.len().min(5));
    let a = cards;
    page("For You", "Built from what you play", t,
        col().gap(space::S6)
            .child(shelf("New releases for you", t, a))
            .child(shelf("From your genres", t, b)))
}

pub fn liked_songs(s: &State, cat: &Catalogue, t: tok::Theme) -> El {
    let tracks: Vec<&Track> = cat.tracks().filter(|tr| s.lib.is_liked(&tr.id)).collect();
    let body = if tracks.is_empty() {
        empty("Nothing liked yet. Tap the heart on the now-playing bar.", t)
    } else {
        track_list(tracks, s.now_playing.as_ref(), cat, t)
    };
    page("Liked Songs", "Tracks you liked", t, body)
}

pub fn albums(s: &State, cat: &Catalogue, covers: Covers<'_>, t: tok::Theme) -> El {
    let saved: Vec<&Album> = cat.albums().filter(|a| s.lib.is_saved_album(&a.id)).collect();
    let body = if saved.is_empty() {
        empty("No saved albums yet. Open an album and tap Save.", t)
    } else {
        grid(saved.into_iter().enumerate().map(|(i, a)| album_card(i, a, cat, covers, t)).collect(), 5)
    };
    page("Albums", "Albums you saved", t, body)
}

pub fn artists(s: &State, cat: &Catalogue, t: tok::Theme) -> El {
    let followed: Vec<&Artist> = cat.artists().filter(|a| s.lib.is_following(&a.id)).collect();
    let body = if followed.is_empty() {
        empty("No artists yet. Open an artist and tap Follow.", t)
    } else {
        grid(followed.into_iter().enumerate().map(|(i, a)| artist_card(i, a, t)).collect(), 6)
    };
    page("Artists", "Artists you follow", t, body)
}

pub fn playlists(s: &State, cat: &Catalogue, covers: Covers<'_>, t: tok::Theme) -> El {
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
    let mut body = if cards.is_empty() {
        col().gap(space::S3).child(new_btn).child(empty("No playlists yet. Create one.", t))
    } else {
        col().gap(space::S3).child(new_btn).child(grid(cards, 5))
    };
    // Tidal playlists open the read-only catalogue route, so no edit action can
    // reach one. Only this account's memberships are listed.
    let tidal: Vec<El> = s.lib.tidal_playlists.iter()
        .filter_map(|id| cat.playlist(id))
        .enumerate()
        .map(|(n, p)| catalogue_playlist_card(format!("tidal-playlist-{n}"), p, "From Tidal", covers, t))
        .collect();
    if !tidal.is_empty() {
        body = body
            .child(text("From Tidal").key("tidal-playlists-title").font(type_::SECTION).color(t.text))
            .child(grid(tidal, 5));
    }
    page("Playlists", "Your playlists", t, body)
}

pub fn collections(app: &App, t: tok::Theme) -> El {
    let s = &app.core;
    let new_btn = el().key("new-collection").label("New collection")
        .h(40.0).px(space::S3).radius(radius::MD).center_y().focusable(true)
        .fill(t.text)
        .on_press(|a: &mut App| {
            let n = a.core.lib.collections.len() + 1;
            a.core.lib.create_collection(format!("Collection {n}"));
        })
        .child(text("New collection").key("label").font(type_::BODY).color(t.bg));

    let rename_field = text_input(&app.rename, type_::BODY)
        .key("col-rename-box")
        .w_full()
        .px(space::S3)
        .py(space::S2)
        .radius(radius::MD)
        .fill(t.hover)
        .single_line()
        .on_edit(|a: &mut App| &mut a.rename)
        .placeholder("Rename a collection", t.text_secondary);

    let mut body = col().gap(space::S3).child(new_btn);
    if s.lib.collections.is_empty() {
        body = body.child(empty("No collections yet. Create one.", t));
    } else {
        body = body.child(rename_field);
    }
    for (idx, c) in s.lib.collections.iter().enumerate() {
        let mut block = col().gap(space::S1);
        let mut head = row().center_y().gap(space::S2);
        head = head
            .child(text(&c.name).key("col-name").font(type_::SECTION).color(t.text).grow(1.0))
            .child(el().key(format!("ren-col-{idx}")).label("Rename")
                .h(28.0).px(space::S2).radius(radius::SM).center_y().hover(t.hover).focusable(true)
                .on_press(move |a: &mut App| {
                    let name = a.rename.text.trim().to_string();
                    if !name.is_empty() {
                        a.core.lib.rename_collection(idx, name);
                        a.rename.clear();
                    }
                })
                .child(text("Rename").key("label").font(type_::LABEL).color(t.text_secondary)))
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

pub fn local(s: &State, cat: &Catalogue, t: tok::Theme) -> El {
    let mut tracks: Vec<&Track> = Vec::new();
    for a in cat.albums() {
        if s.lib.is_downloaded(&a.id) {
            tracks.extend(cat.tracks_of_album(&a.id));
        }
    }
    let body = if tracks.is_empty() {
        empty("Nothing on this device. Download an album first.", t)
    } else {
        track_list(tracks, s.now_playing.as_ref(), cat, t)
    };
    page("On this device", "Music you downloaded", t, body)
}

pub fn history(s: &State, cat: &Catalogue, t: tok::Theme) -> El {
    let tracks: Vec<&Track> = s.lib.history.iter().filter_map(|id| cat.track(id)).collect();
    let body = if tracks.is_empty() {
        empty("Nothing played yet.", t)
    } else {
        track_list(tracks, s.now_playing.as_ref(), cat, t)
    };
    page("History", "Recently played", t, body)
}

pub fn stats(s: &State, cat: &Catalogue, t: tok::Theme) -> El {
    let (recent_artist, recent_track) = recent_artist_and_track(s, cat);
    let playlists = s.lib.playlists.iter().filter(|p| p.is_some()).count();
    let stats: [(&str, String); 6] = [
        ("Liked tracks", s.lib.liked.len().to_string()),
        ("Saved albums", s.lib.saved_albums.len().to_string()),
        ("Followed artists", s.lib.followed.len().to_string()),
        ("Playlists", playlists.to_string()),
        ("Recent artist", recent_artist),
        ("Recent track", recent_track),
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

/// The artist and track at the front of history. History is duplicate-free, so
/// it cannot rank by play frequency; the front is the honest "most recent".
fn recent_artist_and_track(s: &State, cat: &Catalogue) -> (String, String) {
    match s.lib.history.first().and_then(|id| cat.track(id)) {
        Some(tr) => (artist_name(cat, &tr.artist).to_string(), tr.title.clone()),
        None => ("—".into(), "—".into()),
    }
}

pub fn downloads(s: &State, cat: &Catalogue, t: tok::Theme) -> El {
    let downloaded: Vec<&Album> = cat.albums().filter(|a| s.lib.is_downloaded(&a.id)).collect();
    let mut rows = col().gap(space::S1);
    if downloaded.is_empty() {
        rows = rows.child(empty("No downloads yet. Open an album and tap Download.", t));
    }
    for (pos, a) in downloaded.into_iter().enumerate() {
        let id = a.id.clone();
        rows = rows.child(
            row().key(format!("dl-{pos}")).h(44.0).px(space::S2).radius(radius::MD).center_y().gap(space::S3).hover(t.hover)
                .child(text(&a.title).key("title").font(type_::BODY).color(t.text).grow(1.0))
                .child(text(artist_name(cat, &a.artist)).key("artist").font(type_::LABEL).color(t.text_secondary))
                .child(text("Done").key("state").font(type_::LABEL).color(t.text_secondary))
                .child(el().key(format!("rm-dl-{pos}")).label("Remove")
                    .h(28.0).px(space::S2).radius(radius::SM).center_y().hover(t.hover).focusable(true)
                    .on_press(move |app: &mut App| {
                        app.core.lib.remove_download(&id);
                    })
                    .child(text("Remove").key("label").font(type_::LABEL).color(t.text_secondary))),
        );
    }
    page("Downloads", "Your download queue", t, rows)
}

pub fn search(cat: &Catalogue, covers: Covers<'_>, t: tok::Theme, edit: &TextEdit) -> El {
    let query = edit.text.trim();
    let results = if query.is_empty() {
        all_albums_grid(cat, covers, t)
    } else {
        let q = query.to_lowercase();
        let matches = |s: &str| s.to_lowercase().contains(&q);
        let albums: Vec<El> = cat.albums()
            .filter(|a| matches(&a.title) || matches(artist_name(cat, &a.artist)))
            .enumerate().map(|(i, a)| album_card(i, a, cat, covers, t)).collect();
        let artists: Vec<El> = cat.artists()
            .filter(|a| matches(&a.name))
            .enumerate().map(|(i, a)| artist_card(i, a, t)).collect();
        let tracks: Vec<El> = cat.tracks()
            .filter(|tr| matches(&tr.title) || matches(artist_name(cat, &tr.artist)))
            .enumerate().map(|(i, tr)| track_row(i, tr, false, cat, t)).collect();
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
                .on_press(move |a: &mut App| {
                    a.rename.clear();
                    state::open_detail(&mut a.core, &a.catalogue, Route::SettingsPage(p))
                })
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

/// Add the reason a sign-out was refused, when there is one. Every branch that
/// draws the Sign out button calls this, or the button looks dead.
fn with_write_error(app: &App, body: El, t: tok::Theme) -> El {
    match &app.write_error {
        Some(error) => body.child(
            text(error.clone())
                .key("write-error")
                .font(type_::BODY)
                .color(t.text_secondary),
        ),
        None => body,
    }
}

fn account_page(app: &App, t: tok::Theme) -> El {
    match &app.auth {
        crate::auth::Auth::SignedIn { .. } => {
            let name = app.core.lib.name.clone();
            let mut body = col().gap(space::S3)
                .child(settings_row("acct-name".into(), "Name", &name, t));
            // A fetch that failed without ending the session is said here, beside
            // the name it could not refresh. It must not look like a failed sign-in:
            // the session works, the network did not.
            if let Some(reason) = &app.profile_error {
                body = body.child(
                    text(reason.clone())
                        .key("profile-error")
                        .font(type_::BODY)
                        .color(t.text_secondary),
                );
            }
            page("Account", "Your profile", t,
                with_write_error(app, body, t)
                    .child(sync_button(t))
                    .child(text(sync_status(app)).key("sync-status").font(type_::BODY).color(t.text_secondary))
                    .child(sign_out_button(t)))
        }
        crate::auth::Auth::Authorizing { .. } => {
            page("Account", "Your profile", t,
                col().gap(space::S3)
                    .child(text("Authorizing…").key("auth-state").font(type_::BODY).color(t.text_secondary)))
        }
        crate::auth::Auth::Failed { message } => {
            let sign_in = sign_in_button(t);
            let body = col().gap(space::S3)
                .child(text(message).key("auth-error").font(type_::BODY).color(t.text_secondary));
            // This branch offers Sign out too, so it must say why one was refused.
            page("Account", "Your profile", t,
                with_write_error(app, body, t)
                    .child(sign_in)
                    .child(sign_out_button(t)))
        }
        crate::auth::Auth::SignedOut => {
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
                    .child(sign_in_button(t))
                    .child(field)
                    .child(save_btn))
        }
    }
}

/// What the last sync did, or that one is running. A failure says that nothing
/// changed, because a failed sync applies nothing.
fn sync_status(app: &App) -> String {
    if app.sync.is_pending() {
        return "Syncing with Tidal…".to_string();
    }
    match &app.last_sync {
        None => "Not synced in this session.".to_string(),
        Some(Err(message)) => message.clone(),
        Some(Ok(r)) => {
            let mut line = format!(
                "Synced {}, {}, {} and {}.",
                count(r.liked, "track"),
                count(r.saved_albums, "album"),
                count(r.followed, "artist"),
                count(r.tidal_playlists, "playlist")
            );
            if r.dropped > 0 {
                line.push_str(&format!(" {} could not be read.", count(r.dropped, "item")));
            }
            line
        }
    }
}

/// A count with its noun, such as `1 track` or `2 tracks`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

fn sync_button(t: tok::Theme) -> El {
    el().key("sync-library").label("Sync with Tidal")
        .h(40.0).px(space::S3).radius(radius::MD).center_y().focusable(true)
        .fill(t.hover)
        .on_press(|a: &mut App| a.sync())
        .child(text("Sync with Tidal").key("label").font(type_::BODY).color(t.text))
}

fn sign_in_button(t: tok::Theme) -> El {
    el().key("sign-in").label("Sign in with Tidal")
        .h(40.0).px(space::S3).radius(radius::MD).center_y().focusable(true)
        .fill(t.text)
        .on_press(|a: &mut App| a.start_sign_in())
        .child(text("Sign in with Tidal").key("label").font(type_::BODY).color(t.bg))
}

fn sign_out_button(t: tok::Theme) -> El {
    el().key("sign-out").label("Sign out")
        .h(40.0).px(space::S3).radius(radius::MD).center_y().focusable(true)
        .fill(t.text)
        .on_press(|a: &mut App| a.sign_out())
        .child(text("Sign out").key("label").font(type_::BODY).color(t.bg))
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

fn hero(art: El, title: &str, subtitle: &str, t: tok::Theme, actions: Vec<El>) -> El {
    let mut btns = row().gap(space::S2).pt(space::S3);
    for b in actions {
        btns = btns.child(b);
    }
    row().px(space::S6).pt(space::S6).pb(space::S3).gap(space::S6)
        .child(art)
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

/// A detail screen. The route variant selects the subject; the back affordance
/// lives in the top bar, not here.
pub fn detail(route: &Route, app: &App, accent: Color, t: tok::Theme) -> El {
    let s = &app.core;
    let cat = &app.catalogue;
    let covers = app.covers();
    match route {
        Route::Album(id) => album_detail(id, s, cat, covers, accent, t),
        Route::Artist(id) => artist_detail(id, s, cat, accent, t),
        Route::CataloguePlaylist(id) => catalogue_playlist_detail(id, s, cat, covers, accent, t),
        Route::UserPlaylist(id) => user_playlist_detail(*id, app, accent, t),
        _ => page("Detail", "", t, el()),
    }
}

/// Select the first track of a list the catalogue derives, at the time of the
/// press, so the button plays what the screen shows.
fn play_first(first: impl Fn(&Catalogue) -> Option<TrackId> + 'static) -> impl Fn(&mut App) + 'static {
    move |a: &mut App| {
        if let Some(track) = first(&a.catalogue) {
            state::select_track(&mut a.core, &a.catalogue, track);
        }
    }
}

fn album_detail(id: &AlbumId, s: &State, cat: &Catalogue, covers: Covers<'_>, accent: Color, t: tok::Theme) -> El {
    let Some(a) = cat.album(id) else {
        return page("Album", "This album is gone", t, el());
    };
    let saved = s.lib.is_saved_album(id);
    let downloaded = s.lib.is_downloaded(id);
    let artist = artist_name(cat, &a.artist);
    let tracks: Vec<&Track> = cat.tracks_of_album(id).collect();

    let play = id.clone();
    let save = id.clone();
    let download = id.clone();
    let actions = vec![
        action_button("play", "Play", accent, true, t, play_first(move |cat| cat.tracks_of_album(&play).next().map(|tr| tr.id.clone()))),
        action_button("save", if saved { "Remove" } else { "Save" }, accent, false, t, move |a: &mut App| {
            a.core.lib.toggle_save_album(save.clone());
        }),
        action_button("download", if downloaded { "Remove download" } else { "Download" }, accent, false, t, move |a: &mut App| {
            a.core.lib.toggle_download(download.clone());
        }),
    ];

    col().grow(1.0).min_h(0.0)
        .child(hero(cover_art("hero-cover", &covers.album(a), 200.0, radius::MD, t), &a.title, &format!("Album by {artist}"), t, actions))
        .child(detail_body(track_list(tracks, s.now_playing.as_ref(), cat, t)))
}

fn artist_detail(id: &ArtistId, s: &State, cat: &Catalogue, accent: Color, t: tok::Theme) -> El {
    let Some(a) = cat.artist(id) else {
        return page("Artist", "This artist is gone", t, el());
    };
    let following = s.lib.is_following(id);
    let tracks: Vec<&Track> = cat.tracks_of_artist(id).collect();

    let play = id.clone();
    let follow = id.clone();
    let actions = vec![
        action_button("play", "Play", accent, true, t, play_first(move |cat| cat.tracks_of_artist(&play).next().map(|tr| tr.id.clone()))),
        action_button("follow", if following { "Following" } else { "Follow" }, accent, false, t, move |a: &mut App| {
            a.core.lib.toggle_follow(follow.clone());
        }),
    ];

    col().grow(1.0).min_h(0.0)
        .child(hero(placeholder_art("hero-cover", placeholder_seed(id.as_str()), 200.0, 100.0, t), &a.name, "Artist", t, actions))
        .child(detail_body(track_list(tracks, s.now_playing.as_ref(), cat, t)))
}

fn catalogue_playlist_detail(id: &CataloguePlaylistId, s: &State, cat: &Catalogue, covers: Covers<'_>, accent: Color, t: tok::Theme) -> El {
    let Some(p) = cat.playlist(id) else {
        return page("Playlist", "This playlist is gone", t, el());
    };
    let tracks: Vec<&Track> = p.tracks.iter().filter_map(|tid| cat.track(tid)).collect();

    let play = id.clone();
    let actions = vec![
        action_button("play", "Play", accent, true, t, play_first(move |cat| cat.playlist(&play).and_then(|p| p.tracks.first().cloned()))),
    ];

    col().grow(1.0).min_h(0.0)
        .child(hero(cover_art("hero-cover", &covers.playlist(p), 200.0, radius::MD, t), &p.name, &p.description, t, actions))
        .child(detail_body(track_list(tracks, s.now_playing.as_ref(), cat, t)))
}

fn user_playlist_detail(id: PlaylistId, app: &App, accent: Color, t: tok::Theme) -> El {
    let s = &app.core;
    let cat = &app.catalogue;
    let Some(p) = s.lib.playlist(id) else {
        return page("Playlist", "", t, el());
    };
    let name = p.name.clone();
    let tracks: Vec<&Track> = p.tracks.iter().filter_map(|tid| cat.track(tid)).collect();
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
            if let Some(tr) = a.core.now_playing.clone() {
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
            list = list.child(playlist_track_row(id, pos, len, tr, s.now_playing.as_ref() == Some(&tr.id), cat, t));
        }
        body = body.child(list);
    }

    col().grow(1.0).min_h(0.0)
        .child(hero(placeholder_art("hero-cover", id.0 as usize + 10, 200.0, radius::MD, t), &name, "Your playlist", t, actions))
        .child(detail_body(body))
}
