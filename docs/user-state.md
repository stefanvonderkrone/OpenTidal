# User state on disk

The signed-in user and their library survive a restart in one SQLite file.
A fresh install has no session. Sign-in writes the token to the token store and
the user row to the file. A restart shows the name before any network call.

The token store holds the secret and nothing else. The file holds no token, so a
copy of the file grants no access to Tidal.

## Key

The key is the Tidal user id. It comes from the `uid` claim of the access
token, through `auth::decode_claims(...).user_id()` in `src/auth.rs`. That
function already exists and already has a test.

```rust
let token = store.load()?;                 // token store
let uid = decode_claims(&token.access_token)?.user_id()?;
```

The key needs no hash and no network call. It does not change on refresh, so
a refresh never touches the file.

A hash of the access token does not work. The access token rotates on every
refresh, so the key would move and every refresh would have to move the user
row. A crash between the token write and the row move loses the user. The
hash also gives no security, because the token it hashes already sits in the
token store.

## Store

One file. It sits next to the existing `config.json`, so the app owns one
directory.

```
~/Library/Application Support/OpenTidal/opentidal.db
```

- Mode `0600`. The file holds a name and a listening history.
- `PRAGMA user_version` carries the schema version. Every migration bumps it.
- `PRAGMA foreign_keys = ON`, `journal_mode = WAL`, `synchronous = FULL`.
- Add the file to the Time Machine exclusion list. This is a tradeoff, not a
  free choice. The likes, saves, follows and playlists come back from Tidal once
  the sync exists, so a backup of them copies a privacy risk for no gain. Two
  tables do not come back: `download` and `history` are local, and the sync never
  writes them (see the sync section). Excluding the file means a disk loss takes
  them with it, and nothing restores them. The exclusion is kept because the file
  holds the name and the listening history together, and one flag cannot cover
  part of a file. Splitting the local tables into a second file would be the way
  to have both, and is not in this branch.

## Schema

`PRAGMA user_version` is 4.

```
user(id TEXT PRIMARY KEY, name TEXT NOT NULL, updated_at INTEGER NOT NULL)

artist(id TEXT PRIMARY KEY, name TEXT NOT NULL)
album(id TEXT PRIMARY KEY, title TEXT NOT NULL,
      artist_id TEXT NOT NULL REFERENCES artist,
      cover TEXT NOT NULL DEFAULT '')
track(id TEXT PRIMARY KEY, title TEXT NOT NULL,
      artist_id TEXT NOT NULL REFERENCES artist,
      album_id TEXT NOT NULL REFERENCES album, duration TEXT NOT NULL)

like(user_id TEXT NOT NULL REFERENCES user,
     track_id TEXT NOT NULL REFERENCES track,
     PRIMARY KEY (user_id, track_id))
save(user_id TEXT NOT NULL REFERENCES user,
     album_id TEXT NOT NULL REFERENCES album,
     PRIMARY KEY (user_id, album_id))
follow(user_id TEXT NOT NULL REFERENCES user,
       artist_id TEXT NOT NULL REFERENCES artist,
       PRIMARY KEY (user_id, artist_id))
download(user_id TEXT NOT NULL REFERENCES user,
         album_id TEXT NOT NULL REFERENCES album,
         PRIMARY KEY (user_id, album_id))

playlist(slot INTEGER NOT NULL, user_id TEXT NOT NULL REFERENCES user,
         name TEXT NOT NULL, PRIMARY KEY (user_id, slot))
playlist_track(user_id TEXT NOT NULL REFERENCES user, slot INTEGER NOT NULL,
               position INTEGER NOT NULL,
               track_id TEXT NOT NULL REFERENCES track,
               PRIMARY KEY (user_id, slot, position),
               FOREIGN KEY (user_id, slot) REFERENCES playlist(user_id, slot))

collection(idx INTEGER NOT NULL, user_id TEXT NOT NULL REFERENCES user,
           name TEXT NOT NULL, PRIMARY KEY (user_id, idx))
collection_playlist(user_id TEXT NOT NULL REFERENCES user, idx INTEGER NOT NULL,
                    position INTEGER NOT NULL, playlist_slot INTEGER NOT NULL,
                    PRIMARY KEY (user_id, idx, position),
                    FOREIGN KEY (user_id, idx) REFERENCES collection(user_id, idx))

history(user_id TEXT NOT NULL REFERENCES user, position INTEGER NOT NULL,
        track_id TEXT NOT NULL REFERENCES track,
        PRIMARY KEY (user_id, position))

catalogue_playlist(id TEXT PRIMARY KEY, name TEXT NOT NULL,
                   description TEXT NOT NULL, cover TEXT NOT NULL DEFAULT '')
catalogue_playlist_track(playlist_id TEXT NOT NULL REFERENCES catalogue_playlist,
                         position INTEGER NOT NULL,
                         track_id TEXT NOT NULL REFERENCES track,
                         PRIMARY KEY (playlist_id, position))
tidal_playlist(user_id TEXT NOT NULL REFERENCES user,
               playlist_id TEXT NOT NULL REFERENCES catalogue_playlist,
               PRIMARY KEY (user_id, playlist_id))
```

