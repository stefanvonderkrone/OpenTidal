# Accent

Accent changes the app's highlight colour, drawn on the selected nav item and the transport `play`
button. The `a` key cycles through six accents; the `accent-blue` and `accent-pink` scenes start on
a chosen accent. The observable change is the `play` button's fill colour.

## Sub-features

- `accent-default` starts on accent 0 (blue, fill `#4392f7ff`).
- `accent-step` advances one accent with the `a` key.
- `accent-pink` starts on accent 2 (pink, fill `#ee5b9aff`).

## How to get to it (user POV)

- Press the `a` key to cycle the accent.
- Launch with the `accent-pink` or `accent-blue` scene.

## Driving it with control-opentidal

Preconditions:

- Built once: `scripts/control-opentidal build`.
- `scripts/control-opentidal doctor` reports `ok:`.

- **Default accent.** The default scene starts blue. Run
  `scripts/control-opentidal drive scripts/verify/mark.json artifacts/accent/default --scene default`, then assert
  `scripts/tree-assert.py artifacts/accent/default/tree.json fill=play=#4392f7ff`.
- **Step with `a`.** Press `a` once from a fresh run and assert the colour moved off the default.
  Use `{"events":[{"t":0.1,"type":"key","name":"a"},{"t":0.5,"type":"mark","name":"end"}]}`, run
  `scripts/control-opentidal drive SCRIPT artifacts/accent/step`, then assert
  `scripts/tree-assert.py artifacts/accent/step/tree.json fill-not=play=#4392f7ff`.
- **Pink scene.** Drive `accent-pink` and assert the exact pink. Run
  `scripts/control-opentidal drive scripts/verify/mark.json artifacts/accent/pink --scene accent-pink`, then assert
  `scripts/tree-assert.py artifacts/accent/pink/tree.json fill=play=#ee5b9aff`.
- **Proof.** Save the `drive` artifacts and the `tree-assert.py` output under
  `artifacts/accent/<case>/`. The tree shows the `play` fill for each accent.

## Gotchas

- Colours are pixel-level. Assert the element `fill` value, not screenshot pixels or averages.
- `fill` strings carry an alpha byte (`#rrggbbaa`); compare the full 9-char value.
- The selected nav item and the play button share the accent; either is a valid probe, but use
  `play` because it is always accent-coloured.
