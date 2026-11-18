//! The single local user's mutable data and its operations. Rune-free and
//! nav-free: every operation validates its slots, and [`Library::check`] proves
//! no reference outside the catalogue and no dangling slot survives.
//!
//! Bounds shrink under `cfg(test)` so the exhaustive proof in `state.rs` stays
//! finite; the runtime uses the real sizes. The random-walk test runs the full
//! sizes and calls [`Library::check`] after every step.

use std::collections::BTreeSet;

use crate::catalogue::{AlbumId, ArtistId, Catalogue, CatalogueRows, CataloguePlaylistId, TrackId};

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
    /// Playlists the account owns or favorites on Tidal. Read-only here: the
    /// content lives in the catalogue, and no edit method takes this id type.
    pub tidal_playlists: BTreeSet<CataloguePlaylistId>,
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
            tidal_playlists: BTreeSet::new(),
            downloaded: BTreeSet::new(),
            history: Vec::new(),
            playlists: std::array::from_fn(|_| None),
            collections: Vec::new(),
        }
    }
}

/// A complete read of one account's Tidal library, as unvalidated text in the
/// shape Tidal sends. Only a fetch that read every page returns one, so a value
/// of this type is the proof that the read was complete.
#[cfg_attr(not(test), expect(dead_code, reason = "App::sync applies it once the worker lands"))]
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct LibraryRows {
    pub catalogue: CatalogueRows,
    pub liked: Vec<String>,
    pub saved_albums: Vec<String>,
    pub followed: Vec<String>,
    pub tidal_playlists: Vec<String>,
}

/// What one sync left in the library, for the Account screen.
#[cfg_attr(not(test), expect(dead_code, reason = "App::sync applies it once the worker lands"))]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct SyncReport {
    pub liked: usize,
    pub saved_albums: usize,
    pub followed: usize,
    pub tidal_playlists: usize,
    /// Catalogue rows that absorb dropped, plus members that name nothing the
    /// catalogue holds after the absorb.
    pub dropped: usize,
}

/// The members of one list that the catalogue resolves. Each one it cannot
/// resolve is counted in `dropped`.
#[cfg_attr(not(test), expect(dead_code, reason = "App::sync applies it once the worker lands"))]
fn resolve<T: Ord>(raw: Vec<String>, id: impl Fn(&str) -> Option<T>, dropped: &mut usize) -> BTreeSet<T> {
    let mut set = BTreeSet::new();
    for member in raw {
        match id(&member) {
            Some(id) => {
                set.insert(id);
            }
            None => *dropped += 1,
        }
    }
    set
}

