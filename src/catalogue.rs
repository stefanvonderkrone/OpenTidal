//! The catalogue: artists, albums, tracks and catalogue playlists, keyed by
//! opaque Tidal string ids. Rune-free.
//!
//! Two rules live here and nowhere else.
//! 1. Only a `Catalogue` mints an id, and only for a member.
//! 2. A catalogue never forgets an id. `absorb` adds or updates entries, and no
//!    method removes one.
//!
//! Together they mean that an id that exists names a member of the catalogue
//! that minted it, for the life of that catalogue.

use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::library::{Library, Members, Reach};

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        /// The field is private, so only `Catalogue` mints one. Equality, order
        /// and hash compare the text, so two catalogues built from the same rows
        /// mint equal ids.
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        pub struct $name(Arc<str>);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }
    };
}

id!(
    /// A Tidal artist id.
    ArtistId
);
id!(
    /// A Tidal album id.
    AlbumId
);
id!(
    /// A Tidal track id.
    TrackId
);
id!(
    /// A catalogue playlist id: a seed playlist or a Tidal playlist.
    CataloguePlaylistId
);
id!(
    /// A Tidal artwork id, from a `coverArt` relationship. The shape rule of
    /// `mint` makes it a safe file name: ASCII letters, digits and hyphens.
    ArtworkId
);

macro_rules! seeded {
    ($($name:ident),*) => {$(
        impl $name {
            /// Whether the seed holds this item. Every other item came from a
            /// Tidal account, so only those have a Tidal read.
            pub fn is_seed(&self) -> bool {
                self.0.starts_with(SEED_PREFIX)
            }
        }
    )*};
}
seeded!(ArtistId, AlbumId, CataloguePlaylistId);

/// The prefix of every seed id. A Tidal id is a number or a UUID, so it never
/// starts with this.
const SEED_PREFIX: &str = "seed-";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artist {
    pub id: ArtistId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Album {
    pub id: AlbumId,
    pub title: String,
    pub artist: ArtistId,
    /// `None` when Tidal named no cover, or named one with a malformed id.
    pub cover: Option<ArtworkId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub id: TrackId,
    pub title: String,
    pub artist: ArtistId,
    pub album: AlbumId,
    /// Seconds. The screen formats it.
    pub duration: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CataloguePlaylist {
    pub id: CataloguePlaylistId,
    pub name: String,
    pub description: String,
    /// Written only by `Catalogue::merge_tracks`, so a row with no tracks
    /// cannot erase the tracks a playlist already has, and emptied only by
    /// `Catalogue::forget_playlist_tracks` when another account opens.
    pub tracks: Members<TrackId>,
    /// `None` when Tidal named no cover, or named one with a malformed id.
    pub cover: Option<ArtworkId>,
}

/// Whose relationship names a cover: the `coverArt` of an album or a playlist,
/// or the `profileArt` of an artist. A fetch needs the owner, because the
/// pinned spec has no third-party read of an artwork by its id.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CoverOwner {
    Album(AlbumId),
    Playlist(CataloguePlaylistId),
    Artist(ArtistId),
}

/// One cover to show: the artwork and the owner whose relationship reads it.
/// Built only from an `Album` or a `CataloguePlaylist` that has a cover, or
/// from a Tidal `Artist`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CoverRequest {
    pub artwork: ArtworkId,
    pub owner: CoverOwner,
}

impl Album {
    pub fn cover_request(&self) -> Option<CoverRequest> {
        let artwork = self.cover.clone()?;
        Some(CoverRequest { artwork, owner: CoverOwner::Album(self.id.clone()) })
    }
}

impl Artist {
    /// No artist read names its profile art, so the cover is keyed by the
    /// artist as `artist-<id>`, and the fetch takes the artwork the
    /// `profileArt` relationship names. A seed artist is not on Tidal.
    pub fn cover_request(&self) -> Option<CoverRequest> {
        if self.id.is_seed() {
            return None;
        }
        let artwork = mint(&format!("artist-{}", self.id.as_str())).map(ArtworkId)?;
        Some(CoverRequest { artwork, owner: CoverOwner::Artist(self.id.clone()) })
    }
}

impl CataloguePlaylist {
    pub fn cover_request(&self) -> Option<CoverRequest> {
        let artwork = self.cover.clone()?;
        Some(CoverRequest { artwork, owner: CoverOwner::Playlist(self.id.clone()) })
    }
}

