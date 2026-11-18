# Feature map

Each journey below proves one user-management operation headless. A journey is
a `script.json` you drive with `scripts/control-opentidal drive`, then assert
with `scripts/tree-assert.py`. The four drive artifacts and the assert output
live under `artifacts/<journey>/`.

Stable element keys the journeys use. A catalogue key ends in the item's
position on the screen, counted from zero, not in its id. Tidal ids are opaque
strings, so `album-0` is the first album card on the current screen, whatever
the catalogue holds. The seed keeps today's order, so `album-0` on Home is
*Currents*.

- `nav-<name>` names a sidebar section, such as `nav-Playlists` or `nav-Albums`.
- `album-<pos>`, `artist-<pos>`, and `editorial-<pos>` name a catalogue card on a screen.
- `playlist-<slot>` names a user playlist card on the Playlists screen.
- `like` is the heart on the now-playing bar. Its label changes from `Like` to `Unlike`.
- `save`, `follow`, and `download` are the toggle buttons on a detail hero.
- `new-playlist`, `add`, `rename-playlist`, and `delete-playlist` are playlist actions.
- `rm-<slot>-<pos>`, `up-<slot>-<pos>`, and `down-<slot>-<pos>` are per-track playlist actions.
- `new-collection`, `add-to-col-<idx>`, `del-col-<idx>`, and `rm-from-col-<idx>-<pos>` are collection actions.
- `dl-<pos>` and `rm-dl-<pos>` name download rows on the Downloads screen.
- `track-<pos>` names a track row in a list, including a playlist row with its actions.

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
| `sign-in` | Sign in and the sidebar shows the signed-in user's name. |

The `sign-in` journey runs against the deterministic fake service, which signs
in at once, so no browser or network is needed headless. The `signed-in` scene
starts already signed in for a quick look.

Rename (playlist, collection, and profile) uses a text field, which the
headless driver does not type into. A unit test covers the operations
(`library::tests::rename_changes_the_name`).
