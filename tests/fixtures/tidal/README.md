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