Notes:

- Every library table hangs off `user.id`. No table assumes one user.
- Playlist slots and collection indices are local to a user. A global slot
  key would prevent two users from each storing slot zero. Child keys include
  `user_id` for the same reason.
- The current model has no stable playlist id and no item timestamps.
  Slots persist as stored. Timestamp columns arrive with the Tidal sync.
- `history` orders by `position`, never by rowid. A rowid can be reused after
  a delete, and history order is user-visible.
- `track_id`, `album_id` and `artist_id` hold opaque Tidal ids as text. The
  `artist`, `album` and `track` tables hold the catalogue rows that the stored
  libraries name, and they are shared by every account. A foreign key means a
  library row cannot reach the disk before its catalogue row.
- A TEXT column turns an inserted number into text, but a blob stays a blob.
  Load drops a row whose id is not text and counts it, like any other unknown id.
- `catalogue_playlist` and `catalogue_playlist_track` hold the catalogue
  playlists the stored libraries name, shared by every account like the other
  catalogue tables. `tidal_playlist` is the per-account membership. The local
  slots keep the `playlist` table.
- `duration` is ISO 8601 text such as `PT3M26S`, the shape Tidal sends.
- `cover` is the artwork id from the `coverArt` relationship, or empty for none.
  A non-text or malformed cover reads as none and never costs its album or
  playlist. The cover image itself is not in the store. It lives in the cover
  cache, one directory per account under `$XDG_CACHE_HOME/opentidal/covers`.
- No `is_active` column. The token store decides who is active. A stored
  flag would be a second source of truth and could disagree after a crash.

### Migration from version 1

Version 1 stored integer positions into a catalogue that shipped with the app.
Those positions name placeholder content, not Tidal items, so no mapping exists.
The migration runs in one transaction. It drops `like`, `save`, `follow`,
`download`, `playlist_track` and `history`, and then creates the catalogue tables
and the six library tables from the same text a fresh file uses. `user`,
`playlist`, `collection` and `collection_playlist` hold no catalogue id, so the
name, the playlists in their slots, and the collections survive. A playlist
keeps its name and loses its placeholder tracks.

A failure at any statement rolls back to an intact version 1, which the next open
migrates again.

### Migration from version 2

Version 2 holds every row version 3 keeps. The migration adds the three catalogue
playlist tables in one transaction and changes nothing else.

### Migration from version 3

Version 3 lacks only the two `cover` columns. The migration adds them with
`ALTER TABLE` in one transaction, and every row keeps an empty cover until the
next sync names one. Every path to version 4 runs the same `ALTER TABLE` text,
a fresh file included, so a fresh and a migrated file hold the same definitions.

## Start

The start path runs in this order. It never blocks on the network.

1. Keychain has no token. Signed out. Do not open the file.
2. Token present, `user_id()` returns `None`. Signed out. Write the
   `signed-out` sentinel, as `KeychainStore::clear` already does. A token that
   names no account cannot be a session, so the app clears it rather than showing
   a signed-in state with no library behind it. The deterministic build never
   reaches this: `start_sign_in` signs it in directly.
3. `user_id()` found, user row found. Signed in at once, name from the row.
4. `user_id()` found, row missing. Signed in, name `You`, profile fetch fills
   the row. A missing row is not an error.
5. If `token_expired(&token)`, refresh. Refresh writes the token store only.
6. Fetch the profile and update `user.name` and `user.updated_at`.

The refresh in step 5 can fail. A `401` or `400` from the token endpoint means
the refresh token is dead. Then sign out and keep the rows. A network error
means the session is unknown. Stay signed in, retry on the next request, and
do not clear anything. The Account screen names the failed fetch beside the
name it could not refresh, and the next fetch that lands clears it.

The code follows this: `drain` ends the session only for `InvalidGrant`. Every
other error, `Http` and `Decode` included, keeps `SignedIn`, because the disk
already holds the name and the library and a sign-in button over a working
session would drop them. A fresh token that the API rejects anyway is reported
as a dead grant, so a session that cannot work ends instead of being retried
forever.

The retry is the next launch, not a background poll: nothing starts another
fetch during a session. So the Account screen's offline message stays until the
next start, which is the next thing that tries.

## Writes

- Sign-in: write the token store, then the `user` row. The code writes the token
  first, because the token is what makes a session real. A row without a token
  is inert, and a token without a row loads as a new account.
- Sign-out: write what is outstanding, then clear the token store, then drop the
  in-memory library and close the database. The rows stay on disk, so the same
  account finds them on its next sign-in. A kept library in memory would show one
  account's data while another is signed in.
  When the write fails, sign-out does not happen. The session stays `SignedIn`,
  the library stays in memory with its unwritten edits, and the Account screen
  says why. Clearing the token store there would drop the only copy of those edits.
  `sign_out_keeps_unwritten_edits` holds this. A session with no open library
  always signs out, because it has nowhere to write and nothing to lose.
- Refresh: token store only. No library write. This is the gain over a rotating key.
- Profile fetch: update `user.name`.
- Page: merge one page into one mirrored list in memory. The next frame writes
  the library through `Store::write`, so each page costs one write, like any edit.
  A page that changes only the catalogue, such as a Tidal playlist's tracks, is
  written too, because the catalogue revision moved.
