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
2. The library reads liked tracks, saved albums, followed artists, favorited
   playlists, owned playlists, and the tracks of a Tidal playlist. It reads one
   page at a time and applies each page as it lands. The read is read-only. A
   local edit on a remote playlist is blocked. Locally created playlists stay
   editable. A read never touches the local history or the local downloads.
3. The cover cache lives under `XDG_CACHE_HOME`. A cache miss starts a fetch.
   The fake tests the miss path. No image work runs on the startup path or the
   UI thread.
4. Track credits arrive through `include=credits` on a track request. The album
   page shows the track list with the credits of each track. There is no album
   credit object.
5. The artist page shows the artist's albums, EPs and singles from
   `/artists/{id}/relationships/albums`, the tracks by that artist that the
   catalogue already holds, and the artist's `profileArt`. The artist tracks
   endpoint answered `400` live, so there are no top songs. Similar artists,
   compilations and appears-on are not built.

A pass reads one remote list from its first page, one page per call. Each
remote list has its own pass and its own endpoints:

- Liked tracks:
  `GET /userCollectionTracks/me/relationships/items?include=items,items.albums,items.artists,items.albums.coverArt,items.albums.artists`
- Saved albums:
  `GET /userCollectionAlbums/me/relationships/items?include=items,items.artists,items.coverArt`
- Followed artists: `GET /userCollectionArtists/me/relationships/items?include=items`
- Playlists: the favorited playlists from
  `GET /userCollectionPlaylists/me/relationships/items?include=items,items.coverArt`,
  then the owned playlists from `GET /playlists?filter%5Bowners.id%5D=<uid>`
- The tracks of one Tidal playlist:
  `GET /playlists/<id>/relationships/items?include=items,items.albums,items.artists,items.albums.coverArt,items.albums.artists`
- The tracks of one Tidal album:
  `GET /albums/<id>/relationships/items?include=items,items.artists,items.albums,items.albums.coverArt,items.albums.artists`
- The albums of one Tidal artist:
  `GET /artists/<id>/relationships/albums?include=albums,albums.artists,albums.coverArt`

An album's tracks and an artist's albums own no list in the library. Their
pages fill the catalogue, and the album and artist screens draw what the
catalogue holds for that album or artist.

Artist pictures come from one batched read per 20 artists:
`GET /artists?filter%5Bid%5D=<id>%2C<id>...&include=profileArt`. Each artist
in `data` names its artwork in `relationships.profileArt.data[0].id`, and the
artwork in `included` carries `files[].href` and `meta.width`, the shape an
album cover has. The read takes at most 20 ids, because 50 ids answered `400`.
The answers come back in request order, one per artist. An artist with an
empty `profileArt`, an id Tidal does not know, or an artwork missing from
`included` has no picture. That is not an error, so it is not logged. The cover
is `Unavailable`, so the app asks again only after the next page lands, not in
the next batch. A read that fails fails every artist of its chunk
with the same error, so a rate limit defers them all.

An artist picture is cached under the key `artist-<id>`, not under its artwork
id, because only the batched read names the artwork. A cached picture is
therefore never fetched again, and a picture the artist changes on Tidal stays
stale until its cache file is removed.

The request pacer spaces only the JSON:API reads on `openapi.tidal.com`. Image
downloads from `resources.tidal.com` go out with no pacing, because that host
answered 30 rapid requests with no `429`.

The live reads below cover these endpoints and the nested include strings.

A sync reads the first four lists to their last page. A Tidal playlist's
tracks, an album's tracks, and an artist's albums are read only when the user
opens that playlist, album or artist. A seed item has no read. In the live build, opening a
list's screen starts a pass that reads the first page, and a scroll that needs
more rows reads the next page. The list on screen reads first.

A page follows `links.next` to the next page. Each pass reads at most 400
pages. A repeated cursor, the 400-page cap, a failed page, or a `next` link that
is not a path on the service host stalls the pass, so the bearer token never
leaves the Tidal host. A stalled pass keeps every page it applied. After a
failed page, a scroll that asks for more rows reads the same page again. Opening
the screen again starts a new pass from the first page.

A page before the last one merges as partial. It adds and reorders members, but
it never removes one, because a member it omits may sit on a later page. Only a
complete pass, whose last page landed, can remove a member that Tidal no longer
lists. A member the catalogue cannot hold is dropped and counted.

A page can name an album artist that it does not include. The catalogue keeps
that album and its tracks, and it holds the artist under the name
"Unknown artist". A later page that includes the artist renames it in place. A
row with a malformed id is still dropped.

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

### Confirmed live on 2026-11-18

Read these with read-only requests against the signed-in account.

- A `links.next` value is an absolute path from the host root, such as
  `/userCollectionAlbums/me/relationships/items?include=...&page%5Bcursor%5D=...`.
  It has no `/v2` prefix. The pager accepts that shape and the older `/v2`
  shape.
- Nested includes work. Saved albums answer to `include=items,items.artists,items.coverArt`.
  Liked tracks and playlist items answer to
  `include=items,items.albums,items.artists,items.albums.coverArt`. Favorited
  playlists answer to `include=items,items.coverArt`.
- An album or playlist resource carries `relationships.coverArt.data` only when
  the request includes the cover. Without that include the relationship is
  absent.
- `GET /albums/{id}/relationships/coverArt?include=coverArt` returns an
  `included` resource of type `artworks`. Its `attributes.files` entries carry
  `href` and `meta.width`. This matches the synthetic fixture shape.
- A cover `href` serves `image/jpeg` with no `Authorization` header. The cover
  request sends no token to the image host.
- `GET /playlists?filter[owners.id]=<uid>` returned twelve owned playlists and
  no `next` link.
- Tidal answered `429 Too Many Requests` on the tenth rapid unpaced page
  request. The saved-albums cursor chain passed twenty five paced pages with no
  end.

### Confirmed live on 2026-11-19

Read these with read-only requests against the signed-in account, before the
change that uses them. No live run of the app followed.

- Liked tracks asked for with `include=items,items.albums,items.artists,items.albums.coverArt`
  return each included album with an empty artist. The catalogue dropped 18
  albums and 20 tracks on every page, and the log said
  `a page named 58 item(s) the catalogue cannot hold`. Adding
  `items.albums.artists` returned 18 albums, 21 artists, 18 artworks and 20
  tracks, and each album carried `relationships.artists.data`.
- `GET /albums/{id}/relationships/items` with
  `include=items,items.artists,items.albums,items.albums.coverArt,items.albums.artists`
  returned 20 tracks in `data`, with their album, artist and artwork included.
- `GET /artists/{id}/relationships/albums?include=albums,albums.artists,albums.coverArt`
  returned 20 albums in `data`, with 19 artists and 20 artworks included.
- `GET /artists/{id}/relationships/tracks` answered `400` with every include
  tried.
- `GET /artists/{id}/relationships/profileArt?include=profileArt` returned one
  artwork with `files[].href` and `meta.width`, the shape of an album cover.
  The app no longer uses this read. The batched read below replaced it.
- `GET /artists?filter%5Bid%5D=<20 ids>&include=profileArt` returned `200`
  with 20 artists in `data`, 20 artworks in `included`, and no `links.next`.
  The same read with 50 ids and with 100 ids answered `400`.
- The batched read with 7 ids, one of them dead, returned 6 artists and 6
  artworks. The dead id was absent, with no error.
- 30 rapid unpaced GETs to `resources.tidal.com` all answered `200`, in 1.39 s
  in total.

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
