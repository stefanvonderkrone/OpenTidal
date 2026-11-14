# Library

Library is the shell's structure: a `TIDAL` brand, the grouped sidebar (primary, Your Library,
Downloads), a top bar with Search and Settings, the Home shelves of album cards, and the
now-playing bar with transport, progress and volume. This recipe checks element presence and a rendered frame.
Use the now-playing selector and sidebar collapse recipes to check their state changes.

## Sub-features

- `library-sidebar` renders the brand, the grouped sections, and the Downloads entry.
- `library-home` renders the Home shelves of album cards (`card-0` through `card-9`).
- `library-topbar` renders the top bar with Search and Settings entry points.
- `library-now-playing` renders the transport bar with the `play` button.

## How to get to it (user POV)

- Launch the app. The Home screen is the default view.

## Driving it with control-opentidal

Preconditions:

- Built once: `scripts/control-opentidal build`.
- `scripts/control-opentidal doctor` reports `ok:`.

- **Home renders.** Drive the default scene with no input and assert the pieces exist. Run
  `scripts/control-opentidal drive scripts/verify/mark.json artifacts/library/home --scene default`, then assert
  `scripts/tree-assert.py artifacts/library/home/tree.json has=logo has=nav-Home has=lib-header has=shelf-title has=card-0 has=card-9 has=top-bar has=search-go has=settings-go has=now-playing has=play has=vol-track`.
- **Frame is non-empty.** Confirm the render produced pixels. `file artifacts/library/home/frame.png`
  reports a `PNG image data, 2560 x 1600` (1280x800 at 2x); a zero-byte file means the render failed.
- **Proof.** Save the `drive` artifacts under `artifacts/library/home/`. The tree lists the
  keyed elements and the PNG shows the drawn window.

## Gotchas

- The album cards are keyed `card-<i>`, not `album-<i>` as in the old prototype.
- The old `pl-Chill` playlist keys are gone; playlists are a sidebar section (`nav-Playlists`).
- The card keys are shared with the detail drill: `card-0` is both a Home card and the tap target
  that opens `Route::Album(0)`.
