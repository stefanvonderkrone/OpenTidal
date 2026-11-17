//! The immutable catalogue: artists, albums, tracks and the read-only
//! editorial playlists, plus the id newtypes. The catalogue never changes, so
//! it sits outside the state space the invariant proof explores.
//!
//! Ids are distinct newtypes so a track id can never be mistaken for an album
//! or artist id. An id is in range by construction here; the invariant in
//! `library.rs` proves no saved item or playlist references one that is not.

/// A catalogue artist id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ArtistId(pub u8);

/// A catalogue album id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AlbumId(pub u8);

/// A catalogue track id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TrackId(pub u8);

/// A read-only editorial playlist id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EditorialPlaylistId(pub u8);

#[derive(Clone, Copy)]
pub struct Artist {
    pub id: ArtistId,
    pub name: &'static str,
}

#[derive(Clone, Copy)]
pub struct Album {
    pub id: AlbumId,
    pub title: &'static str,
    pub artist: ArtistId,
}

#[derive(Clone, Copy)]
pub struct Track {
    pub id: TrackId,
    pub title: &'static str,
    pub artist: ArtistId,
    pub album: AlbumId,
    pub duration: &'static str,
}

#[derive(Clone, Copy)]
pub struct EditorialPlaylist {
    pub id: EditorialPlaylistId,
    pub name: &'static str,
    pub description: &'static str,
    pub tracks: &'static [TrackId],
}

pub const ARTISTS: [Artist; 10] = [
    Artist { id: ArtistId(0), name: "Tame Impala" },
    Artist { id: ArtistId(1), name: "Frank Ocean" },
    Artist { id: ArtistId(2), name: "Arctic Monkeys" },
    Artist { id: ArtistId(3), name: "Daft Punk" },
    Artist { id: ArtistId(4), name: "The Weeknd" },
    Artist { id: ArtistId(5), name: "Kendrick Lamar" },
    Artist { id: ArtistId(6), name: "Björk" },
    Artist { id: ArtistId(7), name: "Radiohead" },
    Artist { id: ArtistId(8), name: "Kacey Musgraves" },
    Artist { id: ArtistId(9), name: "Solange" },
];

pub const ALBUMS: [Album; 10] = [
    Album { id: AlbumId(0), title: "Currents", artist: ArtistId(0) },
    Album { id: AlbumId(1), title: "Blonde", artist: ArtistId(1) },
    Album { id: AlbumId(2), title: "AM", artist: ArtistId(2) },
    Album { id: AlbumId(3), title: "Random Access Memories", artist: ArtistId(3) },
    Album { id: AlbumId(4), title: "After Hours", artist: ArtistId(4) },
    Album { id: AlbumId(5), title: "DAMN.", artist: ArtistId(5) },
    Album { id: AlbumId(6), title: "Vespertine", artist: ArtistId(6) },
    Album { id: AlbumId(7), title: "In Rainbows", artist: ArtistId(7) },
    Album { id: AlbumId(8), title: "Golden Hour", artist: ArtistId(8) },
    Album { id: AlbumId(9), title: "A Seat at the Table", artist: ArtistId(9) },
];

