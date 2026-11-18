//! The single local user's mutable data and its operations. Rune-free and
//! nav-free: every operation validates its slots, and [`Library::check`] proves
//! no reference outside the catalogue and no dangling slot survives.
//!
//! Bounds shrink under `cfg(test)` so the exhaustive proof in `state.rs` stays
//! finite; the runtime uses the real sizes. The random-walk test runs the full
//! sizes and calls [`Library::check`] after every step.

use std::collections::BTreeSet;

use crate::catalogue::{AlbumId, ArtistId, Catalogue, TrackId};

#[cfg(not(test))]
pub const MAX_PLAYLISTS: usize = 32;
#[cfg(test)]
pub const MAX_PLAYLISTS: usize = 2;

#[cfg(not(test))]
pub const HISTORY_CAP: usize = 50;
#[cfg(test)]
pub const HISTORY_CAP: usize = 2;

#[cfg(not(test))]
pub const PLAYLIST_TRACK_CAP: usize = 200;
#[cfg(test)]
pub const PLAYLIST_TRACK_CAP: usize = 2;

#[cfg(not(test))]
pub const MAX_COLLECTIONS: usize = 16;
#[cfg(test)]
pub const MAX_COLLECTIONS: usize = 1;

#[cfg(not(test))]
pub const COLLECTION_PLAYLIST_CAP: usize = 32;
#[cfg(test)]
pub const COLLECTION_PLAYLIST_CAP: usize = 2;

/// A user playlist id: the slot in `Library::playlists`. The slot may be empty
/// (`None`) after a delete; a create reuses the first empty slot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PlaylistId(pub u8);

/// A user playlist: a name and an ordered, duplicate-free track list.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Playlist {
    pub name: String,
    pub tracks: Vec<TrackId>,
}

/// A folder of user playlists.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Collection {
    pub name: String,
    pub playlists: Vec<PlaylistId>,
}

/// The single local user's data. The sets are `BTreeSet`, not `HashSet`, because
/// a `BTreeSet` is `Hash`, so `Library` and `State` stay hashable for the proof.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Library {
    pub name: String,
    pub liked: BTreeSet<TrackId>,
    pub saved_albums: BTreeSet<AlbumId>,
    pub followed: BTreeSet<ArtistId>,
    /// Albums taken offline.
    pub downloaded: BTreeSet<AlbumId>,
    /// Recently played tracks, most recent first, never longer than
    /// [`HISTORY_CAP`] and duplicate-free.
    pub history: Vec<TrackId>,
    /// User playlists, keyed by [`PlaylistId`] slot. A slot is `None` after a
    /// delete.
    pub playlists: [Option<Playlist>; MAX_PLAYLISTS],
    pub collections: Vec<Collection>,
}

impl Default for Library {
    fn default() -> Self {
        Library {
            name: "You".to_string(),
            liked: BTreeSet::new(),
            saved_albums: BTreeSet::new(),
            followed: BTreeSet::new(),
            downloaded: BTreeSet::new(),
            history: Vec::new(),
            playlists: std::array::from_fn(|_| None),
            collections: Vec::new(),
        }
    }
}

/// A safety violation, named so the random-walk test reports what broke.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Violation {
    LikedNotInCatalogue,
    SavedAlbumNotInCatalogue,
    FollowedNotInCatalogue,
    DownloadedNotInCatalogue,
    HistoryNotInCatalogue,
    HistoryTooLong,
    HistoryDuplicate,
    PlaylistTrackNotInCatalogue,
    PlaylistTooLong,
    PlaylistDuplicate,
    TooManyCollections,
    CollectionPlaylistOutOfRange,
    CollectionPlaylistMissing,
    CollectionPlaylistDuplicate,
    CollectionTooManyPlaylists,
}

/// Insert an item, or remove it when present.
fn toggle<T: Ord>(set: &mut BTreeSet<T>, item: T) {
    if !set.remove(&item) {
        set.insert(item);
    }
}

impl Library {
    // --- Like / save / follow / download ----------------------------------

    pub fn toggle_like(&mut self, t: TrackId) {
        toggle(&mut self.liked, t);
    }
    pub fn is_liked(&self, t: &TrackId) -> bool {
        self.liked.contains(t)
    }

    pub fn toggle_save_album(&mut self, a: AlbumId) {
        toggle(&mut self.saved_albums, a);
    }
    pub fn is_saved_album(&self, a: &AlbumId) -> bool {
        self.saved_albums.contains(a)
    }

    pub fn toggle_follow(&mut self, a: ArtistId) {
        toggle(&mut self.followed, a);
    }
    pub fn is_following(&self, a: &ArtistId) -> bool {
        self.followed.contains(a)
    }

