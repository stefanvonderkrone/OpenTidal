# Glossary

OpenTidal's domain terms. Use these names everywhere: in code, commits, and
docs. One name per thing.

## Catalogue

The immutable content the app ships with. It never changes, so it sits outside
the state space the invariant proof explores.

- **artist** — a performer. An album and a track each name one artist.
- **album** — a release by one artist. A track names one album.
- **track** — one song. A track names one album and one artist.
- **editorial playlist** — a read-only, curated playlist in the catalogue. The
  user cannot change it.

## Library

The mutable data of a Tidal account. It starts empty and changes only through
the operations in `src/library.rs`. The app signs in one account at a time, and
the database keeps a row set per account id.

- **like** — save a track. Liked tracks appear on the Liked Songs screen.
- **save** — save an album. Saved albums appear on the Albums screen.
- **follow** — follow an artist. Followed artists appear on the Artists screen.
- **download** — take an album offline. Downloaded albums appear on the
  Downloads screen; their tracks appear on the On this device screen.
- **user playlist** — a playlist the user owns. The user can create, rename,
  delete, reorder, and add or remove tracks.
- **collection** — a folder of user playlists. The user can create, rename,
  delete, and add or remove playlists.
- **history** — the recently played tracks, most recent first.

## Store

The on-disk half, in `src/store.rs`. One SQLite file, one row set per account.

- **store**: the SQLite database at
  `~/Library/Application Support/OpenTidal/opentidal.db`.
- **tokens**: the token store. The default is the token file at
  `$XDG_DATA_HOME/opentidal/auth.toml`, with `~/.local/share` as the fallback.
  `OPENTIDAL_TOKENS=keychain` or `"tokens": "keychain"` in the config selects the macOS keychain.
  The store holds the token and nothing else.
- **slot**: a position in the fixed playlist array. A slot is empty after a
  delete, and a create reuses the first empty one. Routes are never persisted,
  so a slot means the same playlist across a restart.
- **write**: replace one account's whole library in one transaction. This is
  `Store::write` in code. It never means the album verb **save**, and an edit the
  write has not landed yet is **unwritten**.

## State

The proven shell state in `src/state.rs`.

- **route** — one screen in the navigation stack.
- **section** — a top-level sidebar or top-bar entry.
- **detail** — a screen drilled from a section (an album, an artist, or a
  playlist).

## Invariant

The rule the proof checks after every operation: no route, playlist, history,
or saved item references an id that does not exist.

## Tidal

The remote half, behind the `MusicService` port in `src/tidal.rs`. Read-only
today; the catalogue and library stay local until the later PRs move them.

- **service** — the `MusicService` port. The app reaches Tidal only through it.
  The deterministic fake backs every proof; the real client backs
  `OPENTIDAL_LIVE=1`.
- **sign in** — start the OAuth authorization-code flow with PKCE. The live
  build opens the browser and a loopback listener; the deterministic build
  signs in at once.
- **session** — the signed-in state on `App`, not `State`. Its variants are
  `SignedOut`, `Authorizing`, `SignedIn`, and `Failed`, from `src/auth.rs`.
- **token** — an OAuth access token and its refresh token. The refresh token
  persists in the token file or the macOS keychain.
- **user id**: Tidal's account id, from the `uid` claim of the access token.
  It is the database key. It does not change on a refresh or a fresh sign-in.
