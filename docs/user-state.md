# User state on disk

The signed-in user and their library survive a restart in one SQLite file.
A fresh install has no session. Sign-in writes the token to the keychain and
the user row to the file. A restart shows the name before any network call.

Rule this design keeps: the keychain holds the secret and nothing else. The
file holds no token, so a copy of the file grants no access to Tidal.

## How to read this

One section is one decision. Each section states the rule the code must obey
and the evidence that checks it. Open questions sit at the end.

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

Rejected: a hash of the access token. The access token rotates on every
refresh, so the key would move and every refresh would have to move the user
row. A crash between the keychain write and the row move loses the user. The
hash also buys no security, because the token it hashes already sits in the
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

like(user_id  TEXT NOT NULL REFERENCES user, track_id  TEXT NOT NULL,
     added_at INTEGER NOT NULL, PRIMARY KEY (user_id, track_id))
save(user_id  TEXT NOT NULL REFERENCES user, album_id  TEXT NOT NULL,
     added_at INTEGER NOT NULL, PRIMARY KEY (user_id, album_id))
follow(user_id TEXT NOT NULL REFERENCES user, artist_id TEXT NOT NULL,
     added_at INTEGER NOT NULL, PRIMARY KEY (user_id, artist_id))
download(user_id TEXT NOT NULL REFERENCES user, album_id TEXT NOT NULL,
     PRIMARY KEY (user_id, album_id))

playlist(id INTEGER PRIMARY KEY, user_id TEXT NOT NULL REFERENCES user,
         name TEXT NOT NULL, position INTEGER NOT NULL)
playlist_track(playlist_id INTEGER NOT NULL REFERENCES playlist,
               position INTEGER NOT NULL, track_id TEXT NOT NULL,
               PRIMARY KEY (playlist_id, position))

collection(id INTEGER PRIMARY KEY, user_id TEXT NOT NULL REFERENCES user,
           name TEXT NOT NULL, position INTEGER NOT NULL)
collection_playlist(collection_id INTEGER NOT NULL REFERENCES collection,
                    playlist_id INTEGER NOT NULL REFERENCES playlist,
                    position INTEGER NOT NULL,
                    PRIMARY KEY (collection_id, position))

history(user_id TEXT NOT NULL REFERENCES user, position INTEGER NOT NULL,
        track_id TEXT NOT NULL, played_at INTEGER NOT NULL,
        PRIMARY KEY (user_id, position))
```

Notes:

- Every table hangs off `user.id`. No table assumes one user.
- `history` orders by `position`, never by rowid. A rowid can be reused after
  a delete, and history order is user-visible.
- `track_id`, `album_id`, `artist_id` hold catalogue ids. The catalogue ships
  with the app and never changes, so the schema cannot carry a foreign key to
  it. `Library::check` validates these ids on load.
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

## Load and the invariant

`src/library.rs` ids are slot indices into fixed arrays, and `src/state.rs`
proves that no route, playlist, or saved item dangles after each operation.
Load is a new way into that state, so it must not bypass the proof.

The rules:

1. Persist `playlist.id`, not `PlaylistId`. `PlaylistId` is a slot. A hole in
   the slot array would make a stored slot name the wrong playlist.
2. On load, fill slots in ascending `playlist.id` order. Routes are not
   persisted, so a renumber between runs is safe.
3. Do not derive `Serialize` on `Library`. Map rows into a fresh `Library`
   through the constructors that every other change uses.
4. Run `Library::check` on the loaded value. A row that fails the check is
   dropped and logged. Load never panics on bad data.
5. The caps in `library.rs` are real: `MAX_PLAYLISTS`, `HISTORY_CAP`,
   `PLAYLIST_TRACK_CAP`, `MAX_COLLECTIONS`, `COLLECTION_PLAYLIST_CAP`. A file
   with more rows than a cap is truncated in `position` order, newest kept,
   and the truncation is logged.

## Many accounts, one session

The schema allows many users. The UI signs in one account at a time. This is
deliberate: the `user_id` column costs nothing now and the door stays open.

- The token decides the active user. Never query without a `user_id`.
- Rows for another account stay on disk and are inert. They come back on the
  next sign-in with that account.
- Settings > Account needs a **remove account data** action. A second person's
  listening history on this machine must be deletable.

## Evidence

| Claim | Check |
| --- | --- |
| A library survives a round trip | Property test: random ops, save, load, equal, `check` clean |
| Load cannot break the invariant | The same test asserts `Library::check` on the loaded value |
| No data loss at sign-in | Kill matrix: exit between the row write and the keychain write, and the reverse |
| Start needs no network | Run with the network off, name and playlist visible |
| A refresh writes no rows | Count rows before and after a refresh, `user_version` unchanged |
| File is private | `stat -f '%Lp'` returns `600` |
| Schema is sound | `PRAGMA integrity_check` returns `ok`, and no row has an orphan `user_id` |
| Token is not in the file | `strings opentidal.db | grep` finds no token |

## Open questions

1. Does `uid` stay the same across a refresh and across a fresh sign-in with
   the same account? Check one live login. If it changes, the key is wrong and
   the design stops here.
2. Should a failed `Library::check` drop the row, or refuse the session and
   show a repair screen? Dropping is silent and safe. Refusing is loud and
   honest.
3. Does the app persist the navigation stack? If yes, routes must store
   `playlist.id` and not a slot.
