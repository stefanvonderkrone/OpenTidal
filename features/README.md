# Feature map

Each journey below proves one user-management operation headless. A journey is
a `script.json` you drive with `scripts/control-opentidal drive`, then assert
with `scripts/tree-assert.py`. The four drive artifacts and the assert output
live under `artifacts/<journey>/`.

Stable element keys the journeys use:

- `nav-<name>` — a sidebar section (e.g. `nav-Playlists`, `nav-Albums`).
- `album-<id>` / `artist-<id>` — a catalogue card on a discovery screen.
- `playlist-<slot>` — a user playlist card on the Playlists screen.
- `like` — the heart on the now-playing bar (label `Like` → `Unlike`).
- `save` / `follow` / `download` — the toggle buttons on a detail hero.
- `new-playlist`, `add`, `rename-playlist`, `delete-playlist` — playlist actions.
- `rm-<slot>-<pos>`, `up-<slot>-<pos>`, `down-<slot>-<pos>` — per-track playlist actions.
- `new-collection`, `add-to-col-<idx>`, `del-col-<idx>`, `rm-from-col-<idx>-<pos>`.
- `dl-<id>`, `rm-dl-<id>` — download rows on the Downloads screen.
- `track-<id>` — a track row (also `track-<id>` in a playlist, with the actions).

The journeys:

| Journey | Proves |
| --- | --- |
| `like` | The heart toggles and the track lands in Liked Songs. |
| `save-album` | Saving an album puts it in the Albums screen. |
| `follow-artist` | Following an artist puts it in the Artists screen. |
| `download` | Downloading an album puts it in the Downloads screen. |
| `playlist-create` | New playlist → open → add track. |
| `playlist-delete` | Delete returns to Playlists and leaves no dangling route. |
| `collection` | A collection holds a playlist and removes it. |

Rename (playlist, collection, and profile) uses a text field, which the
headless driver does not type into; the operations are proven by the unit and
invariant tests instead (`library::tests`, `state::tests`).