- Library change: one comparison per frame, not a write per operation. The app
  keeps the library as last written, and `drain` writes when the two differ. This
  replaces the earlier "write through in the same operation", which would have
  needed a persistence call at each of the 22 screen actions. Three things make
  the per-frame write safe. `view` asks for one more frame while the library is
  dirty, because a window at rest draws none. The window's quit hooks flush. And
  a failed write stops the wake, so a full disk cannot spin the loop.

The only point where the two stores can disagree is sign-in, and a stale row
there does nothing. A start with a row and no token reads as signed out.

One instance owns a library. `Store::open` takes the write lock for the life of
the handle, so a second copy of the app is refused the database rather than
taking turns with the first. `write` replaces every row for an account, so
without it the second instance's playlist would vanish under the first
instance's next write, and both writes would report success. The second copy
says the library could not be opened, and the Account screen shows that.

## The mirror rule

While the app is read-only, the library is a mirror of Tidal, not a truth.
Every row came from Tidal. Tidal still holds it.

**The sync is the recovery.** A row dropped by `Store::load` comes back on the
next sync that lists it. Local data is the exception: a dropped download or
history entry has no server copy, which the paragraph below covers.

Both paths treat a bad leaf the same way. `Store::load`, the sync, and any
future reader drop the leaf that fails `Library::check` and log a count. The
container stays visible with the leaves it has. Once the sync exists it restores
what went; until then the drop is final, which is why it is logged. Only a file
that cannot be read at all is an error, and then the database
stays closed for writing, so a save cannot overwrite what the app could not
read.

Two tables hold data no server has: `download` and `history`. A drop there is
permanent, so the rule needs one more sentence. A write stores the catalogue
rows a library names, and the catalogue never forgets an id, so a drop happens
only when the file is damaged, which means the app no longer has the album or
the track. A downloaded album the app cannot name is not something the user can
play, and a history entry it cannot name is not something a screen can show. So
the row went with the content, and nothing the user can reach is lost.

The rule ends the day the app writes to Tidal. From then on a local-only row
exists, a drop loses data the server never had, and recovery needs a real
answer.

## Sync

A pass reads one remote list from its first page. Each page lands in the
library as it arrives, so cached rows show at once and fill in page by page.
The mirrored lists are likes, saves, follows, and Tidal playlists. A pass never
touches downloads, history, user playlists, collections, or the name.

A sync is a complete pass of each of the four mirrored lists. It starts from
the Sync with Tidal button on the Account screen. In live mode it also starts
once the profile fetch lands. In the live build, opening a list's route starts a
pass that reads the first page only. Reopening the route that shows does not
start another. A view that needs more rows than a pass read asks for the next
page.

The Liked Songs, Albums, Artists and Playlists screens, and a Tidal playlist's
tracks, draw the library's ordered lists in Tidal's order. They build only the
rows near the viewport. The rows in view ask for the members up to the last
visible row plus 20 more, so a scroll toward the end reads the next page before
the reader gets there. A line keyed `pass-status` under the list says whether a
page is loading, whether the pass stalled, or whether the list is read to its
end. A stalled pass reads again when a scroll asks for more rows than before, or
when the screen opens again. A pass that stalls with the last row already in
view therefore waits for the screen to open again.

Returning to a list read to its end reads its first page again behind the rows
already shown. Returning to a list whose pass stopped part way resumes that pass
and does not read the first page again.

A pass obeys five rules.

1. Apply each page as it lands. An earlier page adds and reorders members, but
   it never removes one, because a member it omits may sit on a later page. A
   page that fails, a repeated cursor, or the cap of 400 pages halts the pass,
   and every row read before the halt stays in memory and on disk.
2. Only the last page removes. A complete pass is the truth for its list, so a
   member Tidal no longer lists is removed when the last page lands, and not
   before. A halted pass removes nothing.
3. A page applied twice changes nothing. Reading the same pages again leaves
   the library, the catalogue, and the file as they were.
4. Apply only to the account that asked. A page carries the session and the
   user id it started under, and the passes belong to the open library. A page
   for an earlier session, a closed library, or another account is dropped.
5. Read one page at a time, and never while the profile fetch is pending. The
   page worker never refreshes the token. A page Tidal rejects starts one
   renewal through the profile fetch, and the pass resumes with the fresh
   token. A fresh token Tidal rejects too ends the session. A renewal that
   fails without ending the grant stalls every pass that waited on it, and a
   later scroll retries the pass.

A Tidal playlist is a catalogue playlist, not a user playlist. Its name and
tracks live in the catalogue, and the library holds only its id in
`tidal_playlists`. No edit operation takes a catalogue playlist id, so a local
edit cannot reach a Tidal playlist. User playlists keep their slots and stay
editable. Collections stay flat and local, and they hold user playlists only.

The `download` and `history` tables are local. A pass never writes them, so a
wrong pass cannot lose real work. A wrong removal from a mirrored membership
costs one sync, because the server returns the member again.

The cost of the page cap: a list longer than 400 pages is never read to its
end, so it keeps every row it read and removes none.

