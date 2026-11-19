# Glossary

OpenTidal's domain terms. Use these names everywhere: in code, commits, and
docs. One name per thing.

## Catalogue

The content the app can show, in `src/catalogue.rs`. `App` owns the one
catalogue, outside `State`, so it sits outside the state space the invariant
proof explores. It starts from the seed and only grows. A load absorbs the
catalogue rows the store holds, and no operation removes an entry.

- **id.** An opaque Tidal string, such as `77610756`. Only a `Catalogue` mints
  an id, and only for a member. An id is never a position.
- **seed.** The built-in catalogue, with ids such as `seed-track-0`. The
  `seed-` prefix cannot collide with a numeric Tidal id.
- **absorb.** Merge unvalidated catalogue rows into the catalogue. It is the
  only writer, and it drops a row whose parent it does not hold.

- **artist.** A performer. An album and a track each name one artist.
- **album.** A release by one artist. A track names one album.
- **track.** One song. A track names one album and one artist.
- **catalogue playlist.** A read-only playlist in the catalogue, with a name, a
  description, and ordered tracks. The user cannot change it. In code it is
  `CataloguePlaylist`, opened through `Route::CataloguePlaylist`. The seed holds
  three, with ids such as `seed-playlist-0`. Every other one is a Tidal
  playlist.
- **Tidal playlist.** A catalogue playlist the account owns or favorites on
  Tidal. The library holds its id in `tidal_playlists`, and the catalogue holds
  its content. It is never a user playlist and never takes a slot.
- **artwork id.** The id in an album's or a catalogue playlist's `coverArt`
  relationship. In code it is `ArtworkId`. Only a `Catalogue` mints one, with
  the same shape rule as every other id, so it is also a safe file name. No
  read names an artist's artwork, so an artist's cover is keyed `artist-<id>`.
- **cover.** The image of an album, a catalogue playlist, or a Tidal artist.
  An artist's cover is its `profileArt`. A seed artist and a user playlist
  have no cover, and keep the **placeholder**.
- **placeholder.** The gradient a screen draws where no cover is ready.

## Library

The mutable data of a Tidal account. It starts empty and changes only through
the operations in `src/library.rs`. The app signs in one account at a time, and
the database keeps a row set per account id.

- **like.** Save a track. Liked tracks appear on the Liked Songs screen.
- **save.** Save an album. Saved albums appear on the Albums screen.
- **follow.** Follow an artist. Followed artists appear on the Artists screen.
- **download.** Take an album offline. Downloaded albums appear on the
  Downloads screen. Their tracks appear on the On this device screen.
- **user playlist.** A local playlist in a slot. The user can create, rename,
  delete, reorder, and add or remove tracks. A sync never touches one.
- **collection.** A folder of user playlists. The user can create, rename,
  delete, and add or remove playlists.
- **history.** The recently played tracks, most recent first.

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
- **cover cache**: one account's directory of cover files, at
  `$XDG_CACHE_HOME/opentidal/covers/<user id>/`, with `~/.cache` as the
  fallback. In code it is `CoverCache`. It is never the **store** and never the
  **tokens**.
- **put**: replace one cover file through `atomic::replace`. This is
  `CoverCache::put`. It never means **write** (the library) or **save** (an
  album).
- **write**: replace one account's whole library in one transaction. This is
  `Store::write` in code. It never means the album verb **save**, and an edit the
  write has not landed yet is **unwritten**.

## State

The proven shell state in `src/state.rs`.

- **route.** One screen in the navigation stack.
- **section.** A top-level sidebar or top-bar entry.
- **detail.** A screen drilled from a section. It shows an album, an artist, or
  a playlist.

## Invariant

The rule the proof checks after every operation: no route, playlist, history,
or saved item references an id that does not exist.

## Tidal

The remote half, behind the `MusicService` port in `src/tidal.rs`. Read-only.
Passes mirror the library from Tidal. Nothing writes back.

- **service.** The `MusicService` port. The app reaches Tidal only through it.
  The deterministic fake backs every proof. The real client backs
  `OPENTIDAL_LIVE=1`.
- **sign in.** Start the OAuth authorization-code flow with PKCE. The live
  build opens the browser and a loopback listener. The deterministic build
  signs in at once.
- **session.** The signed-in state on `App`, not `State`. Its variants are
  `SignedOut`, `Authorizing`, `SignedIn`, and `Failed`, from `src/auth.rs`.
- **token.** An OAuth access token and its refresh token. The refresh token
  persists in the token file or the macOS keychain.
- **members.** The ordered ids of one mirrored list, in Tidal's order and
  without duplicates. In code it is `Members<T>`. Likes, saves, follows, and
  Tidal playlists are members in `Library`, and a Tidal playlist's tracks are
  members in its catalogue playlist.
- **pass.** One read of one remote list from its first page. Each page merges
  into the members as it lands. Only the last page removes a member, so a pass
  that halts removes nothing. In code it is a `Pass` inside `Passes`, which
  belong to the open library. A remote list is `RemoteList`, never a
  **collection**.
- **sync.** A complete pass of each of the four mirrored lists: likes, saves,
  follows, and Tidal playlists. `App::sync` starts it, and `Library::merge`
  applies each page.
- **cover state.** Where one cover is in its life: wanted by a screen, held
  by a batch, shown, or unavailable until the next page lands. In code it is
  `CoverState`, one map per open library. It is not a **slot**.
- **ticket.** The session and user id a worker started under. In code it is
  `Ticket`, checked by `App::ticket_holds`. A result is applied only when both
  still match the open library.
- **user id**: Tidal's account id, from the `uid` claim of the access token.
  It is the database key. It does not change on a refresh or a fresh sign-in.
