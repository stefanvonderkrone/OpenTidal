//! The single local user's mutable data and its operations. Rune-free and
//! nav-free: every operation validates its slots, and [`Library::check`] proves
//! no reference outside the catalogue and no dangling slot survives.
//!
//! Bounds shrink under `cfg(test)` so the exhaustive proof in `state.rs` stays
//! finite; the runtime uses the real sizes. The random-walk test runs the full
//! sizes and calls [`Library::check`] after every step.

use std::collections::BTreeSet;

use crate::catalogue::{AlbumId, ArtistId, Catalogue, CatalogueRows, CataloguePlaylistId, TrackId};
use crate::state::Route;

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

/// An ordered, duplicate-free list of ids, in Tidal's order. The fields are
/// private, so no caller can insert a duplicate. `index` holds the same ids as
/// `order`, so `contains` stays logarithmic for a screen that asks per row.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Members<T> {
    order: Vec<T>,
    index: BTreeSet<T>,
}

impl<T> Default for Members<T> {
    fn default() -> Self {
        Members { order: Vec::new(), index: BTreeSet::new() }
    }
}

impl<T> std::ops::Deref for Members<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.order
    }
}

impl<'a, T> IntoIterator for &'a Members<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.order.iter()
    }
}

/// Whether a confirmed list is the whole remote list or only its first pages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    Partial,
    Complete,
}

impl<T: Ord + Clone> Members<T> {
    /// Keeps the first occurrence of each id.
    pub fn from_ordered(ids: impl IntoIterator<Item = T>) -> Self {
        let mut members = Members::default();
        for id in ids {
            members.insert(id);
        }
        members
    }

    pub fn contains(&self, id: &T) -> bool {
        self.index.contains(id)
    }

    /// Append an id that is not yet a member. Returns whether it was added.
    pub fn insert(&mut self, id: T) -> bool {
        if !self.index.insert(id.clone()) {
            return false;
        }
        self.order.push(id);
        true
    }

    /// Returns whether the id was a member.
    pub fn remove(&mut self, id: &T) -> bool {
        if !self.index.remove(id) {
            return false;
        }
        self.order.retain(|x| x != id);
        true
    }

    /// Remove a member, or add a new one at the front, because Tidal lists a
    /// collection newest first.
    pub fn toggle(&mut self, id: T) {
        if !self.remove(&id) {
            self.index.insert(id.clone());
            self.order.insert(0, id);
        }
    }

    /// Apply a confirmed list. `Partial` keeps every member the confirmed list
    /// omits, after it, so it adds and reorders but never removes. `Complete`
    /// makes the members exactly the confirmed list. Applying the same list
    /// twice changes nothing. Returns whether the members changed.
    pub fn merge(&mut self, confirmed: &[T], reach: Reach) -> bool {
        let mut next = Members::from_ordered(confirmed.iter().cloned());
        if reach == Reach::Partial {
            for id in &self.order {
                next.insert(id.clone());
            }
        }
        let changed = next != *self;
        *self = next;
        changed
    }
}

/// One remote list that a paged read fills. Not `Collection`, which names a
/// folder of user playlists.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum RemoteList {
    Liked,
    Albums,
    Artists,
    Playlists,
    /// The tracks of one Tidal playlist. Never a seed playlist.
    Playlist(CataloguePlaylistId),
}

impl RemoteList {
    /// The remote list the route shows, if it shows one. A seed playlist is
    /// not on Tidal, so it has none.
    pub fn of(route: Option<&Route>) -> Option<RemoteList> {
        match route? {
            Route::LikedSongs => Some(RemoteList::Liked),
            Route::Albums => Some(RemoteList::Albums),
            Route::Artists => Some(RemoteList::Artists),
            Route::Playlists => Some(RemoteList::Playlists),
            Route::CataloguePlaylist(id) if !id.is_seed() => Some(RemoteList::Playlist(id.clone())),
            _ => None,
        }
    }
}

/// One landed page of one remote list, ready to apply. `confirmed` is every
/// member id the pass has read so far, page one first, as raw text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merge {
    pub list: RemoteList,
    pub rows: CatalogueRows,
    pub confirmed: Vec<String>,
    pub reach: Reach,
}