/// A safety violation, named so the random-walk test reports what broke.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Violation {
    LikedNotInCatalogue,
    SavedAlbumNotInCatalogue,
    FollowedNotInCatalogue,
    TidalPlaylistNotInCatalogue,
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

    // --- Sync -------------------------------------------------------------

    /// Absorb the rows into `cat`, then replace likes, saves, follows and Tidal
    /// playlists with the members that resolve. Downloads, history, user
    /// playlists, collections and the name are untouched. The same rows applied
    /// twice change nothing. A member that does not resolve is counted, not kept.
    #[cfg_attr(not(test), expect(dead_code, reason = "App::sync applies it once the worker lands"))]
    pub fn sync(&mut self, cat: &mut Catalogue, rows: LibraryRows) -> SyncReport {
        let mut dropped = cat.absorb(rows.catalogue);
        self.liked = resolve(rows.liked, |raw| cat.track_id(raw), &mut dropped);
        self.saved_albums = resolve(rows.saved_albums, |raw| cat.album_id(raw), &mut dropped);
        self.followed = resolve(rows.followed, |raw| cat.artist_id(raw), &mut dropped);
        self.tidal_playlists = resolve(rows.tidal_playlists, |raw| cat.playlist_id(raw), &mut dropped);
        SyncReport {
            liked: self.liked.len(),
            saved_albums: self.saved_albums.len(),
            followed: self.followed.len(),
            tidal_playlists: self.tidal_playlists.len(),
            dropped,
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
        if !self.tidal_playlists.iter().all(|p| cat.playlist(p).is_some()) {
            return Err(Violation::TidalPlaylistNotInCatalogue);
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
    use crate::catalogue::{AlbumRow, ArtistRow, CatalogueRows, PlaylistRow, TrackRow};
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
    /// A track the seed lacks, with its parents, so a sync successor proves that
    /// a member a sync brings in is a member of the catalogue the check reads.
    fn fetched_rows() -> CatalogueRows {
        CatalogueRows {
            artists: vec![ArtistRow { id: "x-artist".into(), name: "X".into() }],
            albums: vec![AlbumRow { id: "x-album".into(), title: "X".into(), artist: "x-artist".into() }],
            tracks: vec![TrackRow {
                id: "x-track".into(),
                title: "X".into(),
                artist: "x-artist".into(),
                album: "x-album".into(),
                duration: "PT1M".into(),
            }],
            playlists: Vec::new(),
        }
    }

    fn successors(l: &Library, ids: &Ids, cat: &mut Catalogue) -> Vec<Library> {
        let Ids { t0, t1, album, artist } = ids;
        let mut out = Vec::new();
        let mut synced = l.clone();
        synced.sync(
            cat,
            LibraryRows {
                catalogue: fetched_rows(),
                liked: vec!["seed-track-1".into(), "x-track".into()],
                tidal_playlists: vec!["seed-playlist-0".into()],
                ..LibraryRows::default()
            },
        );
        out.push(synced);
        let mut emptied = l.clone();
        emptied.sync(cat, LibraryRows::default());
        out.push(emptied);
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
        let mut cat = Catalogue::seed();
        let ids = Ids::new(&cat);
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        let init = Library::default();
        assert_eq!(init.check(&cat), Ok(()));
        seen.insert(init.clone());
        queue.push_back(init);
        while let Some(l) = queue.pop_front() {
            assert_eq!(l.check(&cat), Ok(()), "library broke the invariant: {l:?}");
            for next in successors(&l, &ids, &mut cat) {
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
            playlists: vec![PlaylistRow {
                id: "x-playlist".into(),
                name: "X".into(),
                description: String::new(),
                tracks: vec!["x-track".into()],
            }],
        });
        let track = bigger.track_id("x-track").unwrap();
        let playlist = bigger.playlists().find(|p| p.id.as_str() == "x-playlist").unwrap().id.clone();
        let album = bigger.album_id("x-album").unwrap();
        let artist = bigger.artist_id("x-artist").unwrap();

        let l = Library { liked: BTreeSet::from([track.clone()]), ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::LikedNotInCatalogue));
        assert_eq!(l.check(&bigger), Ok(()));

        let l = Library { saved_albums: BTreeSet::from([album.clone()]), ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::SavedAlbumNotInCatalogue));

        let l = Library { followed: BTreeSet::from([artist]), ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::FollowedNotInCatalogue));

        let l = Library { tidal_playlists: BTreeSet::from([playlist]), ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::TidalPlaylistNotInCatalogue));
        assert_eq!(l.check(&bigger), Ok(()));

        let l = Library { downloaded: BTreeSet::from([album]), ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::DownloadedNotInCatalogue));

        let l = Library { history: vec![track.clone()], ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::HistoryNotInCatalogue));

        let mut l = Library::default();
        let id = l.create_playlist("X".to_string()).unwrap();
        l.add_track(id, track);
        assert_eq!(l.check(&seed), Err(Violation::PlaylistTrackNotInCatalogue));
    }

    /// A library with a row in every local field and every mirrored one.
    fn full_library(cat: &Catalogue) -> Library {
        let mut lib = Library::default();
        lib.set_name("Ada".to_string());
        lib.toggle_download(cat.album_id("seed-album-2").unwrap());
        lib.record_play(cat.track_id("seed-track-5").unwrap());
        let id = lib.create_playlist("Mine".to_string()).unwrap();
        lib.add_track(id, cat.track_id("seed-track-6").unwrap());
        lib.create_collection("Folder".to_string());
        lib.add_to_collection(0, id);
        lib.toggle_like(cat.track_id("seed-track-0").unwrap());
        lib.toggle_save_album(cat.album_id("seed-album-0").unwrap());
        lib.toggle_follow(cat.artist_id("seed-artist-0").unwrap());
        lib.tidal_playlists.insert(cat.playlist_id("seed-playlist-0").unwrap());
        lib
    }

    /// A sync replaces only what Tidal owns. Downloads, history, user playlists,
    /// collections and the name are local, and Tidal cannot return them.
    #[test]
    fn a_sync_keeps_local_data() {
        let mut cat = Catalogue::seed();
        let before = full_library(&cat);
        let mut lib = before.clone();
        lib.sync(&mut cat, LibraryRows::default());
        assert_eq!(lib.name, before.name, "the sync changed the name");
        assert_eq!(lib.downloaded, before.downloaded, "the sync touched the downloads");
        assert_eq!(lib.history, before.history, "the sync touched the history");
        assert_eq!(lib.playlists, before.playlists, "the sync touched a user playlist");
        assert_eq!(lib.collections, before.collections, "the sync touched a collection");
    }

    /// A complete read is the truth for the mirrored memberships, so a member
    /// Tidal no longer lists is removed, and one it lists is kept.
    #[test]
    fn a_sync_removes_a_membership_tidal_no_longer_lists() {
        let mut cat = Catalogue::seed();
        let mut lib = full_library(&cat);
        let report = lib.sync(
            &mut cat,
            LibraryRows {
                liked: vec!["seed-track-1".into()],
                saved_albums: vec!["seed-album-1".into()],
                followed: vec!["seed-artist-1".into()],
                tidal_playlists: vec!["seed-playlist-2".into()],
                ..LibraryRows::default()
            },
        );
        assert_eq!(lib.liked, BTreeSet::from([cat.track_id("seed-track-1").unwrap()]));
        assert_eq!(lib.saved_albums, BTreeSet::from([cat.album_id("seed-album-1").unwrap()]));
        assert_eq!(lib.followed, BTreeSet::from([cat.artist_id("seed-artist-1").unwrap()]));
        assert_eq!(lib.tidal_playlists, BTreeSet::from([cat.playlist_id("seed-playlist-2").unwrap()]));
        assert_eq!(
            report,
            SyncReport { liked: 1, saved_albums: 1, followed: 1, tidal_playlists: 1, dropped: 0 }
        );
    }

    /// The same rows applied twice leave the same library, so a retried sync is
    /// harmless.
    #[test]
    fn a_sync_twice_changes_nothing() {
        let mut cat = Catalogue::seed();
        let rows = LibraryRows {
            catalogue: fetched_rows(),
            liked: vec!["seed-track-1".into(), "x-track".into()],
            saved_albums: vec!["x-album".into()],
            followed: vec!["x-artist".into()],
            tidal_playlists: vec!["seed-playlist-1".into()],
        };
        let mut lib = full_library(&cat);
        let first = lib.sync(&mut cat, rows.clone());
        let once = lib.clone();
        let second = lib.sync(&mut cat, rows);
        assert_eq!(lib, once, "a second sync with the same rows changed the library");
        assert_eq!(first, second);
        assert_eq!(lib.liked.len(), 2);
    }

    /// A member the catalogue cannot resolve is counted and left out, so the
    /// library never holds an id outside the catalogue.
    #[test]
    fn an_unresolvable_member_is_counted_not_kept() {
        let mut cat = Catalogue::seed();
        let mut lib = Library::default();
        let report = lib.sync(
            &mut cat,
            LibraryRows {
                liked: vec!["seed-track-0".into(), "404".into()],
                tidal_playlists: vec!["gone".into()],
                ..LibraryRows::default()
            },
        );
        assert_eq!(report.dropped, 2, "an unresolvable member was not counted");
        assert_eq!(report.liked, 1);
        assert!(lib.tidal_playlists.is_empty());
        assert_eq!(lib.check(&cat), Ok(()));
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
