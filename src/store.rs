//! The SQLite store for each user's library. Rune-free: rows map directly to
//! the library model, and load checks the invariant before it returns data.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use std::collections::BTreeSet;

use crate::catalogue::{AlbumRow, ArtistRow, Catalogue, CatalogueRows, PlaylistRow, TrackRow};
use crate::library::{
    COLLECTION_PLAYLIST_CAP, Collection, HISTORY_CAP, Library, MAX_COLLECTIONS, MAX_PLAYLISTS,
    Members, PLAYLIST_TRACK_CAP, Playlist, PlaylistId,
};

const SCHEMA_VERSION: u32 = 5;

/// The tables that name no catalogue id. Version 1 created them with this text,
/// so a migrated file and a fresh one hold the same definitions.
const USER_TABLES: &str = "
    CREATE TABLE user(id TEXT PRIMARY KEY, name TEXT NOT NULL,
        updated_at INTEGER NOT NULL);
    CREATE TABLE playlist(slot INTEGER NOT NULL,
        user_id TEXT NOT NULL REFERENCES user, name TEXT NOT NULL,
        PRIMARY KEY(user_id, slot));
    CREATE TABLE collection(idx INTEGER NOT NULL,
        user_id TEXT NOT NULL REFERENCES user, name TEXT NOT NULL,
        PRIMARY KEY(user_id, idx));
    CREATE TABLE collection_playlist(user_id TEXT NOT NULL REFERENCES user,
        idx INTEGER NOT NULL, position INTEGER NOT NULL,
        playlist_slot INTEGER NOT NULL, PRIMARY KEY(user_id, idx, position),
        FOREIGN KEY(user_id, idx) REFERENCES collection(user_id, idx));";

/// The catalogue rows and the six library tables that name them. A fresh file and
/// the migration from version 1 both run this text, so the two cannot drift. The
/// foreign keys mean a library row cannot reach the disk before the catalogue row
/// it names. A duration is ISO 8601 text, the shape Tidal sends.
const CATALOGUE_ID_TABLES: &str = "
    CREATE TABLE artist(id TEXT PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE album(id TEXT PRIMARY KEY, title TEXT NOT NULL,
        artist_id TEXT NOT NULL REFERENCES artist);
    CREATE TABLE track(id TEXT PRIMARY KEY, title TEXT NOT NULL,
        artist_id TEXT NOT NULL REFERENCES artist,
        album_id TEXT NOT NULL REFERENCES album, duration TEXT NOT NULL);
    CREATE TABLE \"like\"(user_id TEXT NOT NULL REFERENCES user,
        track_id TEXT NOT NULL REFERENCES track, PRIMARY KEY(user_id, track_id));
    CREATE TABLE save(user_id TEXT NOT NULL REFERENCES user,
        album_id TEXT NOT NULL REFERENCES album, PRIMARY KEY(user_id, album_id));
    CREATE TABLE follow(user_id TEXT NOT NULL REFERENCES user,
        artist_id TEXT NOT NULL REFERENCES artist, PRIMARY KEY(user_id, artist_id));
    CREATE TABLE download(user_id TEXT NOT NULL REFERENCES user,
        album_id TEXT NOT NULL REFERENCES album, PRIMARY KEY(user_id, album_id));
    CREATE TABLE playlist_track(user_id TEXT NOT NULL REFERENCES user,
        slot INTEGER NOT NULL, position INTEGER NOT NULL,
        track_id TEXT NOT NULL REFERENCES track, PRIMARY KEY(user_id, slot, position),
        FOREIGN KEY(user_id, slot) REFERENCES playlist(user_id, slot));
    CREATE TABLE history(user_id TEXT NOT NULL REFERENCES user,
        position INTEGER NOT NULL, track_id TEXT NOT NULL REFERENCES track,
        PRIMARY KEY(user_id, position));";

/// The catalogue playlists and the per-account membership that names them, new in
/// version 3. A fresh file and the migration from versions 1 and 2 all run this
/// text. The `playlist` table already holds the local slots, so the catalogue
/// tables take another name.
const CATALOGUE_PLAYLIST_TABLES: &str = "
    CREATE TABLE catalogue_playlist(id TEXT PRIMARY KEY, name TEXT NOT NULL,
        description TEXT NOT NULL);
    CREATE TABLE catalogue_playlist_track(
        playlist_id TEXT NOT NULL REFERENCES catalogue_playlist,
        position INTEGER NOT NULL, track_id TEXT NOT NULL REFERENCES track,
        PRIMARY KEY(playlist_id, position));
    CREATE TABLE tidal_playlist(user_id TEXT NOT NULL REFERENCES user,
        playlist_id TEXT NOT NULL REFERENCES catalogue_playlist,
        PRIMARY KEY(user_id, playlist_id));";

/// The cover columns, new in version 4. Every path to version 4 runs this text
/// after the version 3 tables, a fresh file included, so a fresh and a migrated
/// file hold the same definitions. An empty cover means none.
const COVER_COLUMNS: &str = "
    ALTER TABLE album ADD COLUMN cover TEXT NOT NULL DEFAULT '';
    ALTER TABLE catalogue_playlist ADD COLUMN cover TEXT NOT NULL DEFAULT '';";

/// The member positions, new in version 5, in Tidal's order. Every path to
/// version 5 runs this text after the cover columns, a fresh file included. A
/// migrated row keeps position 0, so it loads in id order until the next write.
const MEMBER_POSITIONS: &str = "
    ALTER TABLE \"like\" ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE save ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE follow ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE tidal_playlist ADD COLUMN position INTEGER NOT NULL DEFAULT 0;";

