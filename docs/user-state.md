# User state on disk

The signed-in user and their library survive a restart in one SQLite file.
A fresh install has no session. Sign-in writes the token to the keychain and
the user row to the file. A restart shows the name before any network call.

The keychain holds the secret and nothing else. The file holds no token, so a
copy of the file grants no access to Tidal.

## Key

The key is the Tidal user id. It comes from the `uid` claim of the access
token, through `auth::decode_claims(...).user_id()` in `src/auth.rs`. That
function already exists and already has a test.

```rust
let token = store.load()?;                 // keychain
let uid = decode_claims(&token.access_token)?.user_id()?;
```

The key needs no hash and no network call. It does not change on refresh, so
a refresh never touches the file.

A hash of the access token does not work. The access token rotates on every
refresh, so the key would move and every refresh would have to move the user
row. A crash between the keychain write and the row move loses the user. The
hash also gives no security, because the token it hashes already sits in the
keychain.

## Store

One file. It sits next to the existing `config.json`, so the app owns one
directory.

```
~/Library/Application Support/OpenTidal/opentidal.db
```

- Mode `0600`. The file holds a name and a listening history.
- `PRAGMA user_version` carries the schema version. Every migration bumps it.
- `PRAGMA foreign_keys = ON`, `journal_mode = WAL`, `synchronous = FULL`.
- Add the file to the Time Machine exclusion list. The data comes back from
  Tidal, so a backup copies a privacy risk for no gain.

## Schema

```
user(id TEXT PRIMARY KEY, name TEXT NOT NULL, updated_at INTEGER NOT NULL)

like(user_id TEXT NOT NULL REFERENCES user, track_id INTEGER NOT NULL,
     PRIMARY KEY (user_id, track_id))
save(user_id TEXT NOT NULL REFERENCES user, album_id INTEGER NOT NULL,
     PRIMARY KEY (user_id, album_id))
follow(user_id TEXT NOT NULL REFERENCES user, artist_id INTEGER NOT NULL,
     PRIMARY KEY (user_id, artist_id))
download(user_id TEXT NOT NULL REFERENCES user, album_id INTEGER NOT NULL,
     PRIMARY KEY (user_id, album_id))

playlist(slot INTEGER NOT NULL, user_id TEXT NOT NULL REFERENCES user,
         name TEXT NOT NULL, PRIMARY KEY (user_id, slot))
playlist_track(user_id TEXT NOT NULL REFERENCES user, slot INTEGER NOT NULL,
               position INTEGER NOT NULL, track_id INTEGER NOT NULL,
               PRIMARY KEY (user_id, slot, position),
               FOREIGN KEY (user_id, slot) REFERENCES playlist(user_id, slot))

collection(idx INTEGER NOT NULL, user_id TEXT NOT NULL REFERENCES user,
           name TEXT NOT NULL, PRIMARY KEY (user_id, idx))
collection_playlist(user_id TEXT NOT NULL REFERENCES user, idx INTEGER NOT NULL,
                    position INTEGER NOT NULL, playlist_slot INTEGER NOT NULL,
                    PRIMARY KEY (user_id, idx, position),
                    FOREIGN KEY (user_id, idx) REFERENCES collection(user_id, idx))

history(user_id TEXT NOT NULL REFERENCES user, position INTEGER NOT NULL,
        track_id INTEGER NOT NULL,
        PRIMARY KEY (user_id, position))
```

Notes:

- Every table hangs off `user.id`. No table assumes one user.
- Playlist slots and collection indices are local to a user. A global slot
  key would prevent two users from each storing slot zero. Child keys include
  `user_id` for the same reason.
- The current model has no stable playlist id and no item timestamps.
  Slots persist as stored. Timestamp columns arrive with the Tidal sync.
- `history` orders by `position`, never by rowid. A rowid can be reused after
  a delete, and history order is user-visible.
- `track_id`, `album_id`, `artist_id` hold integer catalogue indices. The
  catalogue ships with the app and never changes, so the schema cannot carry
  a foreign key to it. `Library::check` validates these ids on load.
- No `is_active` column. The keychain token decides who is active. A stored
  flag would be a second source of truth and could disagree after a crash.

## Start

The start path runs in this order. It never blocks on the network.

1. Keychain has no token. Signed out. Do not open the file.
2. Token present, `user_id()` returns `None`. Signed out. Write the
   `signed-out` sentinel, as `KeychainStore::clear` already does.
3. `user_id()` found, user row found. Signed in at once, name from the row.
4. `user_id()` found, row missing. Signed in, name `You`, profile fetch fills
   the row. A missing row is not an error.
5. If `token_expired(&token)`, refresh. Refresh writes the keychain only.
6. Fetch the profile and update `user.name` and `user.updated_at`.

The refresh in step 5 can fail. A `401` or `400` from the token endpoint means
the refresh token is dead. Then sign out and keep the rows. A network error
means the session is unknown. Stay signed in, retry on the next request, and
do not clear anything.

## Writes

- Sign-in: write the `user` row, then the keychain. If the keychain write
  fails, there is no session and the row is inert.
- Refresh: keychain only. No file write. This is the gain over a rotating key.
- Profile fetch: update `user.name`.
- Library change: write through in the same operation that changes the state.
- Sign-out: keychain sentinel. Keep the rows, so the next sign-in finds the
  library again.

The only point where the two stores can disagree is sign-in, and a stale row
there does nothing. A start with a row and no token reads as signed out.

## The mirror rule

While the app is read-only, the library is a mirror of Tidal, not a truth.
Every row came from Tidal. Tidal still holds it. This settles recovery: a
broken row needs no repair, because the next sync returns it.

