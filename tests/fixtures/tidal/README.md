# Tidal fixtures

These documents are synthetic. I wrote them from the pinned OpenAPI spec,
version 1.10.14. No account token was available when I wrote them, so no real
response is recorded here.

A synthetic fixture proves that decoding works. It does not prove that Tidal
sends the shape. Replace each file with a sanitized real response before it is
used as evidence for a live box. Keep the same filenames so the decoder tests do
not change.

Each file follows the JSON:API document shape. Cursor pagination uses `links.next`.
Track credits arrive through `include=credits` on a track request. The separate
credits endpoints need internal access, so no fixture uses them.

Each track carries `albums` and `artists` relationships, and each page includes
the album and artist resources it names, because the read asks for
`include=items,items.albums,items.artists,items.albums.coverArt,items.albums.artists`.
Each included album carries its own `artists` relationship. Without
`items.albums.artists` the live service sends an album with no artist, and the
catalogue drops it and every track on it. `owned-playlists.json` answers
`/playlists?filter[owners.id]=<uid>`. It lists one playlist that is also
favorited, so a test can prove that the read fetches its items once.
`owned-playlist-items.json` holds the items of the other one.

`album-cover-art.json` answers
`/albums/<id>/relationships/coverArt?include=coverArt`. Its shape is a guess
from the pinned albums spec, and nothing real backs it. The resource type
`artworks`, the `files` array, and each file's `href` and `meta.width` are all
unproven. The decoder matches the included artwork by id only, so it works
with either type name. It holds a second artwork with another id, so a test
can prove that the read picks the artwork the relationship names.

`album-items.json` answers
`/albums/<id>/relationships/items?include=items,items.artists,items.albums,items.albums.coverArt,items.albums.artists`,
and `artist-albums.json` answers
`/artists/<id>/relationships/albums?include=albums,albums.artists,albums.coverArt`.
A live read on 2026-11-19 returned the same resource types for both: tracks in
`data` for the album, albums in `data` for the artist, and each included album
with `relationships.artists.data`. The attribute values here are still
synthetic.
