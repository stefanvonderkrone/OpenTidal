# Tidal API integration plan

OpenTidal gains one real Tidal user and one real catalogue. The placeholder
catalogue and the in-memory library stay until the later PRs replace them. The
work lands as four PRs, each verified before the next.

Rule the program enforces: the proven core stays pure and deterministic. Network
and tokens never enter `State`, so `State` stays `Eq` and `Hash` and the
invariant proof stays bounded.

## How to read this

One box is one unit of work. Each box names the evidence that checks it. The
body is a how-to. The appendices explain and record.

Tests alone are not sufficient verification. A PR is verified only when its
unit, live, and perf boxes are all checked.

## Seam

The boundary is a `MusicService` trait. Two impls exist. `FakeService` is
deterministic and backs every proof and headless drive. `HttpService` is a thin
blocking `reqwest` client. The app holds the service as `Arc<dyn MusicService>`
on `App`, not `State`.

Rune supplies the async model. `Latest<T>` runs a blocking closure off the UI
thread and wakes `AppSpec::poll`. `setup` attaches the waker. No tokio.

Auth is a state machine on `App`. `SignedOut`, `Authorizing`, `SignedIn`. The
OAuth flow is authorization code with PKCE (S256) and a loopback redirect. The
refresh token persists in `auth.toml` by default. The macOS keychain stays as an option.

## PR sequence

1. `feat/tidal-boundary` adds the `MusicService` boundary, the `User` and `Token`
   types, PKCE auth with token persistence, and the signed-in name in the
   sidebar and Account screen. A fake-backed headless proof backs it.
2. `feat/tidal-catalogue` fetches real artists, albums, tracks, and playlists.
   It replaces the placeholder `catalog.rs` with a store keyed by opaque strings.
3. `feat/tidal-library` fetches liked tracks, saved albums, followed artists,
   and user playlists from `/userCollection*` relationships into the library.
4. `feat/tidal-search` searches the live catalogue with a debounce.

Each later PR depends on the one before.

## Decisions for the user library

The user library and the real catalogue land on one branch in five milestones.
Each milestone passes the preflight before the next one starts.

1. Opaque ids and the catalogue store replace the placeholder catalogue. The
   migration drops the old rows because their ids do not exist on Tidal. The
   proof bounds are rebuilt with a small fake catalogue and string ids.
2. The library sync reads saved albums, followed artists, liked tracks,
   favorited playlists, owned playlists, and playlists inside those resources.
   The sync is read-only. A local edit on a remote playlist is blocked. Locally
   created playlists stay editable. The sync keeps the local history and the
   local downloads, and it writes only after every page arrives.
3. The cover cache lives under `XDG_CACHE_HOME`. A cache miss starts a fetch.
   The fake tests the miss path. No image work runs on the startup path or the
   UI thread.
4. Track credits arrive through `include=credits` on a track request. The album
   page shows the track list with the credits of each track. There is no album
   credit object.
5. The artist page shows albums, EPs and singles, top songs sorted by popularity
   in the app, and similar artists. Compilations and appears-on stay unproven
   until a live check against a real account.

Nested folders are dropped. The public API cannot derive them. The folder
endpoints need `r_usr`, which this app cannot request. The deprecated folder
view does not carry a parent field in its schema. Local collections stay flat.

Playback history stays local. Public v2 has no third-party playback history.
The local history stays empty until playback exists.

`scripts/tidal-api-check.py` runs in the preflight. It fails when an endpoint in
the used list is not `THIRD_PARTY`, or when an endpoint in the forbidden list
stops being `INTERNAL`. The endpoint list lives in
`docs/tidal-api-access.json`. The fixtures in `tests/fixtures/tidal/` are
synthetic. Each one must be replaced with a sanitized real response before it
is used as live evidence.

## Confirmed live

Read these against a real account on 2026-11-15.

- The media type is `application/vnd.api+json`. `application/vnd.tidal.v1+json`
  404s on the collection endpoints.
- `/users/me` returns the profile for third-party clients; `/users/{id}`
  returns nothing. The user id comes from the access token's JWT `uid` claim
  (a number, not the OIDC string `sub`). The name comes from `/users/me`; the
  token carries no name claim, and the profile has `username` (and optionally
  `firstName`/`lastName`) but not always a display name.
- Liked tracks: `GET /userCollectionTracks/me/relationships/items?include=items`.
  `data` is the membership (track `id` + `addedAt`), `included` is the track
  resources, and `links.next` carries the `page[cursor]` for the next page.
- The same `/userCollectionAlbums|Artists|Playlists/me/relationships/items`
  shape holds the rest of the library.
- Track titles live in `included[].attributes.title`; duration is ISO 8601
  (`PT3M26S`). Artist and album names need `include=artists,albums`.

## Throughput checkpoint

Blocking first steps. The boundary, types, and auth machine must exist before
any fetch work. They gate everything else.

Independent workstreams. The boundary, the auth machine, and the Account-screen
wiring are separate files that do not share writes. They can go in any order
after the types land.

Shared mutable state. The one shared field is `App.auth`, owned by the auth
machine. `App.service` is immutable after init. `State` is untouched. No
serialization is needed beyond the single-threaded `poll` drain.

Smallest safe decomposition. One owner builds PR 1. The three follow-up PRs each
get their own owner after their parent lands.