The offline window is the other cost. A broken leaf stays missing until the
first sync after the network returns. The local copy is as good as the last
successful read.

A catalogue playlist that leaves `tidal_playlists` stays in the catalogue for
the session, because the catalogue never forgets. A route to it still resolves,
so a sync needs no route repair. The next load does not bring it back, because
a write stores only the catalogue rows the library names.

The catalogue is shared by every account in one run, and so are the catalogue
tables on disk. The library names which Tidal playlists an account sees. The
Home shelf shows seed playlists only, and `forget_session` clears every route
to a Tidal playlist, so one account never sees another account's playlist names.

## Load and the invariant

`src/library.rs` playlist ids are slot indices into a fixed array, catalogue ids
are opaque Tidal strings, and `src/state.rs` proves that no route, playlist, or
saved item dangles after each operation.
Load is a new way into that state, so it must not bypass the proof.

The rules:

1. Persist `PlaylistId` as `playlist.slot`. The model has no `playlist.id`.
   Preserve empty slots so a collection still names the same playlist.
2. On load, restore each playlist to its exact slot. Restore collections in
   `idx` order and their playlist references in `position` order.
3. Do not derive `Serialize` on `Library`. Map rows into a fresh `Library`
   field by field. Validate every catalogue id before it enters the library.
4. Run `Library::check` on the loaded value, and assert it in a debug build. A
   row that would fail the check is dropped at the boundary, before it enters
   the library, so the loaded value always satisfies the invariant.
5. The caps in `library.rs` are real: `MAX_PLAYLISTS`, `HISTORY_CAP`,
   `PLAYLIST_TRACK_CAP`, `MAX_COLLECTIONS`, `COLLECTION_PLAYLIST_CAP`. Load
   keeps the first cap-many rows in `position` order and counts the rest as
   dropped. Keeping the first matches a mirror: the newest rows are the ones
   the last sync wrote first.
6. Read the library in one transaction so a concurrent save cannot mix two
   versions. Save replaces one user's rows in one transaction.

## Many accounts, one session

The schema allows many users. The UI signs in one account at a time. This is
deliberate: the `user_id` column costs nothing now and the door stays open.

- The token decides the active user. Never query without a `user_id`.
- Rows for another account stay on disk and are inert. They come back on the
  next sign-in with that account.
- Opening a different account drops edits the last write could not land, and logs
  it. Writing them under the new account would be the worse failure, and keeping
  the old library on screen would show one person's rows to another.
- Opening the account that is already open keeps them, because a re-sign-in with
  a refreshed token is the common path and nothing has to be detached.
- Settings > Account needs a `remove account data` action. A second person's
  listening history on this machine must be deletable.

## Evidence

Each row names its status. `test:` names a real test and `scripts/evidence-status.py`
fails when the name does not exist. `live:` names a check done by hand, and names a
test that also covers it as `name`(), with the parentheses. The parentheses matter:
without them a renamed test there would pass unchecked, and another pragma or column
name would be read as a test. `pending:` names work this branch does not build, so
nothing there is a test. A `live:` cell that starts with `not run` is open: it
names the test that covers the code path, and no hand check has happened.

