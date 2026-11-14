# Library

Library is the Home screen: a `TIDAL` logo, the six-item nav sidebar, a four-item playlist list,
a grid of ten album cards (cover, title, artist), and the now-playing bar with transport, progress
and volume. It is drawn but static in this build, so the proof is element presence plus a rendered
frame, not a state change.

## Sub-features

- `library-grid` renders ten album cards (`album-0` through `album-9`).
- `library-sidebar` renders the nav and playlist columns.
- `library-now-playing` renders the transport bar with the `play` button.

## How to get to it (user POV)

- Launch the app. The Home screen is the default view.

## Driving it with control-opentidal

Preconditions:

- Built once: `scripts/control-opentidal build`.
- `scripts/control-opentidal doctor` reports `ok:`.

- **Home renders.** Drive the default scene with no input and assert the pieces exist. Run
  `scripts/control-opentidal drive scripts/verify/mark.json artifacts/library/home --scene default`, then assert
  `scripts/tree-assert.py artifacts/library/home/tree.json has=logo has=nav-Home has=pl-Chill has=album-0 has=album-9 has=now-playing has=play has=vol-track`.
- **Frame is non-empty.** Confirm the render produced pixels. `file artifacts/library/home/frame.png`
  reports a `PNG image data, 2560 x 1600` (1280x800 at 2x); a zero-byte file means the render failed.
- **Proof.** Save the `drive` artifacts under `artifacts/library/home/`. The tree lists the
  keyed elements and the PNG shows the drawn window.

## Gotchas

- The album cards and playlists are not interactive; there is no press handler to prove. Do not
  assert a state change for them.
- `--tree` lists only keyed elements in `elements`; the grid's repeated `title`/`artist`/`cover`
  keys are per-card and show as `album-<i>` children, so assert the `album-<i>` keys.
- The `frame.png` is 2560x1600 by default (scale 2); pass `--scale 1` only if you also need the
  1280x800 image, but the default is what the feature's assertion expects.
