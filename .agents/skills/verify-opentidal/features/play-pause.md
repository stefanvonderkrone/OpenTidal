# Play / pause

Play / pause toggles playback state. The transport `play` button is labelled `Pause` while playing
(its icon is a pause glyph) and `Play` while paused. The space key toggles the same state from
anywhere. No audio actually plays in this build; the observable change is the button label.

## Sub-features

- `play-toggle` flips state by clicking the `play` button.
- `space-toggle` flips state with the space key.
- `paused-scene` starts paused via the `paused` scene.

## How to get to it (user POV)

- Click the round `play`/`pause` button in the now-playing bar at the bottom.
- Press the space bar.
- Launch with the `paused` scene.

## Driving it with control-opentidal

Preconditions:

- Built once: `scripts/control-opentidal build`.
- `scripts/control-opentidal doctor` reports `ok:`.

- **Playing at launch.** The default scene starts playing. Run
  `scripts/control-opentidal drive scripts/verify/mark.json artifacts/play-pause/playing --scene default` and assert
  `scripts/tree-assert.py artifacts/play-pause/playing/tree.json label=play=Pause`.
- **Toggle by button.** Tap `play`. Use
  `{"events":[{"t":0.1,"type":"tap","key":"play"},{"t":0.5,"type":"mark","name":"end"}]}`, run
  `scripts/control-opentidal drive SCRIPT artifacts/play-pause/button`, then assert
  `scripts/tree-assert.py artifacts/play-pause/button/tree.json label=play=Play`.
- **Toggle by space.** Press space from a fresh run. Use
  `{"events":[{"t":0.1,"type":"key","name":" "},{"t":0.5,"type":"mark","name":"end"}]}`, run
  `scripts/control-opentidal drive SCRIPT artifacts/play-pause/space`, then assert
  `scripts/tree-assert.py artifacts/play-pause/space/tree.json label=play=Play`.
- **Start paused.** Drive the `paused` scene and assert it opens paused. Run
  `scripts/control-opentidal drive scripts/verify/mark.json artifacts/play-pause/paused-scene --scene paused`, then assert
  `scripts/tree-assert.py artifacts/play-pause/paused-scene/tree.json label=play=Play`.
- **Proof.** Save the `drive` artifacts and the `tree-assert.py` output under
  `artifacts/play-pause/<case>/`. The tree shows the `play` button label for each state.

## Gotchas

- The button label is the source of truth: `Pause` means playing, `Play` means paused. The icon
  glyph is the inverse of the label; do not read the icon.
- Space toggles globally, even when a nav item is focused; it never types.
- The `paused` scene only changes the play state; the accent stays default blue.
