//! The SQLite store for each user's library. Rune-free: rows map directly to
//! the library model, and load checks the invariant before it returns data.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::catalog::{self, TrackId};
use crate::library::{Collection, Library, MAX_COLLECTIONS, MAX_PLAYLISTS, Playlist, PlaylistId};

const SCHEMA_VERSION: u32 = 1;

/// A database that stores libraries by user id.
pub struct Store {
    conn: Connection,
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
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::configure(conn)
    }

    /// An in-memory database, for tests.
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
    pub fn load(&self, user_id: &str) -> Result<Option<Library>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let name = tx
            .query_row("SELECT name FROM user WHERE id = ?1", [user_id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        let Some(name) = name else {
            return Ok(None);
        };
        let mut lib = Library {
            name,
            liked: load_bits(&tx, user_id, "like", "track_id", catalog::TRACKS.len())?,
            saved_albums: load_bits(&tx, user_id, "save", "album_id", catalog::ALBUMS.len())?,
            followed: load_bits(&tx, user_id, "follow", "artist_id", catalog::ARTISTS.len())?,
            downloaded: load_bits(&tx, user_id, "download", "album_id", catalog::ALBUMS.len())?,
            history: Vec::new(),
            playlists: std::array::from_fn(|_| None),
            collections: Vec::new(),
        };
        {
            let mut stmt =
                tx.prepare("SELECT slot, name FROM playlist WHERE user_id = ?1 ORDER BY slot")?;
            let mut rows = stmt.query([user_id])?;
            while let Some(row) = rows.next()? {
                let slot = index("playlist", row.get(0)?, MAX_PLAYLISTS)?;
                lib.playlists[slot] = Some(Playlist {
                    name: row.get(1)?,
                    tracks: Vec::new(),
                });
            }
        }
        {
            let mut stmt = tx.prepare(
                "SELECT slot, position, track_id FROM playlist_track
                 WHERE user_id = ?1 ORDER BY slot, position",
            )?;
            let mut rows = stmt.query([user_id])?;
            while let Some(row) = rows.next()? {
                let raw_slot = row.get(0)?;
                let slot = index("playlist_track", raw_slot, MAX_PLAYLISTS)?;
                let playlist = lib.playlists[slot].as_mut().ok_or_else(|| {
                    StoreError::Sql(format!("playlist_track missing slot {raw_slot}"))
                })?;
                position("playlist_track", row.get(1)?, playlist.tracks.len())?;
                playlist.tracks.push(TrackId(index(
                    "playlist_track",
                    row.get(2)?,
                    catalog::TRACKS.len(),
                )? as u8));
            }
        }
        {
            let mut stmt =
                tx.prepare("SELECT idx, name FROM collection WHERE user_id = ?1 ORDER BY idx")?;
            let mut rows = stmt.query([user_id])?;
            while let Some(row) = rows.next()? {
                let idx = index("collection", row.get(0)?, MAX_COLLECTIONS)?;
                position("collection", idx as i64, lib.collections.len())?;
                lib.collections.push(Collection {
                    name: row.get(1)?,
                    playlists: Vec::new(),
                });
            }
        }
        {
            let mut stmt = tx.prepare(
                "SELECT idx, position, playlist_slot FROM collection_playlist
                 WHERE user_id = ?1 ORDER BY idx, position",
            )?;
            let mut rows = stmt.query([user_id])?;
            while let Some(row) = rows.next()? {
                let raw_idx = row.get(0)?;
                let idx = index("collection_playlist", raw_idx, lib.collections.len())?;
                let raw_slot = row.get(2)?;
                let slot = index("collection_playlist", raw_slot, MAX_PLAYLISTS)?;
                if lib.playlists[slot].is_none() {
                    return Err(StoreError::Sql(format!(
                        "collection_playlist missing slot {raw_slot}"
                    )));
                }
                let collection = &mut lib.collections[idx];
                position(
                    "collection_playlist",
                    row.get(1)?,
                    collection.playlists.len(),
                )?;
                collection.playlists.push(PlaylistId(slot as u8));
            }
        }
        {
            let mut stmt = tx.prepare(
                "SELECT position, track_id FROM history WHERE user_id = ?1 ORDER BY position",
            )?;
            let mut rows = stmt.query([user_id])?;
            while let Some(row) = rows.next()? {
                position("history", row.get(0)?, lib.history.len())?;
                lib.history.push(TrackId(
                    index("history", row.get(1)?, catalog::TRACKS.len())? as u8,
                ));
            }
        }
        lib.check()
            .map_err(|error| StoreError::Sql(format!("library invariant: {error:?}")))?;
        tx.commit()?;
        Ok(Some(lib))
    }

    /// Replace the stored library for a user in one transaction.
    pub fn save(&mut self, user_id: &str, lib: &Library) -> Result<(), StoreError> {
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
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }
}