    pub fn remove_download(&mut self, a: &AlbumId) {
        self.downloaded.remove(a);
    }
    pub fn toggle_download(&mut self, a: AlbumId) {
        toggle(&mut self.downloaded, a);
    }
    pub fn is_downloaded(&self, a: &AlbumId) -> bool {
        self.downloaded.contains(a)
    }

    // --- History ----------------------------------------------------------

    /// Record a play: move `t` to the front, drop its older copy, and keep the
    /// ring within [`HISTORY_CAP`].
    pub fn record_play(&mut self, t: TrackId) {
        self.history.retain(|x| x != &t);
        self.history.insert(0, t);
        self.history.truncate(HISTORY_CAP);
    }

    // --- User playlists ---------------------------------------------------

    pub fn playlist(&self, id: PlaylistId) -> Option<&Playlist> {
        self.playlists.get(id.0 as usize).and_then(|s| s.as_ref())
    }
    pub fn playlist_mut(&mut self, id: PlaylistId) -> Option<&mut Playlist> {
        self.playlists.get_mut(id.0 as usize).and_then(|s| s.as_mut())
    }

    /// Create a playlist in the first empty slot. `None` when full.
    pub fn create_playlist(&mut self, name: String) -> Option<PlaylistId> {
        for (i, slot) in self.playlists.iter().enumerate() {
            if slot.is_none() {
                self.playlists[i] = Some(Playlist {
                    name,
                    tracks: Vec::new(),
                });
                return Some(PlaylistId(i as u8));
            }
        }
        None
    }

    pub fn rename_playlist(&mut self, id: PlaylistId, name: String) -> bool {
        match self.playlist_mut(id) {
            Some(p) => {
                p.name = name;
                true
            }
            None => false,
        }
    }

    pub fn delete_playlist(&mut self, id: PlaylistId) -> bool {
        let idx = id.0 as usize;
        if idx >= MAX_PLAYLISTS || self.playlists[idx].is_none() {
            return false;
        }
        self.playlists[idx] = None;
        for c in &mut self.collections {
            c.playlists.retain(|&p| p != id);
        }
        true
    }

    /// Add a track to the end, unless it is already present or the playlist is
    /// full.
    pub fn add_track(&mut self, id: PlaylistId, t: TrackId) -> bool {
        match self.playlist_mut(id) {
            Some(p) if p.tracks.len() < PLAYLIST_TRACK_CAP && !p.tracks.contains(&t) => {
                p.tracks.push(t);
                true
            }
            _ => false,
        }
    }

    /// Remove every occurrence of a track.
    pub fn remove_track(&mut self, id: PlaylistId, t: &TrackId) -> bool {
        match self.playlist_mut(id) {
            Some(p) => {
                let before = p.tracks.len();
                p.tracks.retain(|x| x != t);
                before != p.tracks.len()
            }
            None => false,
        }
    }

    /// Move the track at position `from` to position `to`.
    pub fn move_track(&mut self, id: PlaylistId, from: usize, to: usize) -> bool {
        match self.playlist_mut(id) {
            Some(p) if from < p.tracks.len() && to < p.tracks.len() => {
                let t = p.tracks.remove(from);
                p.tracks.insert(to, t);
                true
            }
            _ => false,
        }
    }

    // --- Collections (folders of user playlists) --------------------------

    pub fn create_collection(&mut self, name: String) -> bool {
        if self.collections.len() >= MAX_COLLECTIONS {
            return false;
        }
        self.collections.push(Collection { name, playlists: Vec::new() });
        true
    }

    /// Rename a collection.
    pub fn rename_collection(&mut self, idx: usize, name: String) -> bool {
        match self.collections.get_mut(idx) {
            Some(c) => {
                c.name = name;
                true
            }
            None => false,
        }
    }

    pub fn delete_collection(&mut self, idx: usize) -> bool {
        if idx < self.collections.len() {
            self.collections.remove(idx);
            true
        } else {
            false
        }
    }

    /// Add a playlist to a collection, unless already present or the
    /// collection is full. The playlist must exist.
    pub fn add_to_collection(&mut self, idx: usize, p: PlaylistId) -> bool {
        if self.playlist(p).is_none() {
            return false;
        }
        match self.collections.get_mut(idx) {
            Some(c) if c.playlists.len() < COLLECTION_PLAYLIST_CAP && !c.playlists.contains(&p) => {
                c.playlists.push(p);
                true
            }
            _ => false,
        }
    }

    pub fn remove_from_collection(&mut self, idx: usize, p: PlaylistId) -> bool {
        match self.collections.get_mut(idx) {
            Some(c) => {
                let before = c.playlists.len();
                c.playlists.retain(|&x| x != p);
                before != c.playlists.len()
            }
            None => false,
        }
    }

    // --- Profile ----------------------------------------------------------

