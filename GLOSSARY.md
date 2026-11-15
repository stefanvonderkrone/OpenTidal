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

The single local user's mutable data. It starts empty and changes only through
the operations in `src/library.rs`.

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
  persists in the macOS keychain.