/// The version 1 tables that held an integer catalogue id, in the order the
/// migration drops them.
const V1_ID_TABLES: [&str; 6] = ["like", "save", "follow", "download", "playlist_track", "history"];

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
        let conn = Connection::open(path)?;
        // Exclusive first, then WAL. Set the other way round, the `journal_mode` pragma
        // is the statement that blocks, and it reports "database is locked" from inside
        // the open rather than the clear message below.
        //
        // Not in tests: `locking_mode` is a property of the file, so a second open in
        // the same process waits even without the hold below. A test reopens the same
        // file to stand in for a restart, and refusing that would make every restart
        // test a test of the lock.
        // The debug binary and the release build are real apps, so the lock applies
        // there. `cfg!(test)` is true only inside `cargo test`, which is the one build
        // that reopens a file to stand in for a restart.
        let testing = cfg!(test);
        if !testing {
            conn.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
        } else {
            // A test that reopens the same file must not hang on the default timeout
            // while it waits for a lock that never comes.
            conn.busy_timeout(std::time::Duration::from_millis(200))?;
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // One writer per database for the life of the process.
        //
        // `write` replaces every row for an account, so two instances would take turns
        // overwriting each other: the second one's playlist vanishes when the first
        // writes its own view, and both writes report success. A revision check would
        // notice that after the fact; this refuses the second instance outright, which
        // is the only answer that cannot lose an edit.
        //
        // `locking_mode=EXCLUSIVE` stops SQLite taking shared locks, so the write lock
        // is held until the connection closes. It must be set before the hold, because
        // changing it later does not release a lock already taken.
        // Take it now, so a second instance fails at open rather than at its first
        // write, when the user has already made edits it would lose.
        if !testing {
            conn.execute_batch("BEGIN EXCLUSIVE; COMMIT;").map_err(|e| {
                StoreError::Io(format!(
                    "another OpenTidal has this library open, so this one must not write it: {e}"
                ))
            })?;
        }
        // After the pragma, because WAL mode is what creates the `-wal` and `-shm`
        // sidecars, and `xattr` cannot mark a file that does not exist yet.
        exclude_from_backup(path)?;
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
                tx.execute_batch(USER_TABLES)?;
                tx.execute_batch(CATALOGUE_ID_TABLES)?;
                tx.execute_batch(CATALOGUE_PLAYLIST_TABLES)?;
                tx.execute_batch(COVER_COLUMNS)?;
                tx.execute_batch(MEMBER_POSITIONS)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                tx.commit()?;
            }
            // Version 1 stored placeholder integer ids that name no Tidal item, so
            // its id rows are dropped, not mapped. Names, slots and collections hold
            // no catalogue id and survive. One transaction, so a failure at any
            // statement leaves an intact version 1 that the next open migrates again.
            1 => {
                let tx = conn.transaction()?;
                for table in V1_ID_TABLES {
                    tx.execute_batch(&format!("DROP TABLE \"{table}\";"))?;
                }
                tx.execute_batch(CATALOGUE_ID_TABLES)?;
                tx.execute_batch(CATALOGUE_PLAYLIST_TABLES)?;
                tx.execute_batch(COVER_COLUMNS)?;
                tx.execute_batch(MEMBER_POSITIONS)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                tx.commit()?;
            }
            // Version 2 holds every row version 3 needs. Only the catalogue
            // playlist tables are new, so the migration adds them and keeps the
            // rest, in one transaction.
            2 => {
                let tx = conn.transaction()?;
                tx.execute_batch(CATALOGUE_PLAYLIST_TABLES)?;
                tx.execute_batch(COVER_COLUMNS)?;
                tx.execute_batch(MEMBER_POSITIONS)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                tx.commit()?;
            }
            // Version 3 lacks only the cover columns. Every row keeps its cover
            // empty until the next sync names one.
            3 => {
                let tx = conn.transaction()?;
                tx.execute_batch(COVER_COLUMNS)?;
                tx.execute_batch(MEMBER_POSITIONS)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                tx.commit()?;
            }
            // Version 4 lacks only the member positions.
            4 => {
                let tx = conn.transaction()?;
                tx.execute_batch(MEMBER_POSITIONS)?;
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
    /// The stored catalogue rows are absorbed into `cat` first, so every id a
    /// write stored resolves again. A leaf that names an id the catalogue lacks is
    /// dropped and counted, before any cap counts it. The container around it stays, so a playlist missing one track
    /// still appears with the tracks it has. A leaf that comes back on the next
    /// sync is recoverable, so dropping it costs nothing permanent. Only a file
    /// that cannot be read at all is an error.
    pub fn load(&self, user_id: &str, cat: &mut Catalogue) -> Result<Option<Loaded>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        cat.absorb(read_catalogue(&tx)?);
        let cat = &*cat;
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
            liked: load_members(&tx, user_id, "like", "track_id", |raw| cat.track_id(raw), &mut dropped)?,
            saved_albums: load_members(&tx, user_id, "save", "album_id", |raw| cat.album_id(raw), &mut dropped)?,
            followed: load_members(&tx, user_id, "follow", "artist_id", |raw| cat.artist_id(raw), &mut dropped)?,
            tidal_playlists: load_members(
                &tx,
                user_id,
                "tidal_playlist",
                "playlist_id",
                |raw| cat.playlist_id(raw),
                &mut dropped,
            )?,
            downloaded: load_set(&tx, user_id, "download", "album_id", |raw| cat.album_id(raw), &mut dropped)?,
            history: Vec::new(),
            playlists: std::array::from_fn(|_| None),
            collections: Vec::new(),
        };
        {
            // The valid range is filtered before the limit, not after. An invalid slot
            // arriving first would otherwise take one of the limited rows, and a valid
            // playlist past the limit would be dropped here and deleted by the next
            // write. The `index` check below stays, for the value the database holds
            // that this filter cannot express.
            let mut stmt = tx.prepare(
                "SELECT slot, name FROM playlist
                 WHERE user_id = ?1 AND slot >= 0 AND slot < ?2
                 ORDER BY slot LIMIT ?2",
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
                let Some(track) = text(row, 1)?.and_then(|raw| cat.track_id(&raw)) else {
                    dropped += 1;
                    continue;
                };
                if playlist.tracks.contains(&track) {
                    dropped += 1;
                    continue;
                }
                playlist.tracks.push(track);
            }
        }
        {
            let mut stmt = tx.prepare(
                // A negative index is filtered before the limit, not after: it sorts
                // first, would take one of the limited rows, and a valid collection past
                // the limit would be dropped here and deleted by the next write.
                //
                // Only the sign is filtered. The index is not bounded by the count: it is
                // what a child row names, so a gap must not shift it.
                "SELECT idx, name FROM collection
                 WHERE user_id = ?1 AND idx >= 0
                 ORDER BY idx LIMIT ?2",
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
            // No `LIMIT`, because the cap counts entries the loader accepted, not rows
            // it read. A row that names nothing, or repeats one, is dropped here, so a
            // limit on rows would hide a valid entry past it and the next write would
            // delete that entry. Reading stops as soon as the cap is reached, so the
            // work is bounded by the number of stored rows either way.
            let mut stmt = tx.prepare(
                "SELECT track_id FROM history WHERE user_id = ?1 ORDER BY position",
            )?;
            let mut rows = stmt.query([user_id])?;
            let mut past_the_cap = 0usize;
            while let Some(row) = rows.next()? {
                if lib.history.len() >= HISTORY_CAP {
                    // Every row past the cap is one the loader will not keep, whether or
                    // not it is valid. Counted here, so the number matches the rows.
                    past_the_cap += 1;
                    continue;
                }
                let Some(track) = text(row, 0)?.and_then(|raw| cat.track_id(&raw)) else {
                    dropped += 1;
                    continue;
                };
                if lib.history.contains(&track) {
                    dropped += 1;
                    continue;
                }
                lib.history.push(track);
            }
            dropped += past_the_cap;
        }
        tx.commit()?;
        // The boundary has already dropped every row that could break this. A
        // failure here means the loader has a hole, and a test catches it before
        // a user can.
        debug_assert!(lib.check(cat).is_ok(), "load built a library that fails its own invariant");
        Ok(Some(Loaded { lib, dropped }))
    }

    /// Replace the stored library for a user in one transaction, after the
    /// catalogue rows it names. Catalogue rows are shared and never deleted.
    pub fn write(&mut self, user_id: &str, lib: &Library, cat: &Catalogue) -> Result<(), StoreError> {
        lib.check(cat)
            .map_err(|error| StoreError::Sql(format!("library invariant: {error:?}")))?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO user(id, name, updated_at) VALUES (?1, ?2, unixepoch())
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, updated_at = excluded.updated_at",
            params![user_id, lib.name],
        )?;
        upsert_catalogue(&tx, &cat.rows_for(lib))?;
        for table in [
            "playlist_track",
            "collection_playlist",
            "playlist",
            "collection",
            "like",
            "save",
            "follow",
            "tidal_playlist",
            "download",
            "history",
        ] {
            tx.execute(
                &format!("DELETE FROM \"{table}\" WHERE user_id = ?1"),
                [user_id],
            )?;
        }
        for (table, column, ids) in [
            ("like", "track_id", lib.liked.iter().map(|id| id.as_str()).collect::<Vec<_>>()),
            ("save", "album_id", lib.saved_albums.iter().map(|id| id.as_str()).collect()),
            ("follow", "artist_id", lib.followed.iter().map(|id| id.as_str()).collect()),
            ("tidal_playlist", "playlist_id", lib.tidal_playlists.iter().map(|id| id.as_str()).collect()),
        ] {
            let mut stmt = tx.prepare(&format!(
                "INSERT INTO \"{table}\"(user_id, {column}, position) VALUES (?1, ?2, ?3)"
            ))?;
            for (position, id) in ids.into_iter().enumerate() {
                stmt.execute(params![user_id, id, position as i64])?;
            }
        }
        {
            let mut stmt = tx.prepare("INSERT INTO download(user_id, album_id) VALUES (?1, ?2)")?;
            for id in &lib.downloaded {
                stmt.execute(params![user_id, id.as_str()])?;
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
                    params![user_id, slot as i64, position as i64, track.as_str()])?;
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
                params![user_id, position as i64, track.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Make every write fail from now on, so a test can stand where a full disk
    /// leaves the app: the handle is open and the write is refused.
    /// Drop the trigger `abort_writes_for_test` added, so a test can let the
    /// writes through again and watch the retry land.
    #[cfg(test)]
    pub fn allow_writes_again_for_test(&self) {
        self.conn.execute_batch("DROP TRIGGER IF EXISTS abort_user;").unwrap();
    }

    /// Take the writer lock that `open` takes outside tests, so a test can prove a
    /// second instance is refused. The lock is the real one; only its timing differs,
    /// because `open` skips it in a test build for the restart tests.
    #[cfg(test)]
    pub fn take_writer_lock(&self) -> Result<(), StoreError> {
        self.conn.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
        self.conn
            .execute_batch("BEGIN EXCLUSIVE; COMMIT;")
            .map_err(|e| StoreError::Io(format!("another writer holds the library: {e}")))
    }

    #[cfg(test)]
    pub fn abort_writes_for_test(&self) {
        self.conn
            .execute_batch(
                "CREATE TRIGGER abort_user BEFORE INSERT ON user
                 BEGIN SELECT RAISE(ABORT, 'test: writes are failing'); END;",
            )
            .unwrap();
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
/// In WAL mode recent rows live in `opentidal.db-wal`, which SQLite creates during
/// a write and deletes on the last close. A file that does not exist cannot carry
/// a flag, so the flag goes on the directory the app owns, and Apple skips an
/// excluded folder with everything in it.
///
/// Only the app's own directory. `OPENTIDAL_DB` can point at a file in a shared
/// directory, and excluding that folder would drop unrelated files from the
/// backup, so there the database file alone is excluded.
///
/// This sets the extended attribute `tmutil addexclusion` sets, rather than
/// running `tmutil`, which takes eleven seconds here and this runs on the
/// startup path.
#[cfg(target_os = "macos")]
fn exclude_from_backup(path: &Path) -> Result<(), StoreError> {
    let target = match path.parent() {
        Some(dir) if is_app_directory(dir) => dir.to_owned(),
        _ => path.to_owned(),
    };
    let _ = std::process::Command::new("xattr")
        .args([
            "-w",
            "com.apple.metadata:com_apple_backup_excludeItem",
            "com.apple.backupd",
            &target.to_string_lossy(),
        ])
        .status();
    // A sandbox that hides `xattr` must not stop the app opening its database.
    Ok(())
}

/// Nothing to exclude on a platform without Time Machine. The call site in
/// `Store::open` is unconditional, so this must exist for every target or the
/// crate fails to build off macOS.
#[cfg(not(target_os = "macos"))]
fn exclude_from_backup(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

/// Whether this is the directory the app keeps its own files in, so excluding the
/// whole folder touches nothing another program wrote.
#[cfg(target_os = "macos")]
pub(crate) fn is_app_directory(dir: &Path) -> bool {
    // The directory has to end the way this app's own directory ends, not merely be
    // named `OpenTidal`. Without the parent, excluding a directory by name would leave
    // every file in another program's `Application Support/OpenTidal` out of the
    // backup too. The suffix comes from the real path function, so moving the app's
    // files fails the test that checks this, rather than silently stopping the
    // exclusion.
    let Some(suffix) = crate::app::app_directory_suffix() else {
        return false;
    };
    dir.ends_with(&suffix)
}

/// A slot index, rejected when it falls outside the fixed array.
fn index(value: i64, len: usize) -> Result<usize, ()> {
    if value < 0 || value as u64 >= len as u64 {
        return Err(());
    }
    Ok(value as usize)
}

/// Store catalogue rows parents first, so each foreign key finds its target. A
/// known id takes the new fields, the same update in place that `absorb` makes.
fn upsert_catalogue(tx: &Connection, rows: &CatalogueRows) -> Result<(), StoreError> {
    for a in &rows.artists {
        tx.execute(
            "INSERT INTO artist(id, name) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name",
            params![a.id, a.name],
        )?;
    }
    for a in &rows.albums {
        tx.execute(
            "INSERT INTO album(id, title, artist_id, cover) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, artist_id = excluded.artist_id,
                 cover = excluded.cover",
            params![a.id, a.title, a.artist, a.cover],
        )?;
    }
    for t in &rows.tracks {
        tx.execute(
            "INSERT INTO track(id, title, artist_id, album_id, duration) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, artist_id = excluded.artist_id,
                 album_id = excluded.album_id, duration = excluded.duration",
            params![t.id, t.title, t.artist, t.album, t.duration],
        )?;
    }
    for p in &rows.playlists {
        tx.execute(
            "INSERT INTO catalogue_playlist(id, name, description, cover) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, description = excluded.description,
                 cover = excluded.cover",
            params![p.id, p.name, p.description, p.cover],
        )?;
        tx.execute("DELETE FROM catalogue_playlist_track WHERE playlist_id = ?1", [&p.id])?;
        for (position, track) in p.tracks.iter().enumerate() {
            tx.execute(
                "INSERT INTO catalogue_playlist_track(playlist_id, position, track_id) VALUES (?1, ?2, ?3)",
                params![p.id, position as i64, track],
            )?;
        }
    }
    Ok(())
}

/// Every stored catalogue row, as unvalidated rows for `Catalogue::absorb`, which
/// is the one place a row becomes an entry. A row with a non-text column is
/// skipped here, because it cannot be a row of any kind. The cover is the
/// exception: a non-text cover reads as none, so it never costs its row.
fn read_catalogue(conn: &Connection) -> Result<CatalogueRows, StoreError> {
    let mut rows = CatalogueRows::default();
    let mut stmt = conn.prepare("SELECT id, name FROM artist ORDER BY rowid")?;
    let mut q = stmt.query([])?;
    while let Some(row) = q.next()? {
        if let (Some(id), Some(name)) = (text(row, 0)?, text(row, 1)?) {
            rows.artists.push(ArtistRow { id, name });
        }
    }
    let mut stmt = conn.prepare("SELECT id, title, artist_id, cover FROM album ORDER BY rowid")?;
    let mut q = stmt.query([])?;
    while let Some(row) = q.next()? {
        if let (Some(id), Some(title), Some(artist)) = (text(row, 0)?, text(row, 1)?, text(row, 2)?) {
            let cover = text(row, 3)?.unwrap_or_default();
            rows.albums.push(AlbumRow { id, title, artist, cover });
        }
    }
    let mut stmt =
        conn.prepare("SELECT id, title, artist_id, album_id, duration FROM track ORDER BY rowid")?;
    let mut q = stmt.query([])?;
    while let Some(row) = q.next()? {
        if let (Some(id), Some(title), Some(artist), Some(album), Some(duration)) =
            (text(row, 0)?, text(row, 1)?, text(row, 2)?, text(row, 3)?, text(row, 4)?)
        {
            rows.tracks.push(TrackRow { id, title, artist, album, duration });
        }
    }
    let mut stmt =
        conn.prepare("SELECT id, name, description, cover FROM catalogue_playlist ORDER BY rowid")?;
    let mut q = stmt.query([])?;
    while let Some(row) = q.next()? {
        if let (Some(id), Some(name), Some(description)) = (text(row, 0)?, text(row, 1)?, text(row, 2)?) {
            let cover = text(row, 3)?.unwrap_or_default();
            rows.playlists.push(PlaylistRow { id, name, description, tracks: Vec::new(), cover });
        }
    }
    // A track row that is not text cannot name a track, so it is skipped, and
    // absorb drops a track id it does not hold. The playlist keeps the rest.
    let mut stmt = conn.prepare(
        "SELECT playlist_id, track_id FROM catalogue_playlist_track ORDER BY playlist_id, position",
    )?;
    let mut q = stmt.query([])?;
    while let Some(row) = q.next()? {
        if let (Some(playlist), Some(track)) = (text(row, 0)?, text(row, 1)?)
            && let Some(p) = rows.playlists.iter_mut().find(|p| p.id == playlist)
        {
            p.tracks.push(track);
        }
    }
    Ok(rows)
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

/// One account's set of catalogue ids from one table. A row the catalogue
/// cannot resolve is dropped and counted.
fn load_set<T: Ord>(
    conn: &Connection,
    user_id: &str,
    table: &str,
    column: &str,
    resolve: impl Fn(&str) -> Option<T>,
    dropped: &mut usize,
) -> Result<BTreeSet<T>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {column} FROM \"{table}\" WHERE user_id = ?1"
    ))?;
    let mut rows = stmt.query([user_id])?;
    let mut set = BTreeSet::new();
    while let Some(row) = rows.next()? {
        match text(row, 0)?.and_then(|raw| resolve(&raw)) {
            Some(id) => {
                set.insert(id);
            }
            None => *dropped += 1,
        }
    }
    Ok(set)
}

/// One account's catalogue ids from one table, by position, then by id. The id
/// breaks a tie because `VACUUM` can renumber a rowid. A row the catalogue
/// cannot resolve is dropped and counted.
fn load_members<T: Ord + Clone>(
    conn: &Connection,
    user_id: &str,
    table: &str,
    column: &str,
    resolve: impl Fn(&str) -> Option<T>,
    dropped: &mut usize,
) -> Result<Members<T>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {column} FROM \"{table}\" WHERE user_id = ?1 ORDER BY position, {column}"
    ))?;
    let mut rows = stmt.query([user_id])?;
    let mut members = Members::default();
    while let Some(row) = rows.next()? {
        match text(row, 0)?.and_then(|raw| resolve(&raw)) {
            Some(id) => {
                members.insert(id);
            }
            None => *dropped += 1,
        }
    }
    Ok(members)
}