    pub fn set_name(&mut self, name: String) {
        self.name = name;
    }

    // --- The invariant ----------------------------------------------------

    /// Every catalogue reference the library holds is a member of `cat`, and
    /// every cap and duplicate rule holds. This is the property the exhaustive
    /// proof and the random walk both check after every operation. The store
    /// calls it before a write and asserts it after a load.
    pub fn check(&self, cat: &Catalogue) -> Result<(), Violation> {
        if !self.liked.iter().all(|t| cat.track(t).is_some()) {
            return Err(Violation::LikedNotInCatalogue);
        }
        if !self.saved_albums.iter().all(|a| cat.album(a).is_some()) {
            return Err(Violation::SavedAlbumNotInCatalogue);
        }
        if !self.followed.iter().all(|a| cat.artist(a).is_some()) {
            return Err(Violation::FollowedNotInCatalogue);
        }
        if !self.downloaded.iter().all(|a| cat.album(a).is_some()) {
            return Err(Violation::DownloadedNotInCatalogue);
        }
        if self.history.len() > HISTORY_CAP {
            return Err(Violation::HistoryTooLong);
        }
        if !self.history.iter().all(|t| cat.track(t).is_some()) {
            return Err(Violation::HistoryNotInCatalogue);
        }
        if has_dup(&self.history) {
            return Err(Violation::HistoryDuplicate);
        }
        for p in self.playlists.iter().flatten() {
            if p.tracks.len() > PLAYLIST_TRACK_CAP {
                return Err(Violation::PlaylistTooLong);
            }
            if !p.tracks.iter().all(|t| cat.track(t).is_some()) {
                return Err(Violation::PlaylistTrackNotInCatalogue);
            }
            if has_dup(&p.tracks) {
                return Err(Violation::PlaylistDuplicate);
            }
        }
        if self.collections.len() > MAX_COLLECTIONS {
            return Err(Violation::TooManyCollections);
        }
        for c in &self.collections {
            if c.playlists.len() > COLLECTION_PLAYLIST_CAP {
                return Err(Violation::CollectionTooManyPlaylists);
            }
            for &p in &c.playlists {
                if p.0 as usize >= MAX_PLAYLISTS {
                    return Err(Violation::CollectionPlaylistOutOfRange);
                }
                if self.playlists[p.0 as usize].is_none() {
                    return Err(Violation::CollectionPlaylistMissing);
                }
            }
            if has_dup(&c.playlists) {
                return Err(Violation::CollectionPlaylistDuplicate);
            }
        }
        Ok(())
    }
}

