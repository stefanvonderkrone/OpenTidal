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
refresh token persists in the macOS keychain.

## PR sequence

1. `feat/tidal-boundary` — the `MusicService` boundary, the `User` and `Token`
   types, PKCE auth with keychain persistence, and the signed-in name in the
   sidebar and Account screen. Fake-backed headless proof.
2. `feat/tidal-catalogue` — fetch real artists, albums, tracks, and playlists,
   replace the placeholder `catalog.rs` with a store keyed by opaque string ids.
3. `feat/tidal-library` — fetch liked tracks, saved albums, followed artists,
   and user playlists from `/userCollection*` relationships into the library.
4. `feat/tidal-search` — search the live catalogue, with debounce.

Each later PR depends on the one before.

## Confirmed live

Read these against a real account on 2026-11-15.

- The media type is `application/vnd.api+json`. `application/vnd.tidal.v1+json`
  404s on the collection endpoints.
- `/users/me` 404s for third-party clients. The user id comes from the access
  token's JWT `sub` claim; the name from the token or `/users/{id}`.
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