/// A catalogue id column as text, or `None` for a value of another type. A TEXT
/// column turns an inserted number into text, but a blob stays a blob, and a blob
/// is a damaged row to drop, not a file that cannot be read.
fn text(row: &rusqlite::Row, column: usize) -> Result<Option<String>, StoreError> {
    Ok(match row.get_ref(column)? {
        rusqlite::types::ValueRef::Text(bytes) => std::str::from_utf8(bytes).ok().map(str::to_owned),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{AlbumId, ArtistId, CataloguePlaylistId, TrackId};
    use crate::library::Reach;

    /// The seed catalogue. Ids compare by text, so ids minted by two seeds are
    /// equal, and every test can take a fresh one.
    fn cat() -> Catalogue {
        Catalogue::seed()
    }

    fn seed_track(n: usize) -> TrackId {
        let cat = cat();
        cat.tracks().nth(n % cat.tracks().count()).unwrap().id.clone()
    }

    fn seed_album(n: usize) -> AlbumId {
        let cat = cat();
        cat.albums().nth(n % cat.albums().count()).unwrap().id.clone()
    }

    fn seed_artist(n: usize) -> ArtistId {
        let cat = cat();
        cat.artists().nth(n % cat.artists().count()).unwrap().id.clone()
    }

    fn seed_playlist(n: usize) -> CataloguePlaylistId {
        let cat = cat();
        cat.playlists().nth(n % cat.playlists().count()).unwrap().id.clone()
    }

    /// Opening a database keeps it out of a Time Machine backup.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_database_is_excluded_from_backups() {
        let dir = std::env::temp_dir().join(format!("opentidal-backup-{}", std::process::id()));
        let guard = TempDirGuard(dir.clone());
        std::fs::create_dir_all(&dir).unwrap();
        // A file the app does not own, in the directory it writes to. This is what
        // `OPENTIDAL_DB` pointing at a shared directory looks like, and it must
        // stay in the backup.
        let neighbour = dir.join("notes.txt");
        std::fs::write(&neighbour, "keep me").unwrap();
        let path = dir.join("opentidal.db");
        let store = Store::open(&path).unwrap();

        let excluded = |p: &std::path::Path| {
            let out = std::process::Command::new("xattr")
                .args(["-p", "com.apple.metadata:com_apple_backup_excludeItem", &p.to_string_lossy()])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).contains("com.apple.backupd")
        };

        // A temp directory is not the app's own, so only the file is excluded.
        assert!(excluded(&path), "the database is not excluded from backups");
        assert!(
            !excluded(&neighbour),
            "an unrelated file in the same directory was dropped from backups"
        );
        assert!(
            !excluded(&dir),
            "a shared directory was excluded, which drops every neighbour from the backup"
        );
        drop(store);
        drop(guard);
    }

    /// The app's own directory is excluded, so the `-wal` file and the rest of the
    /// app's rows stay out of a backup even though SQLite creates `-wal` only
    /// during a write, long after this flag is set.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_apps_own_directory_is_excluded_from_backups() {
        let dir = std::env::temp_dir().join(format!("opentidal-owned-{}", std::process::id()));
        let guard = TempDirGuard(dir.clone());
        let owned = dir.join("Library").join("Application Support").join("OpenTidal");
        std::fs::create_dir_all(&owned).unwrap();
        let path = owned.join("opentidal.db");
        let store = Store::open(&path).unwrap();

        let out = std::process::Command::new("xattr")
            .args(["-p", "com.apple.metadata:com_apple_backup_excludeItem", &owned.to_string_lossy()])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("com.apple.backupd"),
            "the app's own directory is not excluded, so the -wal file would be backed up"
        );
        drop(store);
        drop(guard);
    }

    /// The lock itself, with the statements `open` runs outside a test build. `open`
    /// skips them in `cargo test`, because the restart tests reopen one file and a lock
    /// would read as a bug in the restart. So the lock is proved here, on the real
    /// pragma and the real `BEGIN EXCLUSIVE`, rather than only by hand.
    #[test]
    fn a_second_connection_is_refused_the_library() {
        let dir = TempDirGuard(std::env::temp_dir().join(format!(
            "opentidal-lock-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )));
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("lib.db");
        let first = Store::open(&path).unwrap();
        first.take_writer_lock().unwrap();

        // `locking_mode` is a property of the file, so a second connection to the same
        // file cannot take the write lock while the first holds it.
        let second = rusqlite::Connection::open(&path).unwrap();
        second.busy_timeout(std::time::Duration::from_millis(100)).unwrap();
        let took = second.execute_batch("BEGIN EXCLUSIVE; COMMIT;");
        assert!(
            took.is_err(),
            "a second connection took the write lock while the first held it, so two instances would replace each other's rows"
        );
    }

    /// A temporary directory that removes itself, so a failing test leaves no
    /// database behind. The lock test uses it on every platform, because the writer
    /// lock is not a macOS-only behaviour.
    struct TempDirGuard(std::path::PathBuf);

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Run raw SQL with the foreign keys off, so a test can plant the damaged row a
    /// write can never produce.
    fn plant(store: &Store, sql: &str) {
        store
            .conn
            .execute_batch(&format!("PRAGMA foreign_keys = OFF; {sql}; PRAGMA foreign_keys = ON;"))
            .unwrap();
    }

    /// The library from a load that dropped nothing, which is every load the
    /// round-trip tests expect.
    fn loaded_lib(store: &Store, user_id: &str) -> Option<Library> {
        let loaded = store.load(user_id, &mut cat()).unwrap()?;
        assert_eq!(loaded.dropped, 0, "a round trip dropped rows");
        Some(loaded.lib)
    }

    fn populated(name: &str, n: usize) -> Library {
        let mut lib = Library::default();
        lib.set_name(name.into());
        let first = lib.create_playlist(format!("{name} first")).unwrap();
        let second = lib.create_playlist(format!("{name} second")).unwrap();
        lib.add_track(first, seed_track(n));
        lib.add_track(first, seed_track(n + 1));
        lib.add_track(second, seed_track(n));
        lib.create_collection(format!("{name} collection"));
        lib.add_to_collection(0, second);
        lib.add_to_collection(0, first);
        lib.toggle_like(seed_track(n));
        lib.toggle_save_album(seed_album(n));
        lib.toggle_follow(seed_artist(n));
        lib.toggle_download(seed_album(n));
        lib.tidal_playlists.insert(seed_playlist(n));
        lib.record_play(seed_track(n));
        lib.record_play(seed_track(n + 1));
        lib
    }

    /// A default library survives a round trip without changes.
    #[test]
    fn default_round_trips() {
        let mut store = Store::in_memory().unwrap();
        let lib = Library::default();
        store.write("a", &lib, &cat()).unwrap();
        assert_eq!(loaded_lib(&store, "a"), Some(lib));
    }

    /// Real library operations produce a library that round-trips and passes the invariant.
    #[test]
    fn operations_round_trip() {
        let mut store = Store::in_memory().unwrap();
        let lib = populated("Ada", 3);
        store.write("a", &lib, &cat()).unwrap();
        let loaded = loaded_lib(&store, "a").unwrap();
        assert_eq!(loaded, lib);
        assert_eq!(loaded.check(&cat()), Ok(()));
    }

    /// A row with an invalid slot must not consume one of the limited rows. With
    /// `MAX_PLAYLISTS` at 2, slots -1, 0 and 1 arrive ordered as -1, 0, 1, so a limit
    /// applied before the check reads -1 and 0, drops -1, and loses valid playlist 1
    /// with its tracks. The next write then deletes that playlist from disk.
    #[test]
    fn an_invalid_slot_does_not_hide_a_valid_one() {
        let store = Store::in_memory().unwrap();
        store.conn.execute(
            "INSERT INTO user(id, name, updated_at) VALUES ('u', 'U', 0)",
            [],
        ).unwrap();
        // Every slot the limit could reach, including an invalid one first.
        for slot in -1..=(MAX_PLAYLISTS as i64) {
            store.conn.execute(
                "INSERT INTO playlist(user_id, slot, name) VALUES ('u', ?1, ?2)",
                params![slot, format!("p{slot}")],
            ).unwrap();
        }
        let loaded = store.load("u", &mut cat()).unwrap().unwrap();
        assert_eq!(
            loaded.lib.playlists.iter().flatten().count(),
            MAX_PLAYLISTS,
            "an invalid slot used up one of the limited rows"
        );
        for (slot, playlist) in loaded.lib.playlists.iter().enumerate() {
            assert!(
                playlist.is_some(),
                "slot {slot} was lost, and the next write would delete it from disk"
            );
        }
    }

    /// History has the same shape as the other two limited queries after all. The limit
    /// is rows read, and a row that names nothing is dropped after it, so an invalid row
    /// inside the limit hides a valid one past it, and the next write deletes that entry.
    ///
    /// The first version of this test used a duplicate as the second row, and a
    /// duplicate hides nothing: it reads two rows, one valid, and both assertions hold.
    /// It has to be an invalid row followed by distinct valid ones.
    #[test]
    fn an_unknown_history_row_does_not_hide_a_valid_one() {
        let store = Store::in_memory().unwrap();
        store.conn.execute(
            "INSERT INTO user(id, name, updated_at) VALUES ('u', 'U', 0)",
            [],
        ).unwrap();
        // Row 0 names nothing the catalogue has, so it is dropped after the limit; rows
        // 1 and 2 are distinct and valid, and both must survive.
        plant(
            &store,
            "INSERT INTO history(user_id, position, track_id)
             VALUES ('u', 0, 'unknown'), ('u', 1, 'seed-track-0'), ('u', 2, 'seed-track-1')",
        );
        let loaded = store.load("u", &mut cat()).unwrap().unwrap();
        assert_eq!(
            loaded.lib.history,
            vec![seed_track(0), seed_track(1)],
            "an invalid row used up one of the limited history rows, so the last entry was lost"
        );
    }

    /// The collection loader must not let an invalid index use up one of the limited
    /// rows. It is the same mistake the playlist loader had: a `LIMIT` applied before
    /// the index check reads a negative row first, drops it, and stops one short, so
    /// the last valid collection is absent and the next write deletes it.
    #[test]
    fn an_invalid_collection_index_does_not_hide_a_valid_one() {
        let store = Store::in_memory().unwrap();
        store.conn.execute(
            "INSERT INTO user(id, name, updated_at) VALUES ('u', 'U', 0)",
            [],
        ).unwrap();
        for idx in -1..=(MAX_COLLECTIONS as i64) {
            store.conn.execute(
                "INSERT INTO collection(user_id, idx, name) VALUES ('u', ?1, ?2)",
                params![idx, format!("c{idx}")],
            ).unwrap();
        }
        let loaded = store.load("u", &mut cat()).unwrap().unwrap();
        assert_eq!(
            loaded.lib.collections.len(),
            MAX_COLLECTIONS,
            "an invalid index used up one of the limited collection rows"
        );
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
            let track = seed_track(next() as usize);
            let album = seed_album(next() as usize);
            match next() % 19 {
                0 => lib.toggle_like(track),
                1 => lib.toggle_save_album(album),
                2 => lib.toggle_follow(seed_artist(next() as usize)),
                3 => lib.toggle_download(album),
                4 => lib.remove_download(&album),
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
                    lib.remove_track(playlist, &track);
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
            assert_eq!(lib.check(&cat()), Ok(()));
            if step % 17 == 0 || step == 1_999 {
                store.write("a", &lib, &cat()).unwrap();
                let loaded = loaded_lib(&store, "a").unwrap();
                assert_eq!(loaded, lib, "step {step}");
                assert_eq!(loaded.check(&cat()), Ok(()));
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
        store.write("a", &lib, &cat()).unwrap();
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
        store.write("a", &lib, &cat()).unwrap();
        store.write("a", &lib, &cat()).unwrap();
        assert_eq!(loaded_lib(&store, "a"), Some(lib.clone()));
        let count: u32 = store
            .conn
            .query_row(
                "SELECT count(*) FROM \"like\" WHERE user_id = 'a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, lib.liked.len() as u32);
    }

    /// Two users can store different libraries with the same playlist and collection indices.
    #[test]
    fn users_are_isolated() {
        let mut store = Store::in_memory().unwrap();
        let a = populated("Ada", 0);
        let b = populated("Bea", 4);
        store.write("a", &a, &cat()).unwrap();
        store.write("b", &b, &cat()).unwrap();
        store.write("a", &a, &cat()).unwrap();
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
        lib.toggle_like(seed_track(0));
        lib.toggle_like(seed_track(1));
        store.write("a", &lib, &cat()).unwrap();
        plant(&store, "INSERT INTO \"like\"(user_id, track_id) VALUES ('a', '999')");

        let loaded = store.load("a", &mut cat()).unwrap().unwrap();
        assert_eq!(loaded.dropped, 1);
        assert_eq!(loaded.lib, lib, "the good likes did not survive");
        assert_eq!(loaded.lib.check(&cat()), Ok(()));
    }

    /// A playlist keeps its name and its remaining tracks when one track is
    /// unknown. The container survives, only the leaf goes.
    #[test]
    fn a_container_survives_a_dropped_leaf() {
        let mut store = Store::in_memory().unwrap();
        let mut lib = Library::default();
        let id = lib.create_playlist("Road trip".to_string()).unwrap();
        lib.add_track(id, seed_track(0));
        lib.add_track(id, seed_track(1));
        store.write("a", &lib, &cat()).unwrap();
        plant(&store, "UPDATE playlist_track SET track_id = '999' WHERE position = 1");

        let loaded = store.load("a", &mut cat()).unwrap().unwrap();
        assert_eq!(loaded.dropped, 1);
        let playlist = loaded.lib.playlist(id).expect("the playlist disappeared");
        assert_eq!(playlist.name, "Road trip");
        assert_eq!(playlist.tracks, vec![seed_track(0)]);
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
        store.write("a", &lib, &cat()).unwrap();
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

        let loaded = store.load("a", &mut cat()).unwrap().unwrap();
        assert_eq!(loaded.lib.collections.len(), 1);
        assert_eq!(loaded.lib.collections[0].name, "Second");
        assert_eq!(
            loaded.lib.collections[0].playlists,
            vec![id],
            "the reference landed on the wrong collection"
        );
        assert_eq!(loaded.lib.check(&cat()), Ok(()));
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
        store.write("a", &lib, &cat()).unwrap();
        store
            .conn
            .execute("INSERT INTO collection_playlist VALUES ('a', 0, 0, 1)", [])
            .unwrap();

        let loaded = store.load("a", &mut cat()).unwrap().unwrap();
        assert_eq!(loaded.dropped, 1);
        assert_eq!(loaded.lib.collections[0].name, "C");
        assert!(loaded.lib.collections[0].playlists.is_empty());
        assert_eq!(loaded.lib.check(&cat()), Ok(()));
    }

    /// A duplicated history entry is dropped, because a duplicate would fail
    /// the invariant the whole app relies on.
    #[test]
    fn a_duplicate_history_entry_is_dropped() {
        let mut store = Store::in_memory().unwrap();
        store.write("a", &Library::default(), &cat()).unwrap();
        plant(&store, "INSERT INTO history VALUES ('a', 0, 'seed-track-0'), ('a', 1, 'seed-track-0')");

        let loaded = store.load("a", &mut cat()).unwrap().unwrap();
        assert_eq!(loaded.dropped, 1);
        assert_eq!(loaded.lib.history, vec![seed_track(0)]);
        assert_eq!(loaded.lib.check(&cat()), Ok(()));
    }

    /// A failed write rolls back the user row and all child rows.
    #[test]
    fn a_failed_write_is_atomic() {
        let mut store = Store::in_memory().unwrap();
        let lib = populated("Ada", 0);
        store.write("a", &lib, &cat()).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER fail_history BEFORE INSERT ON history
             BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();
        assert!(store.write("a", &populated("Changed", 2), &cat()).is_err());
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
            ("tidal_playlist", "playlist_id"),
            ("download", "album_id"),
            ("playlist_track", "track_id"),
            ("history", "track_id"),
        ] {
            let mut store = Store::in_memory().unwrap();
            store.write("a", &populated("Ada", 0), &cat()).unwrap();
            plant(&store, &format!("UPDATE \"{table}\" SET {column} = 'unknown' WHERE user_id = 'a'"));
            let loaded = store.load("a", &mut cat()).unwrap().unwrap();
            assert!(loaded.dropped > 0, "{table} accepted an unknown id");
            assert_eq!(loaded.lib.check(&cat()), Ok(()), "{table} broke the invariant");
        }
    }

    /// Replacing one user's populated library deletes stale rows without changing another user's library.
    #[test]
    fn replacement_removes_stale_rows() {
        let mut store = Store::in_memory().unwrap();
        let b = populated("Bea", 3);
        store.write("a", &populated("Ada", 0), &cat()).unwrap();
        store.write("b", &b, &cat()).unwrap();
        let empty = Library::default();
        store.write("a", &empty, &cat()).unwrap();
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
        store.write("a", &Library::default(), &cat()).unwrap();
        for position in 0..HISTORY_CAP + 3 {
            plant(
                &store,
                &format!("INSERT INTO history VALUES ('a', {position}, 'seed-track-{position}')"),
            );
        }
        let loaded = store.load("a", &mut cat()).unwrap().unwrap();
        assert_eq!(loaded.lib.history.len(), HISTORY_CAP);
        assert_eq!(loaded.dropped, 3);
        assert_eq!(loaded.lib.check(&cat()), Ok(()));
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

    /// The schema the version 1 build created, frozen here so the migration tests
    /// start from the file a user has on disk.
    const V1_SCHEMA: &str = "
        CREATE TABLE user(id TEXT PRIMARY KEY, name TEXT NOT NULL,
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
            PRIMARY KEY(user_id, position));
        PRAGMA user_version = 1;";

    /// One account's version 1 rows: a name, a playlist with a track, a collection
    /// that holds it, and one row in each id table.
    const V1_ROWS: &str = "
        INSERT INTO user VALUES ('u', 'Ada', 0);
        INSERT INTO playlist VALUES (0, 'u', 'Road trip');
        INSERT INTO playlist_track VALUES ('u', 0, 0, 3);
        INSERT INTO collection VALUES (0, 'u', 'Trips');
        INSERT INTO collection_playlist VALUES ('u', 0, 0, 0);
        INSERT INTO \"like\" VALUES ('u', 3);
        INSERT INTO save VALUES ('u', 1);
        INSERT INTO follow VALUES ('u', 1);
        INSERT INTO download VALUES ('u', 1);
        INSERT INTO history VALUES ('u', 0, 3);";

    /// Version 1 ids name placeholder content, so the migration drops every row
    /// that holds one. What the user wrote stays: the name, the playlist with its
    /// slot, and the collection that holds it.
    #[test]
    fn a_version_one_file_keeps_its_playlists_and_drops_its_catalogue_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        conn.execute_batch(V1_ROWS).unwrap();
        let store = Store::configure(conn).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        let loaded = store.load("u", &mut cat()).unwrap().unwrap();
        assert_eq!(loaded.dropped, 0, "the migration left a row that load had to drop");
        assert_eq!(loaded.lib.name, "Ada");
        let playlist = loaded.lib.playlist(PlaylistId(0)).expect("the playlist was lost");
        assert_eq!(playlist.name, "Road trip");
        assert!(playlist.tracks.is_empty(), "a placeholder track survived");
        assert_eq!(loaded.lib.collections[0].name, "Trips");
        assert_eq!(loaded.lib.collections[0].playlists, vec![PlaylistId(0)]);
        assert!(loaded.lib.liked.is_empty());
        assert!(loaded.lib.saved_albums.is_empty());
        assert!(loaded.lib.followed.is_empty());
        assert!(loaded.lib.downloaded.is_empty());
        assert!(loaded.lib.history.is_empty());
    }

    /// Every table, column and key a fresh file has, a migrated file has too, so
    /// no code path can depend on how the file came to be.
    #[test]
    fn a_fresh_and_a_migrated_schema_match() {
        fn schema(store: &Store) -> Vec<(String, String, Option<String>)> {
            let mut stmt = store
                .conn
                .prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")
                .unwrap();
            stmt.query_map([], |row| {
                let sql: Option<String> = row.get(2)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    sql.map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")),
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
        }
        let fresh = Store::in_memory().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        conn.execute_batch(V1_ROWS).unwrap();
        let migrated = Store::configure(conn).unwrap();
        assert_eq!(schema(&migrated), schema(&fresh), "a version 1 file migrated to another schema");
        let migrated = Store::configure(version_2_file()).unwrap();
        assert_eq!(schema(&migrated), schema(&fresh), "a version 2 file migrated to another schema");
        let migrated = Store::configure(version_3_file()).unwrap();
        assert_eq!(schema(&migrated), schema(&fresh), "a version 3 file migrated to another schema");
        let migrated = Store::configure(version_4_file()).unwrap();
        assert_eq!(schema(&migrated), schema(&fresh), "a version 4 file migrated to another schema");
    }

    /// A version 4 file, as the version 4 build left it: no member positions, and
    /// one account with several rows in every membership table.
    fn version_4_file() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(USER_TABLES).unwrap();
        conn.execute_batch(CATALOGUE_ID_TABLES).unwrap();
        conn.execute_batch(CATALOGUE_PLAYLIST_TABLES).unwrap();
        conn.execute_batch(COVER_COLUMNS).unwrap();
        conn.pragma_update(None, "user_version", 4).unwrap();
        conn.execute_batch(
            "INSERT INTO user VALUES ('u', 'Ada', 0);
             INSERT INTO artist VALUES ('seed-artist-0', 'Tame Impala'), ('seed-artist-1', 'Frank Ocean');
             INSERT INTO album VALUES ('seed-album-0', 'Currents', 'seed-artist-0', ''),
                 ('seed-album-1', 'Blonde', 'seed-artist-1', '');
             INSERT INTO track VALUES ('seed-track-2', 'Eventually', 'seed-artist-0', 'seed-album-0', 'PT5M19S'),
                 ('seed-track-0', 'Let It Happen', 'seed-artist-0', 'seed-album-0', 'PT7M46S'),
                 ('seed-track-1', 'The Less I Know the Better', 'seed-artist-0', 'seed-album-0', 'PT3M38S');
             INSERT INTO catalogue_playlist VALUES ('f6b9f0a1', 'Evening Drive', '', ''),
                 ('a0c1', 'Morning', '', '');
             INSERT INTO catalogue_playlist_track VALUES ('f6b9f0a1', 0, 'seed-track-1'),
                 ('f6b9f0a1', 1, 'seed-track-0');
             INSERT INTO \"like\" VALUES ('u', 'seed-track-2'), ('u', 'seed-track-0'), ('u', 'seed-track-1');
             INSERT INTO save VALUES ('u', 'seed-album-1'), ('u', 'seed-album-0');
             INSERT INTO follow VALUES ('u', 'seed-artist-1'), ('u', 'seed-artist-0');
             INSERT INTO tidal_playlist VALUES ('u', 'f6b9f0a1'), ('u', 'a0c1');
             INSERT INTO download VALUES ('u', 'seed-album-0');",
        )
        .unwrap();
        conn
    }

    /// A version 4 file stored no order, so every membership survives and loads
    /// in id order, and a playlist keeps its stored track order.
    #[test]
    fn a_version_4_file_keeps_every_membership() {
        let store = Store::configure(version_4_file()).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let mut next_run = cat();
        let loaded = store.load("u", &mut next_run).unwrap().unwrap();
        assert_eq!(loaded.dropped, 0, "the migration left a row that load had to drop");
        let lib = loaded.lib;
        assert_eq!(lib.liked.to_vec(), vec![seed_track(0), seed_track(1), seed_track(2)]);
        assert_eq!(lib.saved_albums.to_vec(), vec![seed_album(0), seed_album(1)]);
        assert_eq!(lib.followed.to_vec(), vec![seed_artist(0), seed_artist(1)]);
        let playlists: Vec<&str> = lib.tidal_playlists.iter().map(|p| p.as_str()).collect();
        assert_eq!(playlists, ["a0c1", "f6b9f0a1"]);
        assert!(lib.is_downloaded(&seed_album(0)));
        let evening = next_run.playlist(&next_run.playlist_id("f6b9f0a1").unwrap()).unwrap();
        assert_eq!(evening.tracks.to_vec(), vec![seed_track(1), seed_track(0)]);
    }

    /// Tidal's order is the order a screen shows, so it must survive a write and
    /// a load. The ids are listed out of id order, so a load by id would fail.
    #[test]
    fn remote_order_survives_a_restart() {
        let mut store = Store::in_memory().unwrap();
        let mut lib = Library::default();
        lib.liked.merge(&[seed_track(9), seed_track(2), seed_track(5)], Reach::Complete);
        lib.saved_albums.merge(&[seed_album(7), seed_album(1)], Reach::Complete);
        lib.followed.merge(&[seed_artist(4), seed_artist(0)], Reach::Complete);
        lib.tidal_playlists.merge(&[seed_playlist(2), seed_playlist(0)], Reach::Complete);
        store.write("a", &lib, &cat()).unwrap();
        let loaded = loaded_lib(&store, "a").unwrap();
        assert_eq!(loaded.liked.to_vec(), vec![seed_track(9), seed_track(2), seed_track(5)]);
        assert_eq!(loaded.saved_albums.to_vec(), vec![seed_album(7), seed_album(1)]);
        assert_eq!(loaded.followed.to_vec(), vec![seed_artist(4), seed_artist(0)]);
        assert_eq!(loaded.tidal_playlists.to_vec(), vec![seed_playlist(2), seed_playlist(0)]);
    }

    /// A version 2 file, as the version 2 build left it: the tables without the
    /// catalogue playlists, and one account with a row in every table.
    fn version_2_file() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(USER_TABLES).unwrap();
        conn.execute_batch(CATALOGUE_ID_TABLES).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute_batch(
            "INSERT INTO user VALUES ('u', 'Ada', 0);
             INSERT INTO artist VALUES ('seed-artist-0', 'Tame Impala');
             INSERT INTO album VALUES ('seed-album-0', 'Currents', 'seed-artist-0');
             INSERT INTO track VALUES ('seed-track-0', 'Let It Happen', 'seed-artist-0', 'seed-album-0', 'PT7M46S');
             INSERT INTO track VALUES ('seed-track-1', 'The Less I Know the Better', 'seed-artist-0', 'seed-album-0', 'PT3M38S');
             INSERT INTO \"like\" VALUES ('u', 'seed-track-0');
             INSERT INTO save VALUES ('u', 'seed-album-0');
             INSERT INTO follow VALUES ('u', 'seed-artist-0');
             INSERT INTO download VALUES ('u', 'seed-album-0');
             INSERT INTO playlist VALUES (0, 'u', 'Road trip');
             INSERT INTO playlist_track VALUES ('u', 0, 0, 'seed-track-1');
             INSERT INTO collection VALUES (0, 'u', 'Trips');
             INSERT INTO collection_playlist VALUES ('u', 0, 0, 0);
             INSERT INTO history VALUES ('u', 0, 'seed-track-1');",
        )
        .unwrap();
        conn
    }

    /// Version 2 already holds every row version 3 keeps. The migration only adds
    /// the catalogue playlist tables, so the whole library loads unchanged.
    #[test]
    fn a_version_2_file_keeps_every_row() {
        let store = Store::configure(version_2_file()).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        let mut expected = Library::default();
        expected.set_name("Ada".to_string());
        expected.toggle_like(seed_track(0));
        expected.toggle_save_album(seed_album(0));
        expected.toggle_follow(seed_artist(0));
        expected.toggle_download(seed_album(0));
        let id = expected.create_playlist("Road trip".to_string()).unwrap();
        expected.add_track(id, seed_track(1));
        expected.create_collection("Trips".to_string());
        expected.add_to_collection(0, id);
        expected.record_play(seed_track(1));
        assert_eq!(loaded_lib(&store, "u"), Some(expected), "the migration lost a version 2 row");
    }

    /// A version 3 file, as the version 3 build left it: no cover columns, one
    /// account with a saved album and a Tidal playlist.
    fn version_3_file() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(USER_TABLES).unwrap();
        conn.execute_batch(CATALOGUE_ID_TABLES).unwrap();
        conn.execute_batch(CATALOGUE_PLAYLIST_TABLES).unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();
        conn.execute_batch(
            "INSERT INTO user VALUES ('u', 'Ada', 0);
             INSERT INTO artist VALUES ('seed-artist-0', 'Tame Impala');
             INSERT INTO album VALUES ('77610756', 'Fetched', 'seed-artist-0');
             INSERT INTO save VALUES ('u', '77610756');
             INSERT INTO catalogue_playlist VALUES ('f6b9f0a1', 'Evening Drive', '');
             INSERT INTO tidal_playlist VALUES ('u', 'f6b9f0a1');",
        )
        .unwrap();
        conn
    }

    /// A version 3 file gains empty covers and keeps every row it had.
    #[test]
    fn a_version_3_file_gains_empty_covers() {
        let store = Store::configure(version_3_file()).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let mut next_run = cat();
        let loaded = store.load("u", &mut next_run).unwrap().unwrap();
        assert_eq!(loaded.dropped, 0);
        assert_eq!(loaded.lib.saved_albums.len(), 1);
        assert_eq!(loaded.lib.tidal_playlists.len(), 1);
        let album = next_run.album(&next_run.album_id("77610756").unwrap()).unwrap();
        assert_eq!(album.cover, None);
    }

    /// The library and catalogue with an album and a Tidal playlist that each
    /// carry a cover.
    fn covered() -> (Library, Catalogue) {
        let mut fetched = cat();
        fetched.absorb(CatalogueRows {
            albums: vec![AlbumRow {
                id: "77610756".into(),
                title: "Fetched".into(),
                artist: "seed-artist-0".into(),
                cover: "251380836-large".into(),
            }],
            playlists: vec![PlaylistRow {
                id: "f6b9f0a1".into(),
                name: "Evening Drive".into(),
                description: String::new(),
                tracks: vec!["seed-track-0".into()],
                cover: "playlist-cover-1".into(),
            }],
            ..CatalogueRows::default()
        });
        let mut lib = Library::default();
        lib.toggle_save_album(fetched.album_id("77610756").unwrap());
        lib.tidal_playlists.insert(fetched.playlist_id("f6b9f0a1").unwrap());
        (lib, fetched)
    }

    /// A cover outlives the session that fetched it: a later run whose catalogue
    /// lacks the album and the playlist reads both covers back from the file.
    #[test]
    fn a_cover_survives_a_restart() {
        let mut store = Store::in_memory().unwrap();
        let (lib, fetched) = covered();
        store.write("a", &lib, &fetched).unwrap();

        let mut next_run = cat();
        store.load("a", &mut next_run).unwrap();
        let album = next_run.album(&next_run.album_id("77610756").unwrap()).unwrap();
        let playlist = next_run.playlist(&next_run.playlist_id("f6b9f0a1").unwrap()).unwrap();
        assert_eq!(album, fetched.album(&fetched.album_id("77610756").unwrap()).unwrap());
        assert_eq!(playlist, fetched.playlist(&fetched.playlist_id("f6b9f0a1").unwrap()).unwrap());
        assert_eq!(album.cover.as_ref().map(|c| c.as_str()), Some("251380836-large"));
        assert_eq!(playlist.cover.as_ref().map(|c| c.as_str()), Some("playlist-cover-1"));
    }

    /// A damaged cover costs only the cover. A blob or a malformed id leaves the
    /// album and the playlist whole, and nothing is counted as dropped.
    #[test]
    fn a_non_text_cover_keeps_its_album() {
        let mut store = Store::in_memory().unwrap();
        let (lib, fetched) = covered();
        store.write("a", &lib, &fetched).unwrap();
        plant(&store, "UPDATE album SET cover = X'00' WHERE id = '77610756'");
        plant(&store, "UPDATE catalogue_playlist SET cover = 'a/b' WHERE id = 'f6b9f0a1'");

        let mut next_run = cat();
        let loaded = store.load("a", &mut next_run).unwrap().unwrap();
        assert_eq!(loaded.dropped, 0);
        assert_eq!(loaded.lib, lib, "a damaged cover hid its album or playlist");
        assert_eq!(next_run.album(&next_run.album_id("77610756").unwrap()).unwrap().cover, None);
        assert_eq!(next_run.playlist(&next_run.playlist_id("f6b9f0a1").unwrap()).unwrap().cover, None);
    }

    /// A membership that names a playlist the catalogue lacks must not use up a
    /// limited row. The library has no cap on Tidal playlists today, so this
    /// guards the next one added: it must count valid rows, not rows read.
    #[test]
    fn an_unknown_tidal_playlist_does_not_hide_a_valid_one() {
        let mut store = Store::in_memory().unwrap();
        store.write("u", &Library::default(), &cat()).unwrap();
        plant(
            &store,
            "INSERT INTO tidal_playlist(user_id, playlist_id) VALUES ('u', '0-unknown'), ('u', 'seed-playlist-1')",
        );
        let loaded = store.load("u", &mut cat()).unwrap().unwrap();
        assert_eq!(loaded.dropped, 1, "the unknown playlist was not counted");
        assert_eq!(
            loaded.lib.tidal_playlists,
            Members::from_ordered([seed_playlist(1)]),
            "an unknown playlist hid a valid one"
        );
    }

    /// A Tidal playlist outlives the session that fetched it: a write stores its
    /// name and tracks, so a later run whose catalogue lacks it loads it whole.
    #[test]
    fn a_tidal_playlist_survives_a_restart() {
        let mut store = Store::in_memory().unwrap();
        let mut fetched = cat();
        fetched.absorb(CatalogueRows {
            playlists: vec![PlaylistRow {
                id: "f6b9f0a1".into(),
                name: "Evening Drive".into(),
                description: "Remote".into(),
                tracks: vec!["seed-track-4".into(), "seed-track-2".into()],
                cover: String::new(),
            }],
            ..CatalogueRows::default()
        });
        let mut lib = Library::default();
        lib.tidal_playlists.insert(fetched.playlist_id("f6b9f0a1").unwrap());
        store.write("a", &lib, &fetched).unwrap();

        let mut next_run = cat();
        assert!(next_run.playlist_id("f6b9f0a1").is_none(), "the fixture is already in the seed");
        assert_eq!(loaded_lib(&store, "a"), Some(lib.clone()));
        store.load("a", &mut next_run).unwrap();
        let playlist = next_run.playlist(&next_run.playlist_id("f6b9f0a1").unwrap()).unwrap();
        assert_eq!(playlist.name, "Evening Drive");
        assert_eq!(playlist.tracks.to_vec(), vec![seed_track(4), seed_track(2)], "the tracks or their order were lost");
    }

    /// A migration that fails partway leaves the version 1 file whole, so the next
    /// open can migrate it again. A view named `history` makes the last drop fail
    /// after the `like` table is already gone inside the transaction.
    #[test]
    fn a_failed_migration_leaves_version_one() {
        let dir = TempDirGuard(std::env::temp_dir().join(format!(
            "opentidal-migrate-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )));
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("lib.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(V1_SCHEMA).unwrap();
            conn.execute_batch(V1_ROWS).unwrap();
            conn.execute_batch(
                "DROP TABLE history;
                 CREATE VIEW history AS SELECT user_id, position, track_id FROM playlist_track;",
            )
            .unwrap();
        }

        assert!(Store::open(&path).is_err(), "the migration passed over a view it cannot drop");

        let conn = Connection::open(&path).unwrap();
        let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
        assert_eq!(version, 1, "a failed migration moved the version");
        let likes: i64 = conn
            .query_row("SELECT count(*) FROM \"like\"", [], |row| row.get(0))
            .expect("the like table was dropped outside the transaction");
        assert_eq!(likes, 1, "a failed migration lost the version 1 rows");
    }

    /// A TEXT column turns a number into text, but a blob stays a blob. A blob id
    /// costs only its own row, never the whole library.
    #[test]
    fn a_non_text_id_is_dropped_not_fatal() {
        let mut store = Store::in_memory().unwrap();
        let mut lib = Library::default();
        lib.toggle_like(seed_track(0));
        lib.toggle_like(seed_track(1));
        store.write("a", &lib, &cat()).unwrap();
        plant(&store, "INSERT INTO \"like\"(user_id, track_id) VALUES ('a', X'00')");

        let loaded = store.load("a", &mut cat()).unwrap().unwrap();
        assert_eq!(loaded.dropped, 1);
        assert_eq!(loaded.lib, lib, "the text likes did not survive");
    }

    /// A write stores the catalogue rows the library names, so a later run whose
    /// catalogue lacks them gets them back from the file and loads the library
    /// whole. This is what lets a Tidal item outlive the session that fetched it.
    #[test]
    fn a_write_stores_the_catalogue_rows_it_names() {
        let mut store = Store::in_memory().unwrap();
        let mut fetched = cat();
        fetched.absorb(CatalogueRows {
            artists: vec![ArtistRow { id: "3346".into(), name: "Fetched".into() }],
            albums: vec![AlbumRow { id: "77610756".into(), title: "Fetched".into(), artist: "3346".into(), cover: String::new() }],
            tracks: vec![TrackRow {
                id: "77610757".into(),
                title: "Fetched".into(),
                artist: "3346".into(),
                album: "77610756".into(),
                duration: "PT3M26S".into(),
            }],
            playlists: Vec::new(),
        });
        let mut lib = Library::default();
        lib.toggle_like(fetched.track_id("77610757").unwrap());
        lib.toggle_save_album(fetched.album_id("77610756").unwrap());
        store.write("a", &lib, &fetched).unwrap();

        let mut next_run = cat();
        assert!(next_run.track_id("77610757").is_none(), "the fixture is already in the seed");
        let loaded = store.load("a", &mut next_run).unwrap().unwrap();
        assert_eq!(loaded.dropped, 0, "a fetched item did not survive the restart");
        assert_eq!(loaded.lib, lib);
        let track = next_run.track(&next_run.track_id("77610757").unwrap()).unwrap();
        assert_eq!((track.title.as_str(), track.duration), ("Fetched", 206));
    }

    /// The foreign keys refuse a library row whose catalogue row is absent, so the
    /// disk cannot hold a reference that a later load would have to drop.
    #[test]
    fn a_library_row_needs_its_catalogue_row() {
        let mut store = Store::in_memory().unwrap();
        store.write("a", &Library::default(), &cat()).unwrap();
        let insert = store.conn.execute("INSERT INTO \"like\" VALUES ('a', 'seed-track-0')", []);
        assert!(insert.is_err(), "a like reached the disk without its track row");
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
            first.write("173210380", &lib, &cat()).unwrap();
        }

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the database is not private");

        let second = Store::open(&path).unwrap();
        assert_eq!(loaded_lib(&second, "173210380"), Some(lib));
        assert_eq!(second.schema_version().unwrap(), SCHEMA_VERSION);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