/// Unvalidated rows, the only way into a catalogue. The store produces them, and
/// the Tidal client will. Plain text, in the shape Tidal sends.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct CatalogueRows {
    pub artists: Vec<ArtistRow>,
    pub albums: Vec<AlbumRow>,
    pub tracks: Vec<TrackRow>,
    pub playlists: Vec<PlaylistRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtistRow {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumRow {
    pub id: String,
    pub title: String,
    pub artist: String,
    /// The artwork id, or empty for none.
    pub cover: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackRow {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// ISO 8601, such as `PT3M26S`.
    pub duration: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistRow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tracks: Vec<String>,
    /// The artwork id, or empty for none.
    pub cover: String,
}

/// The catalogue. Insertion order is display order. `absorb` is the only writer
/// of the indexes and the only one that adds an entry, so the two cannot
/// disagree. `merge_tracks` writes a playlist's tracks and nothing else.
#[derive(Default, Debug)]
pub struct Catalogue {
    artists: Vec<Artist>,
    albums: Vec<Album>,
    tracks: Vec<Track>,
    playlists: Vec<CataloguePlaylist>,
    artist_at: BTreeMap<ArtistId, usize>,
    album_at: BTreeMap<AlbumId, usize>,
    track_at: BTreeMap<TrackId, usize>,
    playlist_at: BTreeMap<CataloguePlaylistId, usize>,
    revision: u64,
}

impl Catalogue {
    /// The built-in catalogue: ten artists, ten albums, 21 tracks and three
    /// seed playlists, with ids such as `seed-track-0`. The `seed-` prefix
    /// cannot collide with a numeric Tidal id.
    pub fn seed() -> Catalogue {
        let mut cat = Catalogue::default();
        let dropped = cat.absorb(seed_rows());
        debug_assert_eq!(dropped, 0, "the seed names something it does not hold");
        cat
    }

    /// Merge rows and return how many it dropped. Artists go first, then albums
    /// whose artist is known, then tracks whose album and artist are known, then
    /// catalogue playlists with their known tracks. A row with a malformed id or
    /// duration is dropped. A malformed or empty cover never drops a row: it
    /// keeps the cover the entry already has, or none. A known id updates its
    /// fields in place and keeps its position. A playlist row with no tracks
    /// keeps the tracks the playlist has. Absorbing the same rows twice changes
    /// nothing.
    pub fn absorb(&mut self, rows: CatalogueRows) -> usize {
        let mut dropped = 0;
        let mut changed = false;
        for row in rows.artists {
            let Some(id) = mint(&row.id).map(ArtistId) else {
                dropped += 1;
                continue;
            };
            changed |= upsert(&mut self.artists, &mut self.artist_at, Artist { id, name: row.name }, |a| &a.id);
        }
        for row in rows.albums {
            let (Some(id), Some(artist)) = (mint(&row.id).map(AlbumId), self.artist_id(&row.artist))
            else {
                dropped += 1;
                continue;
            };
            let known = self.album(&id).and_then(|a| a.cover.clone());
            let cover = mint(&row.cover).map(ArtworkId).or(known);
            let album = Album { id, title: row.title, artist, cover };
            changed |= upsert(&mut self.albums, &mut self.album_at, album, |a| &a.id);
        }
        for row in rows.tracks {
            let (Some(id), Some(artist), Some(album), Some(duration)) = (
                mint(&row.id).map(TrackId),
                self.artist_id(&row.artist),
                self.album_id(&row.album),
                seconds(&row.duration),
            ) else {
                dropped += 1;
                continue;
            };
            let track = Track { id, title: row.title, artist, album, duration };
            changed |= upsert(&mut self.tracks, &mut self.track_at, track, |t| &t.id);
        }
        for row in rows.playlists {
            let Some(id) = mint(&row.id).map(CataloguePlaylistId) else {
                dropped += 1;
                continue;
            };
            let known = self.playlist(&id);
            let tracks = known.map(|p| p.tracks.clone()).unwrap_or_default();
            let cover = mint(&row.cover).map(ArtworkId).or(known.and_then(|p| p.cover.clone()));
            let playlist =
                CataloguePlaylist { id: id.clone(), name: row.name, description: row.description, tracks, cover };
            changed |= upsert(&mut self.playlists, &mut self.playlist_at, playlist, |p| &p.id);
            if !row.tracks.is_empty() {
                dropped += self.merge_tracks(&id, &row.tracks, Reach::Complete).unwrap_or(0);
            }
        }
        if changed {
            self.revision += 1;
        }
        dropped
    }

    /// Merge one playlist's tracks by the rule of `Members::merge`, and return
    /// how many it could not resolve. A playlist the catalogue lacks is ignored
    /// and returns `None`.
    pub fn merge_tracks(&mut self, id: &CataloguePlaylistId, confirmed: &[String], reach: Reach) -> Option<usize> {
        let &at = self.playlist_at.get(id)?;
        let resolved: Vec<TrackId> = confirmed.iter().filter_map(|t| self.track_id(t)).collect();
        if self.playlists[at].tracks.merge(&resolved, reach) {
            self.revision += 1;
        }
        Some(confirmed.len() - resolved.len())
    }

    /// Empty the tracks of every Tidal playlist, keeping its id, name and
    /// cover. The store keeps one track list per playlist for every account, so
    /// an account that opens after another must not show the other's list
    /// before its own page lands. A seed playlist belongs to no account and
    /// keeps its tracks.
    pub fn forget_playlist_tracks(&mut self) {
        let mut changed = false;
        for p in self.playlists.iter_mut().filter(|p| !p.id.is_seed() && !p.tracks.is_empty()) {
            p.tracks = Members::default();
            changed = true;
        }
        if changed {
            self.revision += 1;
        }
    }

    /// Rises each time `absorb`, `merge_tracks` or `forget_playlist_tracks`
    /// changes an entry, and only
    /// then, so a writer can tell whether the catalogue changed since it last
    /// looked.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn artist_id(&self, raw: &str) -> Option<ArtistId> {
        self.artist_at.get_key_value(raw).map(|(id, _)| id.clone())
    }
    pub fn album_id(&self, raw: &str) -> Option<AlbumId> {
        self.album_at.get_key_value(raw).map(|(id, _)| id.clone())
    }
    pub fn track_id(&self, raw: &str) -> Option<TrackId> {
        self.track_at.get_key_value(raw).map(|(id, _)| id.clone())
    }
    pub fn playlist_id(&self, raw: &str) -> Option<CataloguePlaylistId> {
        self.playlist_at.get_key_value(raw).map(|(id, _)| id.clone())
    }

    pub fn artist(&self, id: &ArtistId) -> Option<&Artist> {
        self.artist_at.get(id).map(|&i| &self.artists[i])
    }
    pub fn album(&self, id: &AlbumId) -> Option<&Album> {
        self.album_at.get(id).map(|&i| &self.albums[i])
    }
    pub fn track(&self, id: &TrackId) -> Option<&Track> {
        self.track_at.get(id).map(|&i| &self.tracks[i])
    }
    pub fn playlist(&self, id: &CataloguePlaylistId) -> Option<&CataloguePlaylist> {
        self.playlist_at.get(id).map(|&i| &self.playlists[i])
    }

    pub fn artists(&self) -> impl Iterator<Item = &Artist> {
        self.artists.iter()
    }
    pub fn albums(&self) -> impl Iterator<Item = &Album> {
        self.albums.iter()
    }
    pub fn tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter()
    }
    /// Only the seed playlists. A Tidal playlist belongs to one account, so a
    /// screen must never list every catalogue playlist. Tests use `playlists`
    /// below to inspect the whole catalogue.
    pub fn seed_playlists(&self) -> impl Iterator<Item = &CataloguePlaylist> {
        self.playlists.iter().filter(|p| p.id.is_seed())
    }

    #[cfg(test)]
    pub fn playlists(&self) -> impl Iterator<Item = &CataloguePlaylist> {
        self.playlists.iter()
    }

    /// The rows for everything a library names, closed over parents: a Tidal
    /// playlist brings its tracks, a track brings its album and artist, and an
    /// album brings its artist. `Store::write` stores these before the library
    /// rows that name them.
    pub fn rows_for(&self, lib: &Library) -> CatalogueRows {
        let playlists: Vec<&CataloguePlaylist> =
            lib.tidal_playlists.iter().filter_map(|id| self.playlist(id)).collect();
        let tracks: BTreeSet<&TrackId> = lib
            .liked
            .iter()
            .chain(&lib.history)
            .chain(lib.playlists.iter().flatten().flat_map(|p| &p.tracks))
            .chain(playlists.iter().flat_map(|p| &p.tracks))
            .collect();
        let tracks: Vec<&Track> = tracks.into_iter().filter_map(|id| self.track(id)).collect();
        let albums: BTreeSet<&AlbumId> = lib
            .saved_albums
            .iter()
            .chain(&lib.downloaded)
            .chain(tracks.iter().map(|t| &t.album))
            .collect();
        let albums: Vec<&Album> = albums.into_iter().filter_map(|id| self.album(id)).collect();
        let artists: BTreeSet<&ArtistId> = lib
            .followed
            .iter()
            .chain(albums.iter().map(|a| &a.artist))
            .chain(tracks.iter().map(|t| &t.artist))
            .collect();
        CatalogueRows {
            artists: artists
                .into_iter()
                .filter_map(|id| self.artist(id))
                .map(|a| ArtistRow { id: a.id.as_str().into(), name: a.name.clone() })
                .collect(),
            albums: albums
                .iter()
                .map(|a| AlbumRow {
                    id: a.id.as_str().into(),
                    title: a.title.clone(),
                    artist: a.artist.as_str().into(),
                    cover: cover_text(&a.cover),
                })
                .collect(),
            tracks: tracks
                .iter()
                .map(|t| TrackRow {
                    id: t.id.as_str().into(),
                    title: t.title.clone(),
                    artist: t.artist.as_str().into(),
                    album: t.album.as_str().into(),
                    duration: iso_duration(t.duration),
                })
                .collect(),
            playlists: playlists
                .iter()
                .map(|p| PlaylistRow {
                    id: p.id.as_str().into(),
                    name: p.name.clone(),
                    description: p.description.clone(),
                    tracks: p.tracks.iter().map(|t| t.as_str().into()).collect(),
                    cover: cover_text(&p.cover),
                })
                .collect(),
        }
    }

    /// Every track on an album, in catalogue order.
    pub fn tracks_of_album<'a>(&'a self, id: &'a AlbumId) -> impl Iterator<Item = &'a Track> {
        self.tracks.iter().filter(move |t| &t.album == id)
    }

    /// Every track by an artist, in catalogue order.
    pub fn tracks_of_artist<'a>(&'a self, id: &'a ArtistId) -> impl Iterator<Item = &'a Track> {
        self.tracks.iter().filter(move |t| &t.artist == id)
    }
}

