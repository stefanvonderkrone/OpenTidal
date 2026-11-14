# Navigate

Navigate moves between the app's screens. The sidebar selects one of thirteen sections (Home,
Feed, Popular, For You, then the "Your Library" sections and Downloads), each with its own content
pane. Cards drill into a detail screen, and a Back button in the top bar returns. The selected
sidebar item is `checked` and the content pane title names the current section.

## Sub-features

- `nav-default` opens on Home, checked, with no Back button.
- `nav-select` chooses a section by click and swaps the content pane to it.
- `nav-drill` opens a detail screen from a card, showing a Back button.
- `nav-back` returns from a detail to the previous section.

## How to get to it (user POV)

- Click a sidebar item, for example `Albums`.
- Click a card, for example the first album card.
- Click `Back` in the top bar while on a detail screen.

## Driving it with control-opentidal

Preconditions:

- Built once: `scripts/control-opentidal build`.
- `scripts/control-opentidal doctor` reports `ok:`.

- **Open on Home.** Drive the default scene with no input.
  Run `scripts/control-opentidal drive scripts/verify/mark.json artifacts/navigate/home --scene default`. Assert
  `scripts/tree-assert.py artifacts/navigate/home/tree.json checked=nav-Home unchecked=nav-Albums text=title=Home`.
- **Choose a section.** Tap `nav-Albums`. Use
  `{"events":[{"t":0.1,"type":"tap","key":"nav-Albums"},{"t":0.5,"type":"mark","name":"end"}]}`, run
  `scripts/control-opentidal drive SCRIPT artifacts/navigate/albums`, then assert
  `scripts/tree-assert.py artifacts/navigate/albums/tree.json checked=nav-Albums unchecked=nav-Home text=title=Albums`.
- **Drill into a detail.** From a fresh run, tap the first card. Use
  `{"events":[{"t":0.1,"type":"tap","key":"card-0"},{"t":0.5,"type":"mark","name":"end"}]}`, run
  `scripts/control-opentidal drive SCRIPT artifacts/navigate/drill`, then assert
  `scripts/tree-assert.py artifacts/navigate/drill/tree.json has=back has=hero-title text=hero-title=Currents`.
- **Go back.** Tap `back` after drilling. Use
  `{"events":[{"t":0.1,"type":"tap","key":"card-0"},{"t":0.5,"type":"tap","key":"back"},{"t":0.9,"type":"mark","name":"end"}]}`,
  run `scripts/control-opentidal drive SCRIPT artifacts/navigate/back`, then assert
  `scripts/tree-assert.py artifacts/navigate/back/tree.json has=shelf-title` and that `back` is gone
  (the `has` predicate has no negative form; assert the Home title instead via `text=title=Home`).
- **Proof.** Save the `drive` artifacts and the `tree-assert.py` output under
  `artifacts/navigate/<case>/`.

## Gotchas

- Sidebar keys carry spaces for multi-word labels: `nav-For You`, `nav-Liked Songs`,
  `nav-On this device`. Quote them exactly.
- The Back button exists only at detail depth; at Home the top bar has Search and Settings only.
- The content pane title is per-section now, so assert it changes with selection, unlike the old
  prototype whose pane was hard-coded to Home.
- Each case starts from a fresh run; a script that drills and then taps Back proves the round trip
  in one run.
