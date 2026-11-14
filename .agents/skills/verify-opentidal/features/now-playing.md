# Now-playing selector

Click a track row to change the title and artist in the now-playing bar. The selected row has a highlight.

## Sub-features

- `now-playing-select` changes the now-playing title and artist and checks the selected row.
- `now-playing-lists` shows the same selection in Liked Songs, On this device, History, details, and Popular Tracks.

## How to get to it (user POV)

- Open Liked Songs, On this device, or History in the sidebar.
- Open an album, artist, or playlist card to reach its track list.
- Open Popular and click Tracks.
- Search for a track and click its result.

## Driving it with control-opentidal

Preconditions:

- Run `scripts/control-opentidal build` and `scripts/control-opentidal doctor`.

- Select a track. Save this script as `SCRIPT.json`:
  `{"events":[{"t":0.1,"type":"tap","key":"nav-Liked Songs"},{"t":0.4,"type":"tap","key":"track-3"},{"t":0.7,"type":"mark","name":"end"}]}`.
  Run `scripts/control-opentidal drive SCRIPT.json artifacts/now-playing/select`.
  Run `scripts/tree-assert.py artifacts/now-playing/select/tree.json 'text=track=Do I Wanna Know?' 'text=artist=Arctic Monkeys' checked=track-3`.
  Save the assertion output as `artifacts/now-playing/select/assert.txt`.
- Check another list. Open one of the entry points above after selection and assert `checked=track-3`.
  Use `tab-Tracks` for the Popular Tracks tab. Save each entry point in its own evidence directory.

## Gotchas

- Track keys use the original catalog index. `track-3` is "Do I Wanna Know?".
- Search results select tracks but do not show the selected-row highlight.
- Selection does not start audio playback.