pub const TRACKS: [Track; 21] = [
    Track { id: TrackId(0), title: "Let It Happen", artist: ArtistId(0), album: AlbumId(0), duration: "7:46" },
    Track { id: TrackId(1), title: "The Less I Know the Better", artist: ArtistId(0), album: AlbumId(0), duration: "3:38" },
    Track { id: TrackId(2), title: "Eventually", artist: ArtistId(0), album: AlbumId(0), duration: "5:19" },
    Track { id: TrackId(3), title: "Nikes", artist: ArtistId(1), album: AlbumId(1), duration: "5:14" },
    Track { id: TrackId(4), title: "Ivy", artist: ArtistId(1), album: AlbumId(1), duration: "4:09" },
    Track { id: TrackId(5), title: "Pink + White", artist: ArtistId(1), album: AlbumId(1), duration: "3:04" },
    Track { id: TrackId(6), title: "Do I Wanna Know?", artist: ArtistId(2), album: AlbumId(2), duration: "4:32" },
    Track { id: TrackId(7), title: "R U Mine?", artist: ArtistId(2), album: AlbumId(2), duration: "3:21" },
    Track { id: TrackId(8), title: "Get Lucky", artist: ArtistId(3), album: AlbumId(3), duration: "6:09" },
    Track { id: TrackId(9), title: "Instant Crush", artist: ArtistId(3), album: AlbumId(3), duration: "5:37" },
    Track { id: TrackId(10), title: "Blinding Lights", artist: ArtistId(4), album: AlbumId(4), duration: "3:20" },
    Track { id: TrackId(11), title: "Save Your Tears", artist: ArtistId(4), album: AlbumId(4), duration: "3:35" },
    Track { id: TrackId(12), title: "HUMBLE.", artist: ArtistId(5), album: AlbumId(5), duration: "2:57" },
    Track { id: TrackId(13), title: "DNA.", artist: ArtistId(5), album: AlbumId(5), duration: "3:05" },
    Track { id: TrackId(14), title: "Hidden Place", artist: ArtistId(6), album: AlbumId(6), duration: "5:28" },
    Track { id: TrackId(15), title: "Pagan Poetry", artist: ArtistId(6), album: AlbumId(6), duration: "5:14" },
    Track { id: TrackId(16), title: "Weird Fishes", artist: ArtistId(7), album: AlbumId(7), duration: "5:18" },
    Track { id: TrackId(17), title: "Reckoner", artist: ArtistId(7), album: AlbumId(7), duration: "4:50" },
    Track { id: TrackId(18), title: "Slow Burn", artist: ArtistId(8), album: AlbumId(8), duration: "4:06" },
    Track { id: TrackId(19), title: "Butterflies", artist: ArtistId(8), album: AlbumId(8), duration: "3:39" },
    Track { id: TrackId(20), title: "Cranes in the Sky", artist: ArtistId(9), album: AlbumId(9), duration: "4:10" },
];

pub const EDITORIAL_PLAYLISTS: [EditorialPlaylist; 3] = [
    EditorialPlaylist {
        id: EditorialPlaylistId(0),
        name: "Chill",
        description: "Laid-back for slow evenings",
        tracks: &[TrackId(3), TrackId(16), TrackId(18), TrackId(20)],
    },
    EditorialPlaylist {
        id: EditorialPlaylistId(1),
        name: "Workout",
        description: "Heavy and relentless",
        tracks: &[TrackId(0), TrackId(6), TrackId(12), TrackId(13)],
    },
    EditorialPlaylist {
        id: EditorialPlaylistId(2),
        name: "Focus",
        description: "Instrumental and steady",
        tracks: &[TrackId(1), TrackId(8), TrackId(15)],
    },
];

/// The artist, falling back to a neutral placeholder when the id is out of
/// range. The invariant keeps every stored id in range; the fallback only
/// defends the view against a bad id the proof would already have caught.
pub fn artist(id: ArtistId) -> Artist {
    ARTISTS.get(id.0 as usize).copied().unwrap_or(Artist { id, name: "Artist" })
}

pub fn album(id: AlbumId) -> Album {
    ALBUMS.get(id.0 as usize).copied().unwrap_or(Album { id, title: "Album", artist: ArtistId(0) })
}

pub fn track(id: TrackId) -> Track {
    TRACKS.get(id.0 as usize).copied().unwrap_or(Track {
        id,
        title: "Track",
        artist: ArtistId(0),
        album: AlbumId(0),
        duration: "0:00",
    })
}

pub fn editorial_playlist(id: EditorialPlaylistId) -> EditorialPlaylist {
    EDITORIAL_PLAYLISTS.get(id.0 as usize).copied().unwrap_or(EditorialPlaylist {
        id,
        name: "Playlist",
        description: "",
        tracks: &[],
    })
}

/// Every track on an album, in catalogue order.
pub fn tracks_of_album(id: AlbumId) -> impl Iterator<Item = Track> + 'static {
    TRACKS.iter().filter(move |t| t.album == id).copied()
}

/// Every track by an artist, in catalogue order.
pub fn tracks_of_artist(id: ArtistId) -> impl Iterator<Item = Track> + 'static {
    TRACKS.iter().filter(move |t| t.artist == id).copied()
}

/// The bitmask that covers every valid track id: bits `0..TRACKS.len()`.
pub fn track_mask() -> u32 {
    (1u32 << TRACKS.len()) - 1
}

/// The bitmask that covers every valid album id.
pub fn album_mask() -> u32 {
    (1u32 << ALBUMS.len()) - 1
}

/// The bitmask that covers every valid artist id.
pub fn artist_mask() -> u32 {
    (1u32 << ARTISTS.len()) - 1
}
