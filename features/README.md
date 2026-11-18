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
- `album-<pos>`, `artist-<pos>`, and `seed-playlist-<pos>` name a catalogue card on a screen.
- `playlist-<slot>` names a user playlist card on the Playlists screen.
- `tidal-playlist-<pos>` names a Tidal playlist card in the From Tidal grid on the Playlists screen.
- `cover` is a card's placeholder gradient, and `cover-image` is its cover once the image decoded with no error. A card has one or the other. The hero uses `hero-cover` and `hero-cover-image` the same way.
- `sync-library` is the Sync with Tidal button on the Account screen, and `sync-status` is the line under it.
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
| `tidal-sync` | A sync shows a Tidal playlist that opens read-only. |
| `tidal-sync-local` | After a sync, a local playlist still opens with its edit actions. |
| `tidal-sync-status` | The Account screen reports what the sync left. |
| `covers` | A synced album shows its fetched cover, and a second run shows it from disk with no fetch. |

The `sign-in` journey runs against the deterministic fake service, which signs
in at once, so no browser or network is needed headless. The `signed-in` scene
starts already signed in for a quick look.

Rename (playlist, collection, and profile) uses a text field, which the
headless driver does not type into. A unit test covers the operations
(`library::tests::rename_changes_the_name`).

The three `tidal-sync` journeys start from the `signed-in-synced` scene, whose
fake service reads one liked track and one Tidal playlist. Nothing syncs until
the journey taps `sync-library`. Point `OPENTIDAL_DB` at a scratch file, then
drive and assert each one:

```sh
scripts/control-opentidal drive features/tidal-sync/script.json artifacts/tidal-sync --scene signed-in-synced
scripts/tree-assert.py artifacts/tidal-sync/tree.json "text=hero-title=Evening Drive" absent=rename-box absent=delete-playlist absent=add

scripts/control-opentidal drive features/tidal-sync-local/script.json artifacts/tidal-sync-local --scene signed-in-synced
scripts/tree-assert.py artifacts/tidal-sync-local/tree.json "text=hero-title=Playlist 1" has=rename-box has=delete-playlist has=add

scripts/control-opentidal drive features/tidal-sync-status/script.json artifacts/tidal-sync-status --scene signed-in-synced
scripts/tree-assert.py artifacts/tidal-sync-status/tree.json "text=sync-status=Synced 1 track, 0 albums, 0 artists and 1 playlist."
```

The `absent` predicates alone cannot catch a Tidal card that opens nothing, so
the title assertion is the one that fails when the card is wired to a user
playlist slot.

The `covers` journey starts from the `signed-in-covers` scene. Its fake service
saves one album whose cover is the solid magenta PNG in
`tests/fixtures/covers/magenta.png`. The cover cache sits beside the scene's
database, so both runs below must share one `OPENTIDAL_DB`. The first run
fetches the cover. The second run reads it from disk, and its log says the batch
fetched 0 covers. `scripts/pixel-assert.py` reads the snap inside the
`cover-image` rect, so a placeholder or a missing key fails it:

```sh
export OPENTIDAL_DB="$(mktemp -d)/scene.db"
scripts/control-opentidal drive features/covers/script.json artifacts/covers --scene signed-in-covers
scripts/tree-assert.py artifacts/covers/tree.json has=cover-image absent=cover
scripts/pixel-assert.py artifacts/covers/frame.png artifacts/covers/tree.json cover-image ff00ff 0.9
grep "fetched 1 of 1 covers" artifacts/covers/run.log

scripts/control-opentidal drive features/covers/script.json artifacts/covers-hit --scene signed-in-covers
scripts/pixel-assert.py artifacts/covers-hit/frame.png artifacts/covers-hit/tree.json cover-image ff00ff 0.9
grep "fetched 0 of 1 covers" artifacts/covers-hit/run.log
```