/// A cover as row text: the artwork id, or empty for none.
fn cover_text(cover: &Option<ArtworkId>) -> String {
    cover.as_ref().map(|c| c.as_str().to_string()).unwrap_or_default()
}

/// Insert a new entry at the end, or replace a known one where it stands.
/// Returns whether anything changed.
fn upsert<I: Ord + Clone, T: PartialEq>(
    items: &mut Vec<T>,
    at: &mut BTreeMap<I, usize>,
    item: T,
    id: impl Fn(&T) -> &I,
) -> bool {
    match at.get(id(&item)) {
        Some(&i) if items[i] == item => false,
        Some(&i) => {
            items[i] = item;
            true
        }
        None => {
            at.insert(id(&item).clone(), items.len());
            items.push(item);
            true
        }
    }
}

/// The longest id the catalogue accepts. Tidal ids are short decimal strings, so
/// this only stops a damaged row from becoming a key.
const MAX_ID_LEN: usize = 64;

/// Whether text has the shape of an id: ASCII letters, digits and hyphens, from
/// one to 64 bytes. Such text is also a safe single path component, which the
/// cover cache relies on for user ids and artwork ids.
pub fn is_id_shaped(raw: &str) -> bool {
    (1..=MAX_ID_LEN).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The text of a new id, when it has the shape of one.
fn mint(raw: &str) -> Option<Arc<str>> {
    is_id_shaped(raw).then(|| Arc::from(raw))
}

/// Seconds from an ISO 8601 duration such as `PT3M26S`, the shape Tidal sends.
/// Hours, minutes and seconds are whole numbers, each optional and in that order,
/// and at least one is present.
pub fn seconds(iso: &str) -> Option<u32> {
    let mut rest = iso.strip_prefix("PT")?;
    let mut total: u32 = 0;
    let mut any = false;
    for (unit, scale) in [('H', 3600), ('M', 60), ('S', 1)] {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !rest[digits..].starts_with(unit) {
            continue;
        }
        let n: u32 = rest[..digits].parse().ok()?;
        total = total.checked_add(n.checked_mul(scale)?)?;
        rest = &rest[digits + 1..];
        any = true;
    }
    (any && rest.is_empty()).then_some(total)
}

/// A duration in seconds as ISO 8601, which `seconds` reads back.
pub fn iso_duration(seconds: u32) -> String {
    format!("PT{}M{}S", seconds / 60, seconds % 60)
}

const SEED_ARTISTS: [&str; 10] = [
    "Tame Impala",
    "Frank Ocean",
    "Arctic Monkeys",
    "Daft Punk",
    "The Weeknd",
    "Kendrick Lamar",
    "Björk",
    "Radiohead",
    "Kacey Musgraves",
    "Solange",
];

/// Album title and artist position. Album `n` is by artist `n`.
const SEED_ALBUMS: [&str; 10] = [
    "Currents",
    "Blonde",
    "AM",
    "Random Access Memories",
    "After Hours",
    "DAMN.",
    "Vespertine",
    "In Rainbows",
    "Golden Hour",
    "A Seat at the Table",
];

/// Title, album position and duration. A track is by its album's artist.
const SEED_TRACKS: [(&str, usize, &str); 21] = [
    ("Let It Happen", 0, "PT7M46S"),
    ("The Less I Know the Better", 0, "PT3M38S"),
    ("Eventually", 0, "PT5M19S"),
    ("Nikes", 1, "PT5M14S"),
    ("Ivy", 1, "PT4M9S"),
    ("Pink + White", 1, "PT3M4S"),
    ("Do I Wanna Know?", 2, "PT4M32S"),
    ("R U Mine?", 2, "PT3M21S"),
    ("Get Lucky", 3, "PT6M9S"),
    ("Instant Crush", 3, "PT5M37S"),
    ("Blinding Lights", 4, "PT3M20S"),
    ("Save Your Tears", 4, "PT3M35S"),
    ("HUMBLE.", 5, "PT2M57S"),
    ("DNA.", 5, "PT3M5S"),
    ("Hidden Place", 6, "PT5M28S"),
    ("Pagan Poetry", 6, "PT5M14S"),
    ("Weird Fishes", 7, "PT5M18S"),
    ("Reckoner", 7, "PT4M50S"),
    ("Slow Burn", 8, "PT4M6S"),
    ("Butterflies", 8, "PT3M39S"),
    ("Cranes in the Sky", 9, "PT4M10S"),
];

/// Name, description and track positions.
const SEED_PLAYLISTS: [(&str, &str, &[usize]); 3] = [
    ("Chill", "Laid-back for slow evenings", &[3, 16, 18, 20]),
    ("Workout", "Heavy and relentless", &[0, 6, 12, 13]),
    ("Focus", "Instrumental and steady", &[1, 8, 15]),
];

fn seed_rows() -> CatalogueRows {
    let artist = |n: usize| format!("seed-artist-{n}");
    let album = |n: usize| format!("seed-album-{n}");
    let track = |n: usize| format!("seed-track-{n}");
    CatalogueRows {
        artists: SEED_ARTISTS
            .iter()
            .enumerate()
            .map(|(n, name)| ArtistRow { id: artist(n), name: name.to_string() })
            .collect(),
        albums: SEED_ALBUMS
            .iter()
            .enumerate()
            .map(|(n, title)| AlbumRow {
                id: album(n),
                title: title.to_string(),
                artist: artist(n),
                cover: String::new(),
            })
            .collect(),
        tracks: SEED_TRACKS
            .iter()
            .enumerate()
            .map(|(n, &(title, on, duration))| TrackRow {
                id: track(n),
                title: title.to_string(),
                artist: artist(on),
                album: album(on),
                duration: duration.to_string(),
            })
            .collect(),
        playlists: SEED_PLAYLISTS
            .iter()
            .enumerate()
            .map(|(n, &(name, description, tracks))| PlaylistRow {
                id: format!("seed-playlist-{n}"),
                name: name.to_string(),
                description: description.to_string(),
                tracks: tracks.iter().map(|&t| track(t)).collect(),
                cover: String::new(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artist(id: &str) -> ArtistRow {
        ArtistRow { id: id.into(), name: format!("{id} name") }
    }

    fn album(id: &str, artist: &str) -> AlbumRow {
        AlbumRow { id: id.into(), title: format!("{id} title"), artist: artist.into(), cover: String::new() }
    }

    fn track(id: &str, album: &str, artist: &str) -> TrackRow {
        TrackRow {
            id: id.into(),
            title: format!("{id} title"),
            artist: artist.into(),
            album: album.into(),
            duration: "PT3M26S".into(),
        }
    }

    /// The seed drops nothing, keeps today's order, and every reference in it
    /// resolves.
    #[test]
    fn the_seed_resolves_every_reference() {
        let mut cat = Catalogue::default();
        assert_eq!(cat.absorb(seed_rows()), 0, "the seed dropped a row");
        assert_eq!(cat.artists().count(), 10);
        assert_eq!(cat.albums().count(), 10);
        assert_eq!(cat.tracks().count(), 21);
        assert_eq!(cat.playlists().count(), 3);
        assert_eq!(cat.albums().next().map(|a| a.title.as_str()), Some("Currents"));
        for t in cat.tracks() {
            assert!(cat.album(&t.album).is_some(), "{t:?} names a missing album");
            assert!(cat.artist(&t.artist).is_some(), "{t:?} names a missing artist");
        }
        for p in cat.playlists() {
            assert!(!p.tracks.is_empty());
            assert!(p.tracks.iter().all(|t| cat.track(t).is_some()));
        }
        assert_eq!(cat.track(&cat.track_id("seed-track-0").unwrap()).unwrap().duration, 466);
    }

    /// A Tidal playlist in the catalogue never appears on the seed shelf, so one
    /// account's playlist names cannot reach another account's Home.
    #[test]
    fn seed_playlists_exclude_tidal_playlists() {
        let mut cat = Catalogue::seed();
        cat.absorb(CatalogueRows {
            playlists: vec![PlaylistRow {
                id: "private-playlist".into(),
                name: "Private".into(),
                description: String::new(),
                tracks: Vec::new(),
                cover: String::new(),
            }],
            ..Default::default()
        });
        assert_eq!(cat.playlists().count(), 4);
        assert_eq!(cat.seed_playlists().count(), 3);
        assert!(cat.seed_playlists().all(|p| p.id.is_seed()));
    }

    /// A row whose parent the catalogue lacks is dropped and counted, so no entry
    /// can name an artist or album that does not exist.
    #[test]
    fn absorb_drops_a_row_whose_parent_is_unknown() {
        let mut cat = Catalogue::default();
        let dropped = cat.absorb(CatalogueRows {
            artists: vec![artist("1")],
            albums: vec![album("10", "1"), album("11", "2")],
            tracks: vec![track("100", "10", "1"), track("101", "11", "1"), track("102", "10", "2")],
            playlists: vec![PlaylistRow {
                id: "e".into(),
                name: "E".into(),
                description: String::new(),
                tracks: vec!["100".into(), "999".into()],
                cover: String::new(),
            }],
        });
        assert_eq!(dropped, 4);
        assert!(cat.album_id("11").is_none(), "an album with an unknown artist got in");
        assert!(cat.track_id("101").is_none(), "a track with an unknown album got in");
        assert!(cat.track_id("102").is_none(), "a track with an unknown artist got in");
        assert!(cat.track_id("100").is_some());
        let e = cat.playlists().find(|p| p.id.as_str() == "e").unwrap();
        assert_eq!(e.tracks.to_vec(), vec![cat.track_id("100").unwrap()]);
    }

    /// Absorbing new rows never removes an earlier id, so an id held by a route or
    /// a library stays a member.
    #[test]
    fn absorb_never_forgets_an_id() {
        let mut cat = Catalogue::seed();
        let held = cat.track_id("seed-track-5").unwrap();
        cat.absorb(CatalogueRows {
            artists: vec![artist("seed-artist-1")],
            albums: vec![album("77610756", "seed-artist-1")],
            tracks: vec![track("77610757", "77610756", "seed-artist-1")],
            playlists: Vec::new(),
        });
        assert!(cat.track(&held).is_some(), "absorb forgot an id");
        assert_eq!(cat.tracks().count(), 22);
        assert_eq!(cat.artist(&cat.artist_id("seed-artist-1").unwrap()).unwrap().name, "seed-artist-1 name");
    }

    /// Absorbing the same rows twice changes nothing.
    #[test]
    fn absorb_twice_changes_nothing() {
        let mut once = Catalogue::seed();
        let before = format!("{once:?}");
        assert_eq!(once.absorb(seed_rows()), 0);
        assert_eq!(format!("{once:?}"), before);
    }

    /// Minting returns an id only for a member, and absorb refuses an id that does
    /// not have the shape of one.
    #[test]
    fn an_id_is_minted_only_for_a_member() {
        let mut cat = Catalogue::seed();
        assert!(cat.track_id("seed-track-0").is_some());
        assert!(cat.track_id("not-a-track").is_none());
        assert!(cat.album_id("seed-track-0").is_none(), "a track id minted an album id");
        let long = "9".repeat(MAX_ID_LEN + 1);
        let dropped = cat.absorb(CatalogueRows {
            artists: vec![artist("has space"), artist(""), artist(&long), artist("a/b")],
            ..CatalogueRows::default()
        });
        assert_eq!(dropped, 4);
        assert!(cat.artist_id("has space").is_none());
        assert!(cat.artist_id(&long).is_none());
        assert_eq!(cat.artists().count(), 10);
    }

    /// The rows for a library hold every entry it names and their parents, and
    /// nothing else. Absorbed into an empty catalogue, they resolve the library.
    #[test]
    fn rows_for_closes_over_parents() {
        let cat = Catalogue::seed();
        let mut lib = Library::default();
        lib.toggle_like(cat.track_id("seed-track-3").unwrap());
        lib.toggle_follow(cat.artist_id("seed-artist-9").unwrap());
        lib.toggle_save_album(cat.album_id("seed-album-5").unwrap());
        let rows = cat.rows_for(&lib);
        let tracks: Vec<&str> = rows.tracks.iter().map(|t| t.id.as_str()).collect();
        let albums: Vec<&str> = rows.albums.iter().map(|a| a.id.as_str()).collect();
        let artists: Vec<&str> = rows.artists.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(tracks, ["seed-track-3"]);
        assert_eq!(albums, ["seed-album-1", "seed-album-5"]);
        assert_eq!(artists, ["seed-artist-1", "seed-artist-5", "seed-artist-9"]);
        let mut fresh = Catalogue::default();
        assert_eq!(fresh.absorb(rows), 0);
        assert_eq!(lib.check(&fresh), Ok(()));
    }

    fn playlist(id: &str, cover: &str) -> PlaylistRow {
        PlaylistRow {
            id: id.into(),
            name: format!("{id} name"),
            description: String::new(),
            tracks: vec!["seed-track-0".into()],
            cover: cover.into(),
        }
    }

    fn covered_album(id: &str, cover: &str) -> AlbumRow {
        AlbumRow { cover: cover.into(), ..album(id, "seed-artist-0") }
    }

    /// A malformed cover costs the cover, never the album. Each bad id is one a
    /// file name must not carry, so it must not become an artwork id.
    #[test]
    fn a_bad_cover_does_not_hide_its_album() {
        let mut cat = Catalogue::seed();
        let long = "9".repeat(MAX_ID_LEN + 1);
        let bad = ["a/b", "..", "has space", long.as_str()];
        let rows: Vec<AlbumRow> =
            bad.iter().enumerate().map(|(n, c)| covered_album(&format!("bad-{n}"), c)).collect();
        assert_eq!(cat.absorb(CatalogueRows { albums: rows, ..CatalogueRows::default() }), 0);
        for n in 0..bad.len() {
            let id = cat.album_id(&format!("bad-{n}")).expect("a bad cover hid its album");
            assert_eq!(cat.album(&id).unwrap().cover, None);
        }
    }

    /// A Tidal artist asks for its profile art under a key of its own. A seed
    /// artist is not on Tidal and asks for nothing.
    #[test]
    fn only_a_tidal_artist_asks_for_a_cover() {
        let mut cat = Catalogue::seed();
        cat.absorb(CatalogueRows { artists: vec![artist("12979")], ..CatalogueRows::default() });
        let request = cat.artist(&cat.artist_id("12979").unwrap()).unwrap().cover_request().unwrap();
        assert_eq!(request.artwork.as_str(), "artist-12979");
        assert_eq!(request.owner, CoverOwner::Artist(cat.artist_id("12979").unwrap()));
        let seed = cat.artist(&cat.artist_id("seed-artist-0").unwrap()).unwrap();
        assert_eq!(seed.cover_request(), None);
    }

    /// The playlist twin of the album rule.
    #[test]
    fn a_bad_cover_does_not_hide_its_playlist() {
        let mut cat = Catalogue::seed();
        let dropped =
            cat.absorb(CatalogueRows { playlists: vec![playlist("p-1", "a/b")], ..CatalogueRows::default() });
        assert_eq!(dropped, 0);
        let id = cat.playlist_id("p-1").expect("a bad cover hid its playlist");
        let p = cat.playlist(&id).unwrap();
        assert_eq!(p.cover, None);
        assert_eq!(p.tracks.len(), 1);
    }

    /// A track page names its album without `coverArt`. That row must not erase
    /// the cover the saved-albums page gave, and neither must a bad one.
    #[test]
    fn a_row_without_a_cover_keeps_the_known_cover() {
        let mut cat = Catalogue::seed();
        cat.absorb(CatalogueRows {
            albums: vec![covered_album("10", "art-1")],
            playlists: vec![playlist("p-1", "art-2")],
            ..CatalogueRows::default()
        });
        cat.absorb(CatalogueRows {
            albums: vec![covered_album("10", "")],
            playlists: vec![playlist("p-1", "a/b")],
            ..CatalogueRows::default()
        });
        let album = cat.album(&cat.album_id("10").unwrap()).unwrap();
        assert_eq!(album.cover.as_ref().map(ArtworkId::as_str), Some("art-1"));
        let p = cat.playlist(&cat.playlist_id("p-1").unwrap()).unwrap();
        assert_eq!(p.cover.as_ref().map(ArtworkId::as_str), Some("art-2"));
        let request = album.cover_request().unwrap();
        assert_eq!(request.owner, CoverOwner::Album(album.id.clone()));
        assert_eq!(p.cover_request().unwrap().owner, CoverOwner::Playlist(p.id.clone()));
    }

    /// The rows for a library carry the covers, so a write and a load keep them.
    #[test]
    fn rows_for_carries_the_cover() {
        let mut cat = Catalogue::seed();
        cat.absorb(CatalogueRows {
            albums: vec![covered_album("10", "art-1")],
            playlists: vec![playlist("p-1", "art-2")],
            ..CatalogueRows::default()
        });
        let mut lib = Library::default();
        lib.toggle_save_album(cat.album_id("10").unwrap());
        lib.tidal_playlists.insert(cat.playlist_id("p-1").unwrap());
        let rows = cat.rows_for(&lib);
        let album = rows.albums.iter().find(|a| a.id == "10").unwrap();
        assert_eq!(album.cover, "art-1");
        assert_eq!(rows.playlists[0].cover, "art-2");
        let mut fresh = Catalogue::default();
        assert_eq!(fresh.absorb(rows), 0);
        assert_eq!(fresh.album(&fresh.album_id("10").unwrap()).unwrap().cover, cat.album(&cat.album_id("10").unwrap()).unwrap().cover);
    }

    fn listed(id: &str, name: &str, tracks: &[&str]) -> PlaylistRow {
        PlaylistRow {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            tracks: tracks.iter().map(|t| t.to_string()).collect(),
            cover: String::new(),
        }
    }

    /// A playlist listing page names a playlist without its tracks. That row
    /// updates the name and must not erase the tracks a track page gave.
    #[test]
    fn a_listing_row_without_tracks_keeps_the_cached_tracks() {
        let mut cat = Catalogue::seed();
        cat.absorb(CatalogueRows {
            playlists: vec![listed("p-1", "Old", &["seed-track-4", "seed-track-2"])],
            ..CatalogueRows::default()
        });
        cat.absorb(CatalogueRows { playlists: vec![listed("p-1", "New", &[])], ..CatalogueRows::default() });
        let p = cat.playlist(&cat.playlist_id("p-1").unwrap()).unwrap();
        assert_eq!(p.name, "New");
        let held: Vec<&str> = p.tracks.iter().map(TrackId::as_str).collect();
        assert_eq!(held, ["seed-track-4", "seed-track-2"], "a listing row erased the tracks");
    }

    /// The revision tells a writer the catalogue changed. It rises on a new or
    /// changed entry and on new playlist tracks, and holds on a repeat.
    #[test]
    fn the_revision_rises_only_on_a_real_change() {
        let mut cat = Catalogue::seed();
        let rows = CatalogueRows {
            artists: vec![artist("1")],
            playlists: vec![listed("p-1", "P", &["seed-track-0"])],
            ..CatalogueRows::default()
        };
        let start = cat.revision();
        cat.absorb(rows.clone());
        let after_new = cat.revision();
        assert!(after_new > start, "a new entry did not raise the revision");
        cat.absorb(rows);
        assert_eq!(cat.revision(), after_new, "an identical absorb raised the revision");
        let id = cat.playlist_id("p-1").unwrap();
        cat.merge_tracks(&id, &["seed-track-0".into()], Reach::Complete);
        assert_eq!(cat.revision(), after_new, "identical tracks raised the revision");
        cat.merge_tracks(&id, &["seed-track-1".into()], Reach::Partial);
        assert!(cat.revision() > after_new, "new tracks did not raise the revision");
    }

    /// Tidal sends a duration as ISO 8601. A track whose duration does not parse
    /// is dropped, not stored as zero.
    #[test]
    fn a_duration_is_read_from_iso_8601() {
        assert_eq!(seconds("PT3M26S"), Some(206));
        assert_eq!(seconds("PT1H2M3S"), Some(3723));
        assert_eq!(seconds("PT45S"), Some(45));
        assert_eq!(seconds("PT2M"), Some(120));
        assert_eq!(seconds(&iso_duration(3723)), Some(3723));
        for bad in ["3:26", "PT", "P1D", "PT3S2M", "PT-1S", "PT3M26", ""] {
            assert_eq!(seconds(bad), None, "{bad} parsed");
        }
        let mut cat = Catalogue::seed();
        let mut row = track("1", "seed-album-0", "seed-artist-0");
        row.duration = "3:26".into();
        assert_eq!(cat.absorb(CatalogueRows { tracks: vec![row], ..CatalogueRows::default() }), 1);
        assert!(cat.track_id("1").is_none());
    }
}