fn index(table: &str, value: i64, len: usize) -> Result<usize, StoreError> {
    if value < 0 || value as u64 >= len as u64 {
        return Err(StoreError::Sql(format!(
            "{table} out-of-range value {value}"
        )));
    }
    Ok(value as usize)
}

fn position(table: &str, value: i64, expected: usize) -> Result<(), StoreError> {
    if value != expected as i64 {
        return Err(StoreError::Sql(format!(
            "{table} invalid position {value}, expected {expected}"
        )));
    }
    Ok(())
}

fn load_bits(
    conn: &Connection,
    user_id: &str,
    table: &str,
    column: &str,
    len: usize,
) -> Result<u32, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {column} FROM \"{table}\" WHERE user_id = ?1"
    ))?;
    let mut rows = stmt.query([user_id])?;
    let mut bits = 0;
    while let Some(row) = rows.next()? {
        bits |= 1 << index(table, row.get(0)?, len)?;
    }
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{AlbumId, ArtistId};

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
        store.save("a", &lib).unwrap();
        assert_eq!(store.load("a").unwrap(), Some(lib));
    }

    /// Real library operations produce a library that round-trips and passes the invariant.
    #[test]
    fn operations_round_trip() {
        let mut store = Store::in_memory().unwrap();
        let lib = populated("Ada", 3);
        store.save("a", &lib).unwrap();
        let loaded = store.load("a").unwrap().unwrap();
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
                store.save("a", &lib).unwrap();
                let loaded = store.load("a").unwrap().unwrap();
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
        store.save("a", &lib).unwrap();
        let loaded = store.load("a").unwrap().unwrap();
        assert!(loaded.playlists[0].is_none());
        assert_eq!(loaded.playlists[1], lib.playlists[1]);
        assert_eq!(loaded.collections[0].playlists, vec![PlaylistId(1)]);
        assert_eq!(loaded, lib);
    }

    /// An absent user row loads as None.
    #[test]
    fn unknown_user_is_absent() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.load("unknown").unwrap(), None);
    }

    /// Repeated saves replace rows without duplicates.
    #[test]
    fn save_is_idempotent() {
        let mut store = Store::in_memory().unwrap();
        let lib = populated("Ada", 0);
        store.save("a", &lib).unwrap();
        store.save("a", &lib).unwrap();
        assert_eq!(store.load("a").unwrap(), Some(lib.clone()));
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
        store.save("a", &a).unwrap();
        store.save("b", &b).unwrap();
        store.save("a", &a).unwrap();
        assert_eq!(store.load("a").unwrap(), Some(a));
        assert_eq!(store.load("b").unwrap(), Some(b));
    }

    /// Load rejects an out-of-range track id and names its table and value.
    #[test]
    fn invalid_track_is_rejected() {
        let mut store = Store::in_memory().unwrap();
        store.save("a", &Library::default()).unwrap();
        let bad = catalog::TRACKS.len() as i64;
        store
            .conn
            .execute("INSERT INTO \"like\" VALUES ('a', ?1)", [bad])
            .unwrap();
        let error = store.load("a").unwrap_err();
        assert!(matches!(error, StoreError::Sql(message)
            if message.contains("like") && message.contains(&bad.to_string())));
    }

    /// A fresh database reports the schema version this build writes.
    #[test]
    fn fresh_schema_has_current_version() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    /// Load rejects a collection reference to an empty playlist slot.
    #[test]
    fn missing_playlist_is_rejected() {
        let mut store = Store::in_memory().unwrap();
        let mut lib = Library::default();
        lib.create_collection("C".into());
        store.save("a", &lib).unwrap();
        store
            .conn
            .execute("INSERT INTO collection_playlist VALUES ('a', 0, 0, 1)", [])
            .unwrap();
        assert!(matches!(store.load("a"), Err(StoreError::Sql(message))
            if message.contains("collection_playlist") && message.contains('1')));
    }

    /// Load runs the invariant checker and rejects duplicate history tracks.
    #[test]
    fn invariant_violation_is_rejected() {
        let mut store = Store::in_memory().unwrap();
        store.save("a", &Library::default()).unwrap();
        store
            .conn
            .execute_batch("INSERT INTO history VALUES ('a', 0, 0), ('a', 1, 0);")
            .unwrap();
        assert!(matches!(store.load("a"), Err(StoreError::Sql(message))
            if message.contains("HistoryDuplicate")));
    }

    /// A failed replacement rolls back the user row and all child rows.
    #[test]
    fn failed_save_is_atomic() {
        let mut store = Store::in_memory().unwrap();
        let lib = populated("Ada", 0);
        store.save("a", &lib).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER fail_history BEFORE INSERT ON history
             BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();
        assert!(store.save("a", &populated("Changed", 2)).is_err());
        assert_eq!(store.load("a").unwrap(), Some(lib));
    }

    /// Load rejects negative and out-of-range catalogue ids in every catalogue table.
    #[test]
    fn invalid_catalogue_ids_are_rejected() {
        for (table, column, len) in [
            ("like", "track_id", catalog::TRACKS.len()),
            ("save", "album_id", catalog::ALBUMS.len()),
            ("follow", "artist_id", catalog::ARTISTS.len()),
            ("download", "album_id", catalog::ALBUMS.len()),
            ("playlist_track", "track_id", catalog::TRACKS.len()),
            ("history", "track_id", catalog::TRACKS.len()),
        ] {
            for value in [-1, len as i64, 256] {
                let mut store = Store::in_memory().unwrap();
                store.save("a", &populated("Ada", 0)).unwrap();
                store.conn.execute(
                    &format!("UPDATE \"{table}\" SET {column} = ?1 WHERE user_id = 'a' AND {column} = 0"),
                    [value],
                ).unwrap();
                assert!(matches!(store.load("a"), Err(StoreError::Sql(message))
                    if message.contains(table) && message.contains(&value.to_string())));
            }
        }
    }

    /// Replacing one user's populated library deletes stale rows without changing another user's library.
    #[test]
    fn replacement_removes_stale_rows() {
        let mut store = Store::in_memory().unwrap();
        let b = populated("Bea", 3);
        store.save("a", &populated("Ada", 0)).unwrap();
        store.save("b", &b).unwrap();
        let empty = Library::default();
        store.save("a", &empty).unwrap();
        assert_eq!(store.load("a").unwrap(), Some(empty));
        assert_eq!(store.load("b").unwrap(), Some(b));
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

    /// Load rejects history beyond its cap instead of silently discarding rows.
    #[test]
    fn excess_history_is_rejected() {
        let mut store = Store::in_memory().unwrap();
        store.save("a", &Library::default()).unwrap();
        for position in 0..=crate::library::HISTORY_CAP {
            store
                .conn
                .execute(
                    "INSERT INTO history VALUES ('a', ?1, ?1)",
                    [position as i64],
                )
                .unwrap();
        }
        assert!(matches!(store.load("a"), Err(StoreError::Sql(message))
            if message.contains("HistoryTooLong")));
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
}