/// True when a slice has a duplicate. Shared by the three duplicate checks in
/// [`Library::check`].
fn has_dup<T: PartialEq>(xs: &[T]) -> bool {
    xs.iter().enumerate().any(|(i, x)| xs[..i].contains(x))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{AlbumRow, ArtistRow, CatalogueRows, TrackRow};
    use std::collections::{HashSet, VecDeque};

    fn applying(f: impl Fn(&mut Library), l: &Library) -> Library {
        let mut t = l.clone();
        f(&mut t);
        t
    }

    /// The fixed ids the proof uses, minted from the seed so the state space
    /// stays finite under the `cfg(test)` bounds.
    struct Ids {
        t0: TrackId,
        t1: TrackId,
        album: AlbumId,
        artist: ArtistId,
    }

    impl Ids {
        fn new(cat: &Catalogue) -> Ids {
            Ids {
                t0: cat.track_id("seed-track-0").unwrap(),
                t1: cat.track_id("seed-track-1").unwrap(),
                album: cat.album_id("seed-album-0").unwrap(),
                artist: cat.artist_id("seed-artist-0").unwrap(),
            }
        }
    }

    /// Every library operation, with a fixed id set and fixed names so the
    /// state space stays finite under the `cfg(test)` bounds.
    fn successors(l: &Library, ids: &Ids) -> Vec<Library> {
        let Ids { t0, t1, album, artist } = ids;
        let mut out = Vec::new();
        out.push(applying(|x| x.toggle_like(t0.clone()), l));
        out.push(applying(|x| x.toggle_like(t1.clone()), l));
        out.push(applying(|x| x.toggle_save_album(album.clone()), l));
        out.push(applying(|x| x.toggle_follow(artist.clone()), l));
        out.push(applying(|x| x.toggle_download(album.clone()), l));
        out.push(applying(|x| x.record_play(t0.clone()), l));
        out.push(applying(|x| x.record_play(t1.clone()), l));

        out.push(applying(|x| {
            x.create_playlist("A".to_string());
        }, l));
        out.push(applying(|x| {
            x.create_playlist("B".to_string());
        }, l));
        out.push(applying(|x| {
            x.rename_playlist(PlaylistId(0), "A".to_string());
        }, l));
        out.push(applying(|x| {
            x.rename_playlist(PlaylistId(0), "B".to_string());
        }, l));
        out.push(applying(|x| {
            x.delete_playlist(PlaylistId(0));
        }, l));
        out.push(applying(|x| {
            x.delete_playlist(PlaylistId(1));
        }, l));
        out.push(applying(|x| {
            x.add_track(PlaylistId(0), t0.clone());
        }, l));
        out.push(applying(|x| {
            x.add_track(PlaylistId(0), t1.clone());
        }, l));
        out.push(applying(|x| {
            x.remove_track(PlaylistId(0), t0);
        }, l));
        out.push(applying(|x| {
            x.move_track(PlaylistId(0), 0, 1);
        }, l));

        out.push(applying(|x| {
            x.create_collection("A".to_string());
        }, l));
        out.push(applying(|x| {
            x.create_collection("B".to_string());
        }, l));
        out.push(applying(|x| {
            x.delete_collection(0);
        }, l));
        out.push(applying(|x| {
            x.rename_collection(0, "A".to_string());
        }, l));
        out.push(applying(|x| {
            x.rename_collection(0, "B".to_string());
        }, l));
        out.push(applying(|x| {
            x.add_to_collection(0, PlaylistId(0));
        }, l));
        out.push(applying(|x| {
            x.remove_from_collection(0, PlaylistId(0));
        }, l));
        out
    }

    /// Exhaustively explores every reachable library state and asserts the
    /// invariant at each. This is the library half of the proof; `state.rs`
    /// proves the nav half.
    #[test]
    fn every_reachable_library_satisfies_check() {
        let cat = Catalogue::seed();
        let ids = Ids::new(&cat);
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        let init = Library::default();
        assert_eq!(init.check(&cat), Ok(()));
        seen.insert(init.clone());
        queue.push_back(init);
        while let Some(l) = queue.pop_front() {
            assert_eq!(l.check(&cat), Ok(()), "library broke the invariant: {l:?}");
            for next in successors(&l, &ids) {
                if seen.insert(next.clone()) {
                    queue.push_back(next);
                }
            }
        }
        assert!(seen.len() > 1000, "library space was under-explored: {}", seen.len());
    }

    /// `check` rejects each way a reference can go bad, so the proof is testing
    /// a checker that actually detects violations.
    #[test]
    fn check_detects_violations() {
        // A real id the seed lacks: minted by a larger catalogue, checked against
        // the seed. No test-only constructor exists, so this is the only way.
        let seed = Catalogue::seed();
        let mut bigger = Catalogue::seed();
        bigger.absorb(CatalogueRows {
            artists: vec![ArtistRow { id: "x-artist".into(), name: "X".into() }],
            albums: vec![AlbumRow { id: "x-album".into(), title: "X".into(), artist: "x-artist".into() }],
            tracks: vec![TrackRow {
                id: "x-track".into(),
                title: "X".into(),
                artist: "x-artist".into(),
                album: "x-album".into(),
                duration: "PT1M".into(),
            }],
            editorial: Vec::new(),
        });
        let track = bigger.track_id("x-track").unwrap();
        let album = bigger.album_id("x-album").unwrap();
        let artist = bigger.artist_id("x-artist").unwrap();

        let l = Library { liked: BTreeSet::from([track.clone()]), ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::LikedNotInCatalogue));
        assert_eq!(l.check(&bigger), Ok(()));

        let l = Library { saved_albums: BTreeSet::from([album.clone()]), ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::SavedAlbumNotInCatalogue));

        let l = Library { followed: BTreeSet::from([artist]), ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::FollowedNotInCatalogue));

        let l = Library { downloaded: BTreeSet::from([album]), ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::DownloadedNotInCatalogue));

        let l = Library { history: vec![track.clone()], ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::HistoryNotInCatalogue));

        let mut l = Library::default();
        let id = l.create_playlist("X".to_string()).unwrap();
        l.add_track(id, track);
        assert_eq!(l.check(&seed), Err(Violation::PlaylistTrackNotInCatalogue));
    }

    /// Renaming actually changes the name. The invariant does not constrain
    /// names, so this is the only coverage of the three rename operations.
    #[test]
    fn rename_changes_the_name() {
        let mut l = Library::default();
        let id = l.create_playlist("Old".to_string()).unwrap();
        assert!(l.rename_playlist(id, "New".to_string()));
        assert_eq!(l.playlist(id).unwrap().name, "New");

        l.create_collection("OldC".to_string());
        assert!(l.rename_collection(0, "NewC".to_string()));
        assert_eq!(l.collections[0].name, "NewC");

        l.set_name("Me".to_string());
        assert_eq!(l.name, "Me");
    }
}
