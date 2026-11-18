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
the album and artist resources it names, because the sync asks for
`include=items,items.albums,items.artists`. `owned-playlists.json` answers
`/playlists?filter[owners.id]=<uid>`. It lists one playlist that is also
favorited, so a test can prove that the read fetches its items once.
`owned-playlist-items.json` holds the items of the other one.