| Claim | Check | Status |
| --- | --- | --- |
| A library survives a round trip | Random ops, write, load, equal, `check` clean | test: `random_walk_round_trips`, `operations_round_trip` |
| Load cannot break the invariant | The loaded value satisfies `Library::check` | test: `random_walk_round_trips`, `every_reachable_library_satisfies_check` |
| An unknown item costs only that item | Seed an unknown track id, assert the rest loads and a count is reported | test: `an_unknown_track_is_dropped_not_fatal`, `an_unknown_item_costs_only_that_item` |
| A container survives a dropped leaf | One bad track in a playlist, assert the name and good tracks stay | test: `a_container_survives_a_dropped_leaf` |
| Every catalogue table drops an unknown id | Break each table in turn, assert the drop and the invariant | test: `every_catalogue_table_drops_an_unknown_id` |
| A second account starts empty | Sign out of one account, open another, assert no rows carried over | test: `a_second_account_does_not_inherit_the_first` |
| A new account has a row at once | Open a new account, reopen the file, assert the row is there | test: `a_new_account_is_written_at_once` |
| A token with no account signs out | Store a token with no `uid`, start, assert signed out and cleared | test: `a_token_without_an_account_signs_out` |
| A signed-in account with no open library says so | Fail the open, assert the message and an unwritable flush | test: `an_account_that_cannot_write_says_so` |
| A network error keeps the session | Land an `Http` profile error, assert `SignedIn`, the disk name, and the stored token | test: `a_network_error_keeps_the_session_signed_in` |
| A rejected fresh token ends the session | Classify a post-refresh `Unauthorized` as a dead grant | test: `a_fresh_token_that_is_rejected_ends_the_session` |
| A scope is assigned nowhere but `src/auth.rs` | Write an assignment into a doc, a script and an untracked file | `python3 scripts/scope-check.py`, by hand: each fails, and a clean tree passes |
| A driven scene owns its stores | Run the stored scene over a non-memory store, assert it installs its own and a scratch database path | test: `the_stored_scene_owns_its_store_and_its_database` |
| A quit whose write fails reports the lost edits | Make every write fail, then quit and assert the report | test: `a_quit_that_cannot_write_reports_the_edits_as_lost` |
| An invalid index cannot hide a valid collection | Seed a negative index first with one allowed, assert the valid one loads | test: `an_invalid_collection_index_does_not_hide_a_valid_one` |
| An invalid slot cannot hide a valid playlist | Seed slots -1, 0, 1 with two allowed, assert both valid ones load | test: `an_invalid_slot_does_not_hide_a_valid_one` |
| One instance owns a library | Hold the writer lock, then take it from a second connection | test: `a_second_connection_is_refused_the_library` |
| A broken file is never overwritten | Write junk to the path, assert no library opens and the bytes stay | test: `a_broken_file_is_never_overwritten` |
| A restart shows the library | Write, reopen the file, assert the library and the name come back | test: `a_restart_reads_the_written_library`, `a_restart_shows_the_saved_library` |
| A quit writes what is outstanding | Change the library, quit, reopen, assert the change is there | test: `a_quit_writes_what_is_outstanding` |
| A dirty library asks for a frame | A change wakes the loop, a clean library does not | test: `only_a_dirty_library_asks_for_a_frame` |
| A dirty library with nothing to write it to asks for no frame | No session, or a failed write, wakes nothing | test: `a_dirty_library_with_no_session_asks_for_no_frame` |
| A dead refresh token closes the library | Push an `InvalidGrant` result, assert no library and no token | test: `a_dead_refresh_token_closes_the_library` |
| Sign-out leaves no route to a playlist that is gone | Open a playlist screen, sign out, assert the state invariant | test: `signing_out_leaves_no_route_to_a_gone_playlist` |
| A failed restore detaches the previous account | Fail account B's open, assert account A's library is closed | test: `a_failed_restore_detaches_the_previous_account` |
| A missing row is written without an edit | Clear the row, flush with no change, assert the row is back | test: `a_missing_row_is_written_without_an_edit` |
| Sign-out keeps unwritten edits | Fail the write, sign out, assert the library and the playlist stay | test: `sign_out_keeps_unwritten_edits` |
| The refusal clears once the write lands | Fail, refuse, allow writes, flush, assert no message | test: `the_refusal_clears_when_a_write_lands` |
| A clean library clears a stale failure | Refuse, revert the edit so nothing is owed, flush, assert no message | test: `a_clean_library_clears_a_stale_failure` |
| A refusal does not follow the next account | Refuse in one account, open another, assert no message | test: `a_refusal_does_not_follow_the_next_account` |
| Reopening the same account keeps unwritten edits | Fail the write, edit, open the same account, assert the edit stays | test: `reopening_the_same_account_keeps_unwritten_edits` |
| A failed write does not silence the next account | Fail one account's write, open another, assert the wake flag is clear | test: `a_failed_write_does_not_silence_the_next_account` |
| Playlist slots survive deletion | Delete slot zero, write and load, assert slot one and its reference stay | test: `slot_hole_survives` |
| A collection gap does not misplace a reference | Move a collection's index, assert the reference follows it | test: `a_collection_gap_does_not_misplace_a_reference` |
| Users do not share slots | Two populated libraries with the same slots, assert each loads unchanged | test: `users_are_isolated` |
| A failed write loses nothing | Abort a child insert with a trigger, assert the previous library loads | test: `a_failed_write_is_atomic` |
| A repeated write makes no duplicates | Write twice, assert one row per bit | test: `write_is_idempotent` |
| An email is not a display name | A profile whose `username` is an address asserts `You` | test: `an_email_is_not_used_as_a_name` |
| The key holds offline | `uid` read from the token store alone, no network, before any fetch | test: `the_user_id_comes_from_the_token`, `the_fake_token_names_no_user` |
| A landed profile fetch starts a sync in live mode only | Land a profile in live and in fake mode, assert only live syncs | test: `a_landed_profile_syncs_only_in_live_mode` |
| A live sign-in syncs the library | Sign in with a real account, assert Liked Songs and the From Tidal grid fill and the file holds the rows | live: not run. No token exists, so this stays open. The trigger is covered by `a_landed_profile_syncs_only_in_live_mode`() |
| Tidal honours the nested include | A live read of liked tracks reports no dropped item | pending: no token. A track whose album or artist is missing is dropped and counted |
| A live next link starts with the base path | A live read of a library longer than one page reaches its last page | pending: no token. The fixtures send `/v2/...`, and any other shape fails the read with a clear error |
| The owned playlist list pages like the others | A live read of an account with many owned playlists | pending: no token |
| The file is private | `stat -f '%Lp'` returns `600` | live: 2026-11-15, plus `a_restart_reads_the_written_library`() |
| Schema is sound | `PRAGMA integrity_check` is `ok` and no row has an orphan `user_id` | live: 2026-11-15, plus `replacement_removes_stale_rows`() |
| A like reaches the disk from a live session | Like a track, wait for idle, read the row | live: 2026-11-15 |
| Token is not in the file | `strings opentidal.db` shows no token | live: 2026-11-15, and the schema has no token column |
| The database is out of the backup | `xattr -p com.apple.metadata:com_apple_backup_excludeItem` shows `com.apple.backupd` | test: `the_database_is_excluded_from_backups`, `the_apps_own_directory_is_excluded_from_backups` |
| The excluded directory is the real one | The real config and database path functions feed the matcher | test: `the_default_database_directory_is_the_one_excluded` |
| No data loss at sign-in | Kill the process between the token write and the library write, and the reverse | pending: no kill harness. The token-first order makes a token without a row read as a new account, so the loss is a name, not the library |
| Start needs no network | Launch with the network off, name and playlist visible | pending: no test. Start reads the disk first, then fetches the profile |
| A library screen needs no network | Open Liked Songs offline, assert the disk rows paint | pending: no test |
| A refresh writes no rows | Count rows before and after a refresh, `user_version` unchanged | pending: no test. A refresh writes the token store only |
| A halted pass removes nothing | Fail one list on its first page and another on its second, assert every cached row stays in memory and on disk | test: `a_halted_pass_removes_nothing` |
| A sync lands on disk | Sync, then open the file in a new run, assert the like and the playlist name | test: `a_sync_lands_on_disk` |
| A sync for an earlier session is dropped | Start a sync, sign out and back in, drain, assert nothing landed | test: `a_sync_for_an_earlier_session_is_dropped` |
| A sync for a closed library is dropped | Start a sync, end the session on a dead token, drain, assert the library stays empty | test: `a_sync_for_a_closed_library_is_dropped` |
| An edit during a sync survives it | Edit while a page is in flight, assert the edit and the page both remain | test: `an_edit_during_a_sync_survives_it` |
| A dead token found by a sync ends the session | A page returns a dead grant, assert no library, no token, and the last edit on disk | test: `a_dead_refresh_token_during_sync_closes_the_library` |
| A failed page keeps the earlier pages | Serve an error as page 3, assert pages 1 and 2 stay in memory and on disk | test: `a_failed_page_keeps_the_earlier_pages` |
| A paged list is read to its last page | Two liked-track pages, assert all three members in order | test: `liked_tracks_follow_the_cursor_to_the_last_page` |
| The fixtures decode into a whole library | Read every fixture page, merge each, assert the counts, no drop, and the playlist tracks | test: `the_fixtures_decode_into_a_whole_library` |
| A repeated cursor halts the pass | Two pages that name each other, assert the stall and that more demand does not retry | test: `a_repeated_cursor_stalls_the_pass_for_good` |
| The page cap halts the pass | A list with no last page, assert the stall at the cap | test: `the_page_cap_stalls_the_pass` |
| The real client reads one page per call with the token | A loopback server serves the fixtures, assert one request per call and the bearer header | test: `http_pages_read_one_response_per_call` |
| The token never leaves the host | A `next` link to another host or outside the base, assert it is refused before a fetch | test: `a_next_link_to_another_host_is_refused` |
| A version 1 file keeps what the user wrote | Migrate a version 1 file, assert the name, playlists and collections stay and every id row is gone | test: `a_version_one_file_keeps_its_playlists_and_drops_its_catalogue_rows` |
| A migrated file has the fresh schema | Compare `sqlite_master` of a fresh and a migrated file | test: `a_fresh_and_a_migrated_schema_match` |
| A version 2 file keeps every row | Migrate a version 2 file with a row in every table, assert the whole library loads | test: `a_version_2_file_keeps_every_row` |
| A Tidal playlist survives a restart | Write a library that names a fetched playlist, load into the seed alone, assert the name and tracks | test: `a_tidal_playlist_survives_a_restart` |
| An unknown Tidal playlist cannot hide a valid one | Plant an unknown membership before a valid one, assert the valid one loads and the drop is counted | test: `an_unknown_tidal_playlist_does_not_hide_a_valid_one` |
| A failed migration leaves version 1 | A view named `history` fails the last drop, assert version 1 and the `like` rows | test: `a_failed_migration_leaves_version_one` |
| A non-text id costs only its row | Plant a blob id, assert the drop and the rest of the library | test: `a_non_text_id_is_dropped_not_fatal` |
| A write keeps the catalogue rows it names | Write a library that names a fetched track, load into the seed alone, assert the track comes back | test: `a_write_stores_the_catalogue_rows_it_names` |
| A library row needs its catalogue row | Insert a like with no track row, assert the foreign key refuses it | test: `a_library_row_needs_its_catalogue_row` |
| A route outside the catalogue cannot open | Open an album id from a larger catalogue, assert the stack is unchanged | test: `opening_an_unknown_album_is_refused` |
| A track outside the catalogue cannot play | Select a track id from a larger catalogue, assert the state is unchanged | test: `selecting_an_unknown_track_is_refused` |
| An invalid history row cannot hide a valid entry | Seed an unknown track then two distinct valid ones, assert both load | test: `an_unknown_history_row_does_not_hide_a_valid_one` |
| A merge never touches local data | Seed a download, a history entry, a user playlist and a collection, merge a partial and a complete page, assert all remain | test: `a_local_like_survives_a_partial_merge_and_only_a_complete_merge_removes_it` |
| One account never sees another's playlist names | Sync account A, open its playlist, sign out, open account B in this run and the next, assert Home and the stack hold no name of A's | test: `account_b_never_sees_account_a_playlist_names` |
| A page for another account is dropped | Hold a page, open another account in the same session, release, assert nothing landed in memory or on disk | test: `a_page_for_another_account_is_dropped` |
| A page waits for the profile fetch | Hold the profile fetch, ask for a sync, assert no page, then release and assert one page per mirrored list | test: `a_page_waits_for_the_profile_fetch` |
| Only a complete pass removes a row | Read page one of two, assert the cached row stays, then read to the end, assert it is gone | test: `only_a_complete_pass_removes_a_row` |
| A page applied twice changes nothing | Sync twice over the same pages, assert the library, the catalogue revision and the file are unchanged | test: `a_page_applied_twice_changes_nothing` |
| A rejected page forces one renewal | Reject the first token, assert one profile fetch and the pass resuming with the fresh token | test: `an_unauthorized_page_forces_one_refresh` |
| A renewed token rejected again ends the session | Reject every page token, assert one renewal, then no library and no token | test: `a_renewed_token_rejected_again_ends_the_session` |
| A failed renewal stalls its passes and a scroll retries them | Reject the first token and fail the refresh, assert the pass stalls and the session stays, then scroll and assert the fresh token reads the page | test: `a_failed_renewal_stalls_the_pass_until_a_scroll_retries` |
| A catalogue-only change reaches disk | Read a Tidal playlist's tracks, assert the library is unchanged and a new run loads the tracks | test: `a_catalogue_only_change_reaches_disk` |
| Entering a route starts one pass | Open Liked Songs twice, assert one page, then Albums, assert one more | test: `entering_a_route_starts_one_pass_and_reentering_does_not` |
| A larger view reads the next page | Enter a two-page list, assert one page, ask for two rows, assert both pages | test: `a_larger_want_reads_the_next_page` |
| A scroll reads the page its rows need | Open Albums in the paged scene, assert album 50 is absent, scroll, assert its row | test: `a_scroll_reads_the_page_its_rows_need`. The paged journey asserts the same row headless |
| Cached rows show before page one lands | Write a read library, restart with page one held, assert the rows and the loading line | test: `cached_rows_show_before_the_first_page_lands` |
| Reentry refreshes behind the rows | Read Albums to its end, leave, hold page one, return, assert the rows and the loading line | test: `reentering_a_cached_screen_shows_rows_before_the_refresh_lands` |
| A stalled pass says so and a larger scroll retries it | Fail the page a scroll asked for, assert the stall line, scroll further, assert the page lands | test: `a_stalled_pass_says_so_and_a_larger_scroll_retries_it` |
| A live library pages on scroll | Sign in with a large real account, open Albums, scroll, watch later rows arrive | pending: no live run in this unit |
| A merge keeps no unresolvable member | Merge a member the catalogue lacks, assert it is counted and left out | test: `an_unresolvable_member_is_counted_not_kept` |
| A merge cannot break the invariant | Three merge successors in the exhaustive library proof | test: `every_reachable_library_satisfies_check` |
| A bad cover never hides its album or playlist | Absorb rows with malformed covers, assert each row is kept with no cover and nothing is counted | test: `a_bad_cover_does_not_hide_its_album`, `a_bad_cover_does_not_hide_its_playlist` |
| A row without a cover keeps the known cover | Absorb a cover, then the same album and playlist with an empty and a malformed one | test: `a_row_without_a_cover_keeps_the_known_cover` |
| The decoder reads the cover id | Decode the synthetic fixtures, assert each album and playlist cover id | test: `the_fixtures_decode_into_a_whole_library` |
| A cover id survives a restart | Write an album and a playlist with covers, load into the seed alone, compare the entries whole | test: `a_cover_survives_a_restart`, `rows_for_carries_the_cover` |
| A damaged cover column costs only the cover | Plant a blob and a malformed cover, assert the library loads whole with no cover | test: `a_non_text_cover_keeps_its_album` |
| A version 3 file gains empty covers | Migrate a version 3 file, assert every row loads with no cover | test: `a_version_3_file_gains_empty_covers`, `a_fresh_and_a_migrated_schema_match` |
| The cover cache lives under the XDG cache root | Ask for the root with and without `XDG_CACHE_HOME`, relative and empty included | test: `the_cover_cache_lives_under_xdg_cache_home`, `without_xdg_cache_home_the_cover_cache_falls_back_to_dot_cache` |
| A put cover survives a restart | Put the fixture, rebuild the cache from the same root, assert the bytes | test: `a_put_cover_survives_a_restart` |
| A failed put leaves no partial cover | Block the rename with a directory, assert no cover and no temporary file | test: `a_failed_put_leaves_no_partial_cover` |
| Putting a cover twice changes nothing | Put the same bytes twice, compare the directory and the bytes | test: `putting_a_cover_twice_changes_nothing` |
| Bytes that are not an image are never put | Put HTML, empty and truncated bodies, assert no file | test: `bytes_that_are_not_an_image_are_never_put` |
| A damaged cover reads as a miss | Overwrite a cover with junk, assert a miss and the file removed | test: `a_damaged_cover_reads_as_a_miss`, `an_evicted_cover_is_a_miss` |
| A stray file cannot push out a valid cover | Plant stray names, prune to one, assert the valid cover stays and the strays are untouched | test: `an_invalid_cache_entry_does_not_count_against_the_limit` |
| A crashed replace leaves nothing behind | Plant a temporary file, sweep, assert only the cover remains | test: `a_leftover_temporary_file_is_swept` |
| One account cannot read another's covers | Put a cover for account A, assert a miss for B | test: `one_account_cannot_read_another_accounts_covers`, `a_user_id_cannot_leave_the_cover_root` |
| The cover cache is private | Assert mode 0600 on a cover and 0700 on its directory | test: `the_cover_cache_is_private` |
| A cover fetch reads the pinned relationship | Read the synthetic artwork fixture, assert the path, the artwork by id and the file | test: `a_cover_link_is_read_from_the_artwork_files`, `the_smallest_file_at_or_above_the_width_wins` |
| A cover link must be https | Serve http, relative, protocol-relative, file and ftp links, assert no image request | test: `a_cover_link_that_is_not_https_is_refused` |
| A cover with no file is an error | Serve artworks with no usable file and a missing artwork | test: `a_cover_with_no_files_is_a_decode_error` |
| The image request carries no token and a cap | A loopback server checks the header and serves an over-cap body | test: `an_image_body_over_the_cap_is_refused` |
| A miss fetches and a hit does not | Resolve twice over one root, count the fetches | test: `a_miss_fetches_and_puts_the_cover`, `a_hit_makes_no_fetch` |
| One failed cover costs only itself | Resolve a batch with an error, an HTML body and a good cover | test: `a_failed_cover_does_not_block_the_batch` |
| A rejected token stops the fetches | Reject the first cover, assert one fetch and the hits still served | test: `a_rejected_token_stops_further_fetches`, `no_token_still_serves_hits` |
| Only fetches count toward the batch cap | Plant a cap of hits and more damaged files, assert the cap counts fetches only | test: `only_fetches_count_toward_the_cap` |
| Every wanted cover arrives | Want more covers than one batch fetches, drain until idle, assert all shown | test: `every_wanted_cover_is_fetched_across_batches`, `a_cover_wanted_during_a_batch_is_fetched_next` |
| A cover batch for another account is dropped | Start a batch, open another account in the same session, assert nothing shown and no file in its directory | test: `a_cover_batch_from_another_account_is_dropped` |
| A cover batch waits for the profile fetch | Hold the profile fetch, want a cover, assert no batch, then release | test: `a_cover_batch_waits_for_the_profile_fetch` |
| Sign-out forgets the shown covers | Show a cover, sign out, assert no cover and an empty map on reopen | test: `sign_out_forgets_the_shown_covers` |
| A restart fetches no cover it has | Two apps on one root, assert the second fetches nothing | test: `a_cover_restart_fetches_nothing` |
| Setup does no cover work | Restore a session in setup, drain, assert no batch, no fetch and no cover root | test: `no_cover_work_runs_at_setup` |
| A put cover decodes through Rune | Put the fixture, decode it with `ImageCache::file`, assert its size | test: `a_ready_cover_decodes_to_its_pixels` |
| A decoded cover appears on screen | Drive `features/covers` twice with one database, assert `cover-image`, the magenta share inside its rect, and a second run with no fetch | `scripts/pixel-assert.py` on the `features/covers` drive, by hand on 2026-11-18 against the fake service: both runs pass. This is not a live check |
| A real account shows a real cover | Sign in with a real account, sync, open Albums, compare a cover with the Tidal app | live: not run. The response shape is confirmed live, but a full sync of this account did not finish in the drive window, so the screen stays open. The read is covered by `a_cover_link_is_read_from_the_artwork_files`() |
| A live next link is followed | Read a live collection page, follow its `links.next`, read the second page | live: 2026-11-18, plus `a_next_link_without_the_base_path_is_followed`() |
| A live cover reads an artwork file | Read a live album coverArt response, pick a file, fetch the image with no token | live: 2026-11-18, plus `a_cover_link_is_read_from_the_artwork_files`() and `the_smallest_file_at_or_above_the_width_wins`() |
| The live library needs cover includes | Read live collections with and without the coverArt include, compare the relationships | live: 2026-11-18. The include assertions are covered by `http_pages_read_one_response_per_call`() |
| Tidal limits rapid requests | Send ten rapid paced-free page requests, observe `429 Too Many Requests` | live: 2026-11-18, plus `a_rate_limited_page_call_is_retried_and_lands`() |

## Confirmed live

Read against a real account on 2026-11-15.

- `uid` was `173210380` on the first sign-in, after a sign-out and a fresh
  sign-in, and after a token refresh. The key holds across all three, so a
  refresh needs no row move.
- The access token lives about four hours. A doctored `exp` claim sends the app
  down the refresh path, so the refresh is testable without the wait.
- After sign-out the default token store has no `auth.toml`, and a start reads
  it as no token. The keychain option keeps its `signed-out` sentinel.