/// What one merge left in the list it named.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct MergeReport {
    pub members: usize,
    /// Catalogue rows that absorb dropped, plus confirmed ids that name nothing
    /// the catalogue holds after the absorb.
    pub dropped: usize,
}

/// The single local user's data. `downloaded` is a `BTreeSet`, not a `HashSet`,
/// because a `BTreeSet` is `Hash`, so `Library` and `State` stay hashable for
/// the proof.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Library {
    pub name: String,
    pub liked: Members<TrackId>,
    pub saved_albums: Members<AlbumId>,
    pub followed: Members<ArtistId>,
    /// Playlists the account owns or favorites on Tidal. Read-only here: the
    /// content lives in the catalogue, and no edit method takes this id type.
    pub tidal_playlists: Members<CataloguePlaylistId>,
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
            liked: Members::default(),
            saved_albums: Members::default(),
            followed: Members::default(),
            tidal_playlists: Members::default(),
            downloaded: BTreeSet::new(),
            history: Vec::new(),
            playlists: std::array::from_fn(|_| None),
            collections: Vec::new(),
        }
    }
}

/// The members of one list that the catalogue resolves, in order. Each one it
/// cannot resolve is counted in `dropped`.
fn resolve<T>(raw: Vec<String>, id: impl Fn(&str) -> Option<T>, dropped: &mut usize) -> Vec<T> {
    let resolved: Vec<T> = raw.iter().filter_map(|member| id(member)).collect();
    *dropped += raw.len() - resolved.len();
    resolved
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
        self.liked.toggle(t);
    }
    pub fn is_liked(&self, t: &TrackId) -> bool {
        self.liked.contains(t)
    }

    pub fn toggle_save_album(&mut self, a: AlbumId) {
        self.saved_albums.toggle(a);
    }
    pub fn is_saved_album(&self, a: &AlbumId) -> bool {
        self.saved_albums.contains(a)
    }

    pub fn toggle_follow(&mut self, a: ArtistId) {
        self.followed.toggle(a);
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

    // --- Merge ------------------------------------------------------------

    /// Absorb the page's rows into `cat`, then merge the confirmed ids that
    /// resolve into the one list `m.list` names. The tracks of a Tidal
    /// playlist live in the catalogue, so `RemoteList::Playlist` merges there.
    /// Downloads, history, user playlists, collections and the name are
    /// untouched. A confirmed id that does not resolve is counted, not kept.
    pub fn merge(&mut self, cat: &mut Catalogue, m: Merge) -> MergeReport {
        fn into<T: Ord + Clone>(
            members: &mut Members<T>,
            confirmed: Vec<String>,
            id: impl Fn(&str) -> Option<T>,
            reach: Reach,
            dropped: &mut usize,
        ) -> usize {
            members.merge(&resolve(confirmed, id, dropped), reach);
            members.len()
        }
        let mut dropped = cat.absorb(m.rows);
        let members = match &m.list {
            RemoteList::Liked => into(&mut self.liked, m.confirmed, |raw| cat.track_id(raw), m.reach, &mut dropped),
            RemoteList::Albums => {
                into(&mut self.saved_albums, m.confirmed, |raw| cat.album_id(raw), m.reach, &mut dropped)
            }
            RemoteList::Artists => {
                into(&mut self.followed, m.confirmed, |raw| cat.artist_id(raw), m.reach, &mut dropped)
            }
            RemoteList::Playlists => {
                into(&mut self.tidal_playlists, m.confirmed, |raw| cat.playlist_id(raw), m.reach, &mut dropped)
            }
            RemoteList::Playlist(id) => match cat.merge_tracks(id, &m.confirmed, m.reach) {
                Some(unresolved) => {
                    dropped += unresolved;
                    cat.playlist(id).map_or(0, |p| p.tracks.len())
                }
                None => {
                    dropped += m.confirmed.len();
                    0
                }
            },
        };
        MergeReport { members, dropped }
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
    /// A track the seed lacks, with its parents, so a merge successor proves that
    /// a member a page brings in is a member of the catalogue the check reads.
    fn fetched_rows() -> CatalogueRows {
        CatalogueRows {
            artists: vec![ArtistRow { id: "x-artist".into(), name: "X".into() }],
            albums: vec![AlbumRow { id: "x-album".into(), title: "X".into(), artist: "x-artist".into(), cover: String::new() }],
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
        let merge = |list: RemoteList, rows: CatalogueRows, confirmed: &[&str], reach: Reach| Merge {
            list,
            rows,
            confirmed: confirmed.iter().map(|id| id.to_string()).collect(),
            reach,
        };
        for page in [
            merge(RemoteList::Liked, fetched_rows(), &["seed-track-1", "x-track"], Reach::Complete),
            merge(RemoteList::Playlists, CatalogueRows::default(), &["seed-playlist-0"], Reach::Partial),
            merge(RemoteList::Liked, CatalogueRows::default(), &[], Reach::Complete),
        ] {
            let mut merged = l.clone();
            merged.merge(cat, page);
            out.push(merged);
        }
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
            albums: vec![AlbumRow { id: "x-album".into(), title: "X".into(), artist: "x-artist".into(), cover: String::new() }],
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
                cover: String::new(),
            }],
        });
        let track = bigger.track_id("x-track").unwrap();
        let playlist = bigger.playlists().find(|p| p.id.as_str() == "x-playlist").unwrap().id.clone();
        let album = bigger.album_id("x-album").unwrap();
        let artist = bigger.artist_id("x-artist").unwrap();

        let l = Library { liked: Members::from_ordered([track.clone()]), ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::LikedNotInCatalogue));
        assert_eq!(l.check(&bigger), Ok(()));

        let l = Library { saved_albums: Members::from_ordered([album.clone()]), ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::SavedAlbumNotInCatalogue));

        let l = Library { followed: Members::from_ordered([artist]), ..Default::default() };
        assert_eq!(l.check(&seed), Err(Violation::FollowedNotInCatalogue));

        let l = Library { tidal_playlists: Members::from_ordered([playlist]), ..Default::default() };
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

    /// A member the catalogue cannot resolve is counted and left out, so the
    /// library never holds an id outside the catalogue.
    #[test]
    fn an_unresolvable_member_is_counted_not_kept() {
        let mut cat = Catalogue::seed();
        let mut lib = Library::default();
        let page = |list: RemoteList, confirmed: &[&str]| Merge {
            list,
            rows: CatalogueRows::default(),
            confirmed: confirmed.iter().map(|id| id.to_string()).collect(),
            reach: Reach::Complete,
        };
        let liked = lib.merge(&mut cat, page(RemoteList::Liked, &["seed-track-0", "404"]));
        let playlists = lib.merge(&mut cat, page(RemoteList::Playlists, &["gone"]));
        assert_eq!(liked.dropped + playlists.dropped, 2, "an unresolvable member was not counted");
        assert_eq!(lib.liked.len(), 1);
        assert!(lib.tidal_playlists.is_empty());
        assert_eq!(lib.check(&cat), Ok(()));
    }

    fn tracks(cat: &Catalogue, ns: &[usize]) -> Vec<TrackId> {
        ns.iter().map(|n| cat.track_id(&format!("seed-track-{n}")).unwrap()).collect()
    }

    /// A partial list is only the first pages, so a member it omits may sit on a
    /// later page. It is kept after the confirmed list, a local like included.
    #[test]
    fn a_partial_merge_adds_and_reorders_but_never_removes() {
        let cat = Catalogue::seed();
        let mut lib = Library::default();
        lib.liked.merge(&tracks(&cat, &[1, 2]), Reach::Complete);
        lib.toggle_like(tracks(&cat, &[9])[0].clone());
        assert_eq!(lib.liked.to_vec(), tracks(&cat, &[9, 1, 2]));
        assert!(lib.liked.merge(&tracks(&cat, &[2, 3]), Reach::Partial));
        assert_eq!(lib.liked.to_vec(), tracks(&cat, &[2, 3, 9, 1]), "a partial merge removed or misplaced a member");
    }

    /// A complete list is the whole remote list, so it is the only merge that
    /// removes, and it removes exactly what the list omits.
    #[test]
    fn a_complete_merge_removes_exactly_the_omitted_members() {
        let cat = Catalogue::seed();
        let mut lib = Library::default();
        lib.liked.merge(&tracks(&cat, &[1, 2, 3, 4]), Reach::Complete);
        lib.liked.merge(&tracks(&cat, &[3, 1, 5]), Reach::Complete);
        assert_eq!(lib.liked.to_vec(), tracks(&cat, &[3, 1, 5]));
    }

    /// A retried page lands twice. The second landing changes nothing and says so.
    #[test]
    fn the_same_merge_twice_changes_nothing() {
        let cat = Catalogue::seed();
        for reach in [Reach::Partial, Reach::Complete] {
            let mut lib = Library::default();
            lib.liked.merge(&tracks(&cat, &[1, 2]), Reach::Complete);
            lib.toggle_like(tracks(&cat, &[9])[0].clone());
            let confirmed = tracks(&cat, &[2, 3]);
            lib.liked.merge(&confirmed, reach);
            let once = lib.clone();
            assert!(!lib.liked.merge(&confirmed, reach), "{reach:?} reported a change on a repeat");
            assert_eq!(lib, once, "a repeated {reach:?} merge changed the library");
        }
    }

    fn liked_page(confirmed: &[&str], reach: Reach) -> Merge {
        Merge {
            list: RemoteList::Liked,
            rows: CatalogueRows::default(),
            confirmed: confirmed.iter().map(|s| s.to_string()).collect(),
            reach,
        }
    }

    /// A partial page is only the start of the list, so a local like it omits
    /// may sit on a later page and survives. Only a complete read removes it.
    /// Local data and the name survive both.
    #[test]
    fn a_local_like_survives_a_partial_merge_and_only_a_complete_merge_removes_it() {
        let mut cat = Catalogue::seed();
        let before = full_library(&cat);
        let mut lib = before.clone();
        let local = cat.track_id("seed-track-0").unwrap();
        assert!(lib.is_liked(&local));

        let report = lib.merge(&mut cat, liked_page(&["seed-track-1", "404"], Reach::Partial));
        assert_eq!(lib.liked.to_vec(), tracks(&cat, &[1, 0]), "a partial merge dropped the local like");
        assert_eq!(report, MergeReport { members: 2, dropped: 1 });

        let report = lib.merge(&mut cat, liked_page(&["seed-track-1"], Reach::Complete));
        assert_eq!(lib.liked.to_vec(), tracks(&cat, &[1]), "a complete merge kept a like Tidal does not list");
        assert_eq!(report, MergeReport { members: 1, dropped: 0 });

        assert_eq!(lib.name, before.name, "a merge changed the name");
        assert_eq!(lib.downloaded, before.downloaded, "a merge touched the downloads");
        assert_eq!(lib.history, before.history, "a merge touched the history");
        assert_eq!(lib.playlists, before.playlists, "a merge touched a user playlist");
        assert_eq!(lib.collections, before.collections, "a merge touched a collection");
        assert_eq!(lib.saved_albums, before.saved_albums, "a liked page touched the saved albums");
    }

    /// The tracks of a Tidal playlist live in the catalogue. A track page
    /// replaces them there, adds the tracks it brings, and leaves the
    /// playlist's name and cover and every library list alone.
    #[test]
    fn a_playlist_track_page_changes_its_tracks_and_not_its_name_or_cover() {
        let mut cat = Catalogue::seed();
        cat.absorb(CatalogueRows {
            playlists: vec![PlaylistRow {
                id: "x-playlist".into(),
                name: "Evening".into(),
                description: String::new(),
                tracks: vec!["seed-track-0".into()],
                cover: "x-cover".into(),
            }],
            ..CatalogueRows::default()
        });
        let id = cat.playlist_id("x-playlist").unwrap();
        let cover = cat.playlist(&id).unwrap().cover.clone();
        assert!(cover.is_some());
        let before = full_library(&cat);
        let mut lib = before.clone();

        let report = lib.merge(
            &mut cat,
            Merge {
                list: RemoteList::Playlist(id.clone()),
                rows: fetched_rows(),
                confirmed: vec!["x-track".into(), "seed-track-3".into()],
                reach: Reach::Complete,
            },
        );

        let playlist = cat.playlist(&id).unwrap();
        let x_track = cat.track_id("x-track").unwrap();
        assert_eq!(playlist.tracks.to_vec(), [vec![x_track], tracks(&cat, &[3])].concat());
        assert_eq!(playlist.name, "Evening", "a track page renamed the playlist");
        assert_eq!(playlist.cover, cover, "a track page changed the cover");
        assert_eq!(report, MergeReport { members: 2, dropped: 0 });
        assert_eq!(lib, before, "a playlist track page changed the library");
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