The Tidal sync drops and logs a leaf that fails `Library::check`. The container
stays visible with the leaves it has. The next sync restores the leaf.

The disk store has a stricter boundary. `Store::load` returns an error for an
invalid stored library. It never drops rows or repairs data. The app's recovery
path must handle that error when it connects the store to start.

The rule ends the day the app writes to Tidal. From then on a local-only row
exists, a drop loses data the server never had, and recovery needs a real
answer.

## Sync

A sync runs on demand, when a library screen opens. Start reads disk only, so
a launch stays free of network work. The cost of that choice: a user who never
opens a library screen never syncs, and a broken leaf waits until one does.

How it lands: paint the rows from disk first, then update in place as pages
arrive. The app must never show a spinner over a list it already has, or the
sync makes the app slower than the file alone.

A sync is the only thing that creates rows in bulk, and the only thing that
deletes them. It obeys three rules.

1. Merge, never replace. The collection read is paged. Page 4 of 7 can fail,
   and a replace has already deleted everything. A merge makes an unread page
   a no-op.
2. Delete only after a complete read. The sync carries a `complete` flag, set
   when the last page arrives. No delete pass runs on a partial read.
3. Delete only from the tables Tidal owns. Those are `like`, `save`, `follow`,
   `playlist`, `playlist_track`, `collection`, and `collection_playlist`. The
   `download` and `history` tables are local, so the sync never deletes a row
   there.

Rule 3 removes the need for a size guard. A wrong delete from a mirrored table
costs one sync, because the server returns the row again. The two tables where
a wrong delete would lose real work are the two the sync cannot touch.

The offline window is the one cost. A broken leaf stays missing until the
first sync after the network returns. The local copy is as good as the last
successful read.

The delete pass runs through the state operations, never through the store
directly. `Route::UserPlaylist` holds a slot, so a playlist that a route names
can be the target of a delete. `state.rs:272` `delete_playlist` already repairs
the route. Raw SQL would break the invariant on the bulk path.

## Load and the invariant

`src/library.rs` ids are slot indices into fixed arrays, and `src/state.rs`
proves that no route, playlist, or saved item dangles after each operation.
Load is a new way into that state, so it must not bypass the proof.

The rules:

1. Persist `PlaylistId` as `playlist.slot`. The model has no `playlist.id`.
   Preserve empty slots so a collection still names the same playlist.
2. On load, restore each playlist to its exact slot. Restore collections in
   `idx` order and their playlist references in `position` order.
3. Do not derive `Serialize` on `Library`. Map rows into a fresh `Library`
   field by field. Validate catalogue indices before a bit shift or cast.
4. Run `Library::check` on the loaded value. Return `StoreError::Sql` if the
   check fails. Reject a missing playlist reference with its table and slot.
   Load never panics on bad data.
5. The caps in `library.rs` are real: `MAX_PLAYLISTS`, `HISTORY_CAP`,
   `PLAYLIST_TRACK_CAP`, `MAX_COLLECTIONS`, `COLLECTION_PLAYLIST_CAP`. A file
   with more rows than a cap fails load. The store never silently truncates it.
6. Read the library in one transaction so a concurrent save cannot mix two
   versions. Save replaces one user's rows in one transaction.

## Many accounts, one session

The schema allows many users. The UI signs in one account at a time. This is
deliberate: the `user_id` column costs nothing now and the door stays open.

- The token decides the active user. Never query without a `user_id`.
- Rows for another account stay on disk and are inert. They come back on the
  next sign-in with that account.
- Settings > Account needs a `remove account data` action. A second person's
  listening history on this machine must be deletable.

## Evidence

| Claim | Check |
| --- | --- |
| A library survives a round trip | Property test: random ops, save, load, equal, `check` clean |
| Load cannot break the invariant | The same test asserts `Library::check` on the loaded value |
| The store rejects a broken leaf | Seed an unknown track id, assert load returns an error with the table and value |
| Playlist slots survive deletion | Delete slot zero, save and load, assert slot one and its collection reference stay intact |
| Users do not share slots | Save two populated libraries with the same slots, assert each loads unchanged |
| A failed save loses nothing | Abort a child insert with a trigger, assert the previous library still loads |
| A partial sync loses nothing | Fail the read after page 2, assert every row still present |
| A partial sync deletes nothing | Same test, assert the delete pass did not run |
| The sync never touches local tables | Seed a `download` and a `history` row, run a sync, assert both rows remain |
| No data loss at sign-in | Kill matrix: exit between the row write and the keychain write, and the reverse |
| Start needs no network | Run with the network off, name and playlist visible |
| A library screen needs no network | Open Liked Songs offline, assert the disk rows paint and no spinner shows |
| A refresh writes no rows | Count rows before and after a refresh, `user_version` unchanged |
| File is private | `stat -f '%Lp'` returns `600` |
| Schema is sound | `PRAGMA integrity_check` returns `ok`, and no row has an orphan `user_id` |
| Token is not in the file | `strings opentidal.db` shows no token |
| The key holds offline | `uid` read from the keychain alone, no network, before any fetch |

## Confirmed live

Read against a real account on 2026-11-15.

- `uid` was `173210380` on the first sign-in, after a sign-out and a fresh
  sign-in, and after a token refresh. The key holds across all three, so a
  refresh needs no row move.
- The access token lives about four hours. A doctored `exp` claim sends the app
  down the refresh path, so the refresh is testable without the wait.
- After sign-out the keychain holds the `signed-out` sentinel, and a start reads
  it as no token.
