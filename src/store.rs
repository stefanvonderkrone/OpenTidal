//! The SQLite store for each user's library. Rune-free: rows map directly to
//! the library model, and load checks the invariant before it returns data.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::catalog::{self, TrackId};
use crate::library::{
    COLLECTION_PLAYLIST_CAP, Collection, HISTORY_CAP, Library, MAX_COLLECTIONS, MAX_PLAYLISTS,
    PLAYLIST_TRACK_CAP, Playlist, PlaylistId,
};

const SCHEMA_VERSION: u32 = 1;

/// A database that stores libraries by user id.
pub struct Store {
    conn: Connection,
}

/// One account's library as read from disk, with the number of rows the
/// catalogue could no longer resolve.
#[derive(Debug, PartialEq, Eq)]
pub struct Loaded {
    pub lib: Library,
    pub dropped: usize,
}

/// A database or filesystem failure.
#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    /// SQLite rejected an operation or stored data.
    Sql(String),
    /// The filesystem rejected an operation.
    Io(String),
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error.to_string())
    }
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl Store {
    /// Open or create the database and its parent directory.
    /// Set mode 0600, apply the pragmas, and migrate the schema.
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        // Create privately before SQLite can create a journal with this mode.
        // `truncate(false)` is required: this is a database, not a scratch file,
        // so an existing file must keep its contents.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        exclude_from_backup(path)?;
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::configure(conn)
    }

    /// An in-memory database, for tests.
    #[cfg(test)]
    pub fn in_memory() -> Result<Store, StoreError> {
        Self::configure(Connection::open_in_memory()?)
    }

    fn configure(mut conn: Connection) -> Result<Store, StoreError> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => {
                let tx = conn.transaction()?;
                tx.execute_batch(
                    "CREATE TABLE user(id TEXT PRIMARY KEY, name TEXT NOT NULL,
                        updated_at INTEGER NOT NULL);
                    CREATE TABLE \"like\"(user_id TEXT NOT NULL REFERENCES user,
                        track_id INTEGER NOT NULL, PRIMARY KEY(user_id, track_id));
                    CREATE TABLE save(user_id TEXT NOT NULL REFERENCES user,
                        album_id INTEGER NOT NULL, PRIMARY KEY(user_id, album_id));
                    CREATE TABLE follow(user_id TEXT NOT NULL REFERENCES user,
                        artist_id INTEGER NOT NULL, PRIMARY KEY(user_id, artist_id));
                    CREATE TABLE download(user_id TEXT NOT NULL REFERENCES user,
                        album_id INTEGER NOT NULL, PRIMARY KEY(user_id, album_id));
                    CREATE TABLE playlist(slot INTEGER NOT NULL,
                        user_id TEXT NOT NULL REFERENCES user, name TEXT NOT NULL,
                        PRIMARY KEY(user_id, slot));
                    CREATE TABLE playlist_track(user_id TEXT NOT NULL REFERENCES user,
                        slot INTEGER NOT NULL, position INTEGER NOT NULL,
                        track_id INTEGER NOT NULL, PRIMARY KEY(user_id, slot, position),
                        FOREIGN KEY(user_id, slot) REFERENCES playlist(user_id, slot));
                    CREATE TABLE collection(idx INTEGER NOT NULL,
                        user_id TEXT NOT NULL REFERENCES user, name TEXT NOT NULL,
                        PRIMARY KEY(user_id, idx));
                    CREATE TABLE collection_playlist(user_id TEXT NOT NULL REFERENCES user,
                        idx INTEGER NOT NULL, position INTEGER NOT NULL,
                        playlist_slot INTEGER NOT NULL, PRIMARY KEY(user_id, idx, position),
                        FOREIGN KEY(user_id, idx) REFERENCES collection(user_id, idx));
                    CREATE TABLE history(user_id TEXT NOT NULL REFERENCES user,
                        position INTEGER NOT NULL, track_id INTEGER NOT NULL,
                        PRIMARY KEY(user_id, position));",
                )?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                tx.commit()?;
            }
            SCHEMA_VERSION => {}
            _ => {
                return Err(StoreError::Sql(format!(
                    "unsupported schema version {version}"
                )));
            }
        }
        Ok(Store { conn })
    }

    /// The stored library for a user, or `None` when the user row is absent.
    ///
    /// A leaf that names an id the catalogue no longer ships is dropped and
    /// counted. The container around it stays, so a playlist missing one track
    /// still appears with the tracks it has. A leaf that comes back on the next
    /// sync is recoverable, so dropping it costs nothing permanent. Only a file
    /// that cannot be read at all is an error.
    pub fn load(&self, user_id: &str) -> Result<Option<Loaded>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let name = tx
            .query_row("SELECT name FROM user WHERE id = ?1", [user_id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        let Some(name) = name else {
            return Ok(None);
        };
        let mut dropped = 0;
        // The database `idx` of each loaded collection, in the order they load.
        let mut collection_idx: Vec<usize> = Vec::new();
        let mut lib = Library {
            name,
            liked: load_bits(&tx, user_id, "like", "track_id", catalog::TRACKS.len(), &mut dropped)?,
            saved_albums: load_bits(&tx, user_id, "save", "album_id", catalog::ALBUMS.len(), &mut dropped)?,
            followed: load_bits(&tx, user_id, "follow", "artist_id", catalog::ARTISTS.len(), &mut dropped)?,
            downloaded: load_bits(&tx, user_id, "download", "album_id", catalog::ALBUMS.len(), &mut dropped)?,
            history: Vec::new(),
            playlists: std::array::from_fn(|_| None),
            collections: Vec::new(),
        };
        {
            let mut stmt = tx.prepare(
                "SELECT slot, name FROM playlist WHERE user_id = ?1 ORDER BY slot LIMIT ?2",
            )?;
            let mut rows = stmt.query(params![user_id, MAX_PLAYLISTS as i64])?;
            while let Some(row) = rows.next()? {
                let Ok(slot) = index(row.get(0)?, MAX_PLAYLISTS) else {
                    dropped += 1;
                    continue;
                };
                lib.playlists[slot] = Some(Playlist { name: row.get(1)?, tracks: Vec::new() });
            }
            dropped += count_beyond(&tx, "playlist", user_id, MAX_PLAYLISTS)?;
        }
        {
            let mut stmt = tx.prepare(
                "SELECT slot, track_id FROM playlist_track
                 WHERE user_id = ?1 ORDER BY slot, position",
            )?;
            let mut rows = stmt.query([user_id])?;
            while let Some(row) = rows.next()? {
                let Ok(slot) = index(row.get(0)?, MAX_PLAYLISTS) else {
                    dropped += 1;
                    continue;
                };
                let Some(playlist) = lib.playlists[slot].as_mut() else {
                    dropped += 1;
                    continue;
                };
                if playlist.tracks.len() >= PLAYLIST_TRACK_CAP {
                    dropped += 1;
                    continue;
                }
                let Ok(track) = index(row.get(1)?, catalog::TRACKS.len()) else {
                    dropped += 1;
                    continue;
                };
                let track = TrackId(track as u8);
                if playlist.tracks.contains(&track) {
                    dropped += 1;
                    continue;
                }
                playlist.tracks.push(track);
            }
        }
        {
            let mut stmt = tx.prepare(
                "SELECT idx, name FROM collection WHERE user_id = ?1 ORDER BY idx LIMIT ?2",
            )?;
            let mut rows = stmt.query(params![user_id, MAX_COLLECTIONS as i64])?;
            while let Some(row) = rows.next()? {
                // `LIMIT` already bounds the count, so the stored index only has
                // to be non-negative. It is what a child row names, so a gap must
                // not shift it.
                let Ok(raw) = usize::try_from(row.get::<_, i64>(0)?) else {
                    dropped += 1;
                    continue;
                };
                collection_idx.push(raw);
                lib.collections.push(Collection { name: row.get(1)?, playlists: Vec::new() });
            }
            dropped += count_beyond(&tx, "collection", user_id, MAX_COLLECTIONS)?;
        }
        {
            let mut stmt = tx.prepare(
                "SELECT idx, playlist_slot FROM collection_playlist
                 WHERE user_id = ?1 ORDER BY idx, position",
            )?;
            let mut rows = stmt.query([user_id])?;
            while let Some(row) = rows.next()? {
                let raw: i64 = row.get(0)?;
                let Some(idx) = usize::try_from(raw)
                    .ok()
                    .and_then(|raw| collection_idx.iter().position(|&i| i == raw))
                else {
                    dropped += 1;
                    continue;
                };
                let Ok(slot) = index(row.get(1)?, MAX_PLAYLISTS) else {
                    dropped += 1;
                    continue;
                };
                let collection = &mut lib.collections[idx];
                let playlist = PlaylistId(slot as u8);
                if lib.playlists[slot].is_none()
                    || collection.playlists.len() >= COLLECTION_PLAYLIST_CAP
                    || collection.playlists.contains(&playlist)
                {
                    dropped += 1;
                    continue;
                }
                collection.playlists.push(playlist);
            }
        }
        {
            let mut stmt = tx.prepare(
                "SELECT track_id FROM history WHERE user_id = ?1 ORDER BY position LIMIT ?2",
            )?;
            let mut rows = stmt.query(params![user_id, HISTORY_CAP as i64])?;
            while let Some(row) = rows.next()? {
                let Ok(track) = index(row.get(0)?, catalog::TRACKS.len()) else {
                    dropped += 1;
                    continue;
                };
                let track = TrackId(track as u8);
                if lib.history.contains(&track) {
                    dropped += 1;
                    continue;
                }
                lib.history.push(track);
            }
            dropped += count_beyond(&tx, "history", user_id, HISTORY_CAP)?;
        }
        tx.commit()?;
        // The boundary has already dropped every row that could break this. A
        // failure here means the loader has a hole, and a test catches it before
        // a user can.
        debug_assert!(lib.check().is_ok(), "load built a library that fails its own invariant");
        Ok(Some(Loaded { lib, dropped }))
    }

    /// Replace the stored library for a user in one transaction.
    pub fn write(&mut self, user_id: &str, lib: &Library) -> Result<(), StoreError> {
        lib.check()
            .map_err(|error| StoreError::Sql(format!("library invariant: {error:?}")))?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO user(id, name, updated_at) VALUES (?1, ?2, unixepoch())
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, updated_at = excluded.updated_at",
            params![user_id, lib.name],
        )?;
        for table in [
            "playlist_track",
            "collection_playlist",
            "playlist",
            "collection",
            "like",
            "save",
            "follow",
            "download",
            "history",
        ] {
            tx.execute(
                &format!("DELETE FROM \"{table}\" WHERE user_id = ?1"),
                [user_id],
            )?;
        }
        for (table, column, bits) in [
            ("like", "track_id", lib.liked),
            ("save", "album_id", lib.saved_albums),
            ("follow", "artist_id", lib.followed),
            ("download", "album_id", lib.downloaded),
        ] {
            let mut stmt = tx.prepare(&format!(
                "INSERT INTO \"{table}\"(user_id, {column}) VALUES (?1, ?2)"
            ))?;
            for id in 0..u32::BITS {
                if bits & (1 << id) != 0 {
                    stmt.execute(params![user_id, id])?;
                }
            }
        }
        for (slot, playlist) in lib.playlists.iter().enumerate() {
            let Some(playlist) = playlist else { continue };
            tx.execute(
                "INSERT INTO playlist(user_id, slot, name) VALUES (?1, ?2, ?3)",
                params![user_id, slot as i64, playlist.name],
            )?;
            for (position, track) in playlist.tracks.iter().enumerate() {
                tx.execute("INSERT INTO playlist_track(user_id, slot, position, track_id) VALUES (?1, ?2, ?3, ?4)",
                    params![user_id, slot as i64, position as i64, track.0])?;
            }
        }
        for (idx, collection) in lib.collections.iter().enumerate() {
            tx.execute(
                "INSERT INTO collection(user_id, idx, name) VALUES (?1, ?2, ?3)",
                params![user_id, idx as i64, collection.name],
            )?;
            for (position, playlist) in collection.playlists.iter().enumerate() {
                tx.execute("INSERT INTO collection_playlist(user_id, idx, position, playlist_slot) VALUES (?1, ?2, ?3, ?4)",
                    params![user_id, idx as i64, position as i64, playlist.0])?;
            }
        }
        for (position, track) in lib.history.iter().enumerate() {
            tx.execute(
                "INSERT INTO history(user_id, position, track_id) VALUES (?1, ?2, ?3)",
                params![user_id, position as i64, track.0],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The schema version this build writes.
    #[cfg(test)]
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }
}

/// Keep the database out of a Time Machine backup. The library comes back from
/// Tidal, and the file holds a name and a listening history, so a backup copies
/// a privacy risk for no gain.
///
/// This excludes the directory, not the file. In WAL mode SQLite keeps recent
/// rows in `opentidal.db-wal`, and an exclusion on the database alone would leave
/// them in the backup. It sets the extended attribute `tmutil addexclusion` sets,
/// rather than running `tmutil`, which takes eleven seconds here and this runs on
/// the startup path.
#[cfg(target_os = "macos")]
fn exclude_from_backup(path: &Path) -> Result<(), StoreError> {
    let Some(dir) = path.parent() else { return Ok(()) };
    let _ = std::process::Command::new("xattr")
        .args([
            "-w",
            "com.apple.metadata:com_apple_backup_excludeItem",
            "com.apple.backupd",
            &dir.to_string_lossy(),
        ])
        .status();
    // A sandbox that hides `xattr` must not stop the app opening its database.
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn exclude_from_backup(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

/// A catalogue index, rejected when it names nothing the catalogue ships.
fn index(value: i64, len: usize) -> Result<usize, ()> {
    if value < 0 || value as u64 >= len as u64 {
        return Err(());
    }
    Ok(value as usize)
}

/// How many rows a table holds beyond a cap, which load keeps the first of.
fn count_beyond(
    conn: &Connection,
    table: &str,
    user_id: &str,
    cap: usize,
) -> Result<usize, StoreError> {
    let total: i64 = conn.query_row(
        &format!("SELECT count(*) FROM \"{table}\" WHERE user_id = ?1"),
        [user_id],
        |row| row.get(0),
    )?;
    Ok((total.max(0) as usize).saturating_sub(cap))
}

fn load_bits(
    conn: &Connection,
    user_id: &str,
    table: &str,
    column: &str,
    len: usize,
    dropped: &mut usize,
) -> Result<u32, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {column} FROM \"{table}\" WHERE user_id = ?1"
    ))?;
    let mut rows = stmt.query([user_id])?;
    let mut bits = 0;
    while let Some(row) = rows.next()? {
        match index(row.get(0)?, len) {
            Ok(id) => bits |= 1 << id,
            Err(()) => *dropped += 1,
        }
    }
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{AlbumId, ArtistId};

    /// Opening a database keeps it out of a Time Machine backup.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_database_is_excluded_from_backups() {
        let dir = std::env::temp_dir().join(format!("opentidal-backup-{}", std::process::id()));
        let guard = TempDirGuard(dir.clone());
        let path = dir.join("opentidal.db");
        Store::open(&path).unwrap();

        // The directory is what is excluded, so the WAL and SHM files beside the
        // database stay out of the backup too.
        let out = std::process::Command::new("xattr")
            .args([
                "-p",
                "com.apple.metadata:com_apple_backup_excludeItem",
                &dir.to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("com.apple.backupd"),
            "the database directory is not excluded from backups"
        );
        drop(guard);
    }

    /// A temporary directory that removes itself, so a failing test leaves no
    /// database behind.
    struct TempDirGuard(std::path::PathBuf);

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The library from a load that dropped nothing, which is every load the
    /// round-trip tests expect.
    fn loaded_lib(store: &Store, user_id: &str) -> Option<Library> {
        let loaded = store.load(user_id).unwrap()?;
        assert_eq!(loaded.dropped, 0, "a round trip dropped rows");
        Some(loaded.lib)
    }

    fn populated(name: &str, track: u8) -> Library {
        let mut lib = Library::default();
        lib.set_name(name.into());
        let first = lib.create_playlist(format!("{name} first")).unwrap();
        let second = lib.create_playlist(format!("{name} second")).unwrap();
        lib.add_track(first, TrackId(track));
        lib.add_track(first, TrackId((track + 1) % catalog::TRACKS.len() as u8));
        lib.add_track(second, TrackId(track));
        lib.create_collection(format!("{name} collection"));
        lib.add_to_collection(0, second);
        lib.add_to_collection(0, first);
        lib.toggle_like(TrackId(track));
        lib.toggle_save_album(AlbumId(track % catalog::ALBUMS.len() as u8));
        lib.toggle_follow(ArtistId(track % catalog::ARTISTS.len() as u8));
        lib.toggle_download(AlbumId(track % catalog::ALBUMS.len() as u8));
        lib.record_play(TrackId(track));
        lib.record_play(TrackId((track + 1) % catalog::TRACKS.len() as u8));
        lib
    }

    /// A default library survives a round trip without changes.
    #[test]
    fn default_round_trips() {
        let mut store = Store::in_memory().unwrap();
        let lib = Library::default();
        store.write("a", &lib).unwrap();
        assert_eq!(loaded_lib(&store, "a"), Some(lib));
    }

    /// Real library operations produce a library that round-trips and passes the invariant.
    #[test]
    fn operations_round_trip() {
        let mut store = Store::in_memory().unwrap();
        let lib = populated("Ada", 3);
        store.write("a", &lib).unwrap();
        let loaded = loaded_lib(&store, "a").unwrap();
        assert_eq!(loaded, lib);
        assert_eq!(loaded.check(), Ok(()));
    }

    /// Interleaved round trips preserve each library in a deterministic random walk.
    #[test]
    fn random_walk_round_trips() {
        let mut store = Store::in_memory().unwrap();
        let mut lib = Library::default();
        let mut x: u64 = 0x853c49e6748fea9b;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for step in 0..2_000 {
            let playlist = PlaylistId((next() % MAX_PLAYLISTS as u64) as u8);
            let track = TrackId((next() % catalog::TRACKS.len() as u64) as u8);
            let album = AlbumId((next() % catalog::ALBUMS.len() as u64) as u8);
            match next() % 19 {
                0 => lib.toggle_like(track),
                1 => lib.toggle_save_album(album),
                2 => lib.toggle_follow(ArtistId((next() % catalog::ARTISTS.len() as u64) as u8)),
                3 => lib.toggle_download(album),
                4 => lib.remove_download(album),
                5 => lib.record_play(track),
                6 => {
                    lib.create_playlist(format!("P{step}"));
                }
                7 => {
                    lib.delete_playlist(playlist);
                }
                8 => {
                    lib.rename_playlist(playlist, format!("R{step}"));
                }
                9 => {
                    lib.add_track(playlist, track);
                }
                10 => {
                    lib.remove_track(playlist, track);
                }
                11 => {
                    lib.move_track(playlist, 0, 1);
                }
                12 => {
                    lib.create_collection(format!("C{step}"));
                }
                13 => {
                    lib.delete_collection(0);
                }
                14 => {
                    lib.rename_collection(0, format!("F{step}"));
                }
                15 => {
                    lib.add_to_collection(0, playlist);
                }
                16 => {
                    lib.remove_from_collection(0, playlist);
                }
                _ => lib.set_name(format!("U{step}")),
            }
            assert_eq!(lib.check(), Ok(()));
            if step % 17 == 0 || step == 1_999 {
                store.write("a", &lib).unwrap();
                let loaded = loaded_lib(&store, "a").unwrap();
                assert_eq!(loaded, lib, "step {step}");
                assert_eq!(loaded.check(), Ok(()));
                lib = loaded;
            }
        }
    }

    /// Empty playlist slots and collection references retain their exact indices.
    #[test]
    fn slot_hole_survives() {
        let mut store = Store::in_memory().unwrap();
        let mut lib = populated("Ada", 0);
        lib.delete_playlist(PlaylistId(0));
        store.write("a", &lib).unwrap();
        let loaded = loaded_lib(&store, "a").unwrap();
        assert!(loaded.playlists[0].is_none());
        assert_eq!(loaded.playlists[1], lib.playlists[1]);
        assert_eq!(loaded.collections[0].playlists, vec![PlaylistId(1)]);
        assert_eq!(loaded, lib);
    }

    /// An absent user row loads as None.
    #[test]
    fn unknown_user_is_absent() {
        let store = Store::in_memory().unwrap();
        assert_eq!(loaded_lib(&store, "unknown"), None);
    }

    /// Repeated writes replace rows without duplicates.
    #[test]
    fn write_is_idempotent() {
        let mut store = Store::in_memory().unwrap();
        let lib = populated("Ada", 0);
        store.write("a", &lib).unwrap();
        store.write("a", &lib).unwrap();
        assert_eq!(loaded_lib(&store, "a"), Some(lib.clone()));
        let count: u32 = store
            .conn
            .query_row(
                "SELECT count(*) FROM \"like\" WHERE user_id = 'a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, lib.liked.count_ones());
    }

    /// Two users can store different libraries with the same playlist and collection indices.
    #[test]
    fn users_are_isolated() {
        let mut store = Store::in_memory().unwrap();
        let a = populated("Ada", 0);
        let b = populated("Bea", 4);
        store.write("a", &a).unwrap();
        store.write("b", &b).unwrap();
        store.write("a", &a).unwrap();
        assert_eq!(loaded_lib(&store, "a"), Some(a));
        assert_eq!(loaded_lib(&store, "b"), Some(b));
    }

    /// A like that names a track the catalogue lacks is dropped, and the rest of
    /// the library loads. A drop is recoverable by the next sync, so it must not
    /// cost the user the whole library.
    #[test]
    fn an_unknown_track_is_dropped_not_fatal() {
        let mut store = Store::in_memory().unwrap();
        let mut lib = Library::default();
        lib.toggle_like(TrackId(0));
        lib.toggle_like(TrackId(1));
        store.write("a", &lib).unwrap();
        store
            .conn
            .execute("INSERT INTO \"like\" VALUES ('a', 999)", [])
            .unwrap();

        let loaded = store.load("a").unwrap().unwrap();
        assert_eq!(loaded.dropped, 1);
        assert_eq!(loaded.lib, lib, "the good likes did not survive");
        assert_eq!(loaded.lib.check(), Ok(()));
    }

    /// A playlist keeps its name and its remaining tracks when one track is
    /// unknown. The container survives, only the leaf goes.
    #[test]
    fn a_container_survives_a_dropped_leaf() {
        let mut store = Store::in_memory().unwrap();
        let mut lib = Library::default();
        let id = lib.create_playlist("Road trip".to_string()).unwrap();
        lib.add_track(id, TrackId(0));
        lib.add_track(id, TrackId(1));
        store.write("a", &lib).unwrap();
        store
            .conn
            .execute("UPDATE playlist_track SET track_id = 999 WHERE position = 1", [])
            .unwrap();

        let loaded = store.load("a").unwrap().unwrap();
        assert_eq!(loaded.dropped, 1);
        let playlist = loaded.lib.playlist(id).expect("the playlist disappeared");
        assert_eq!(playlist.name, "Road trip");
        assert_eq!(playlist.tracks, vec![TrackId(0)]);
    }

    /// A gap in the collection indices does not attach a reference to the wrong
    /// folder. Save never writes a gap, so only a damaged file reaches this.
    #[test]
    fn a_collection_gap_does_not_misplace_a_reference() {
        let mut store = Store::in_memory().unwrap();
        let mut lib = Library::default();
        let id = lib.create_playlist("P".to_string()).unwrap();
        lib.create_collection("Second".into());
        lib.add_to_collection(0, id);
        store.write("a", &lib).unwrap();
        // Move the only collection to idx 3, leaving a gap below it, and take
        // its reference with it.
        store
            .conn
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 UPDATE collection SET idx = 3;
                 UPDATE collection_playlist SET idx = 3;
                 PRAGMA foreign_keys = ON;",
            )
            .unwrap();

        let loaded = store.load("a").unwrap().unwrap();
        assert_eq!(loaded.lib.collections.len(), 1);
        assert_eq!(loaded.lib.collections[0].name, "Second");
        assert_eq!(
            loaded.lib.collections[0].playlists,
            vec![id],
            "the reference landed on the wrong collection"
        );
        assert_eq!(loaded.lib.check(), Ok(()));
    }

    /// A fresh database reports the schema version this build writes.
    #[test]
    fn fresh_schema_has_current_version() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    /// A collection reference to an empty playlist slot is dropped, so the
    /// invariant holds without the collection disappearing.
    #[test]
    fn a_reference_to_an_empty_slot_is_dropped() {
        let mut store = Store::in_memory().unwrap();
        let mut lib = Library::default();
        lib.create_collection("C".into());
        store.write("a", &lib).unwrap();
        store
            .conn
            .execute("INSERT INTO collection_playlist VALUES ('a', 0, 0, 1)", [])
            .unwrap();

        let loaded = store.load("a").unwrap().unwrap();
        assert_eq!(loaded.dropped, 1);
        assert_eq!(loaded.lib.collections[0].name, "C");
        assert!(loaded.lib.collections[0].playlists.is_empty());
        assert_eq!(loaded.lib.check(), Ok(()));
    }

    /// A duplicated history entry is dropped, because a duplicate would fail
    /// the invariant the whole app relies on.
    #[test]
    fn a_duplicate_history_entry_is_dropped() {
        let mut store = Store::in_memory().unwrap();
        store.write("a", &Library::default()).unwrap();
        store
            .conn
            .execute_batch("INSERT INTO history VALUES ('a', 0, 0), ('a', 1, 0);")
            .unwrap();

        let loaded = store.load("a").unwrap().unwrap();
        assert_eq!(loaded.dropped, 1);
        assert_eq!(loaded.lib.history, vec![TrackId(0)]);
        assert_eq!(loaded.lib.check(), Ok(()));
    }

    /// A failed write rolls back the user row and all child rows.
    #[test]
    fn a_failed_write_is_atomic() {
        let mut store = Store::in_memory().unwrap();
        let lib = populated("Ada", 0);
        store.write("a", &lib).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER fail_history BEFORE INSERT ON history
             BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();
        assert!(store.write("a", &populated("Changed", 2)).is_err());
        assert_eq!(loaded_lib(&store, "a"), Some(lib));
    }

    /// Every catalogue table drops a row that names an id the catalogue does not
    /// ship, and reports it.
    #[test]
    fn every_catalogue_table_drops_an_unknown_id() {
        for (table, column) in [
            ("like", "track_id"),
            ("save", "album_id"),
            ("follow", "artist_id"),
            ("download", "album_id"),
            ("playlist_track", "track_id"),
            ("history", "track_id"),
        ] {
            let mut store = Store::in_memory().unwrap();
            store.write("a", &populated("Ada", 0)).unwrap();
            store
                .conn
                .execute(
                    &format!("UPDATE \"{table}\" SET {column} = -1 WHERE user_id = 'a'"),
                    [],
                )
                .unwrap();
            let loaded = store.load("a").unwrap().unwrap();
            assert!(loaded.dropped > 0, "{table} accepted an unknown id");
            assert_eq!(loaded.lib.check(), Ok(()), "{table} broke the invariant");
        }
    }

    /// Replacing one user's populated library deletes stale rows without changing another user's library.
    #[test]
    fn replacement_removes_stale_rows() {
        let mut store = Store::in_memory().unwrap();
        let b = populated("Bea", 3);
        store.write("a", &populated("Ada", 0)).unwrap();
        store.write("b", &b).unwrap();
        let empty = Library::default();
        store.write("a", &empty).unwrap();
        assert_eq!(loaded_lib(&store, "a"), Some(empty));
        assert_eq!(loaded_lib(&store, "b"), Some(b));
        let integrity: String = store
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        assert!(
            !store
                .conn
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap()
        );
    }

    /// History beyond its cap is trimmed to the cap, newest first, and counted.
    #[test]
    fn excess_history_is_trimmed() {
        let mut store = Store::in_memory().unwrap();
        store.write("a", &Library::default()).unwrap();
        for position in 0..HISTORY_CAP + 3 {
            store
                .conn
                .execute(
                    "INSERT INTO history VALUES ('a', ?1, ?1)",
                    [position as i64],
                )
                .unwrap();
        }
        let loaded = store.load("a").unwrap().unwrap();
        assert_eq!(loaded.lib.history.len(), HISTORY_CAP);
        assert_eq!(loaded.dropped, 3);
        assert_eq!(loaded.lib.check(), Ok(()));
    }

    /// Configuration preserves a current schema and rejects a schema from a newer build.
    #[test]
    fn schema_version_controls_migration() {
        let store = Store::in_memory().unwrap();
        let store = Store::configure(store.conn).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let foreign_keys: bool = store
            .conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        let synchronous: u32 = store
            .conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert!(foreign_keys);
        assert_eq!(synchronous, 2);
        store
            .conn
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(
            Store::configure(store.conn),
            Err(StoreError::Sql(_))
        ));
    }

    /// A restart reads back what the previous run wrote: a second connection to
    /// the same file sees the library, and the file is private.
    #[test]
    fn a_restart_reads_the_written_library() {
        let dir = std::env::temp_dir().join(format!("opentidal-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("opentidal.db");
        let lib = populated("Ada", 7);

        {
            let mut first = Store::open(&path).unwrap();
            first.write("173210380", &lib).unwrap();
        }

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the database is not private");

        let second = Store::open(&path).unwrap();
        assert_eq!(loaded_lib(&second, "173210380"), Some(lib));
        assert_eq!(second.schema_version().unwrap(), SCHEMA_VERSION);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
