# Navigate

Navigate selects one of the six sidebar sections (Home, Explore, My Collection, Radio, My Mixes,
Favorites). The selected item is highlighted and its accessibility state is `checked`; the others
are `unchecked`. Selection is the user-visible change: in this build the content pane title is
static, so only the sidebar highlight moves.

## Sub-features

- `nav-default` opens on Home, the first item, checked.
- `nav-select` chooses an item by click and moves the checked state to it.
- `nav-radio` selects a non-adjacent item (Radio) to show selection is not index-adjacent.

## How to get to it (user POV)

- Click a sidebar item: `Home`, `Explore`, `My Collection`, `Radio`, `My Mixes`, or `Favorites`.
- The sidebar is the left column; each item is a button.

## Driving it with control-opentidal

Preconditions:

- Built once: `scripts/control-opentidal build`.
- `scripts/control-opentidal doctor` reports `ok:`.

- **Open on Home.** Drive the default scene with no input.
  Run `scripts/control-opentidal drive scripts/verify/mark.json artifacts/navigate/home --scene default`. Assert
  `scripts/tree-assert.py artifacts/navigate/home/tree.json checked=nav-Home unchecked=nav-Explore`.
- **Choose Explore.** Tap `nav-Explore` and assert the state moves.
  Write a script
  `{"events":[{"t":0.1,"type":"tap","key":"nav-Explore"},{"t":0.5,"type":"mark","name":"end"}]}`,
  run `scripts/control-opentidal drive SCRIPT artifacts/navigate/explore`, then assert
  `scripts/tree-assert.py artifacts/navigate/explore/tree.json checked=nav-Explore unchecked=nav-Home`.
- **Choose Radio.** Tap `nav-Radio` from a fresh run and assert only Radio is checked.
  Use `{"events":[{"t":0.1,"type":"tap","key":"nav-Radio"},{"t":0.5,"type":"mark","name":"end"}]}`,
  run `scripts/control-opentidal drive SCRIPT artifacts/navigate/radio`, then assert
  `scripts/tree-assert.py artifacts/navigate/radio/tree.json checked=nav-Radio unchecked=nav-Explore unchecked=nav-Home`.
- **Proof.** Save the `drive` artifacts and the `tree-assert.py` output under
  `artifacts/navigate/<case>/`. The tree shows the tapped item checked and the others unchecked.

## Gotchas

- The content pane title always reads `Home`; do not assert it changes with selection.
- The global space key toggles play/pause, it does not activate the focused nav item; drive
  selection by `tap`, not by key.
- `nav-select` must start from a fresh run; tapping two items in one script leaves the last one
  checked, so a per-case script taps exactly one item.
