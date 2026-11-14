# Search filters

Search filters albums, artists, and tracks as you type. A blank query shows the album browse grid.

## Sub-features

- `search-filter` matches titles and artist names without case sensitivity.
- `search-empty` shows a message when no catalog item matches.
- `search-browse` restores the album grid when the field contains only whitespace or no text.

## How to get to it (user POV)

- Click Search in the top bar. Click the search field and type a query.
- Clear the field to restore the browse grid.

## Driving it with control-opentidal

Preconditions:

- Run `scripts/control-opentidal build` and `scripts/control-opentidal doctor`.

- Filter the catalog. Save this script as `SCRIPT.json`:
  `{"events":[{"t":0.1,"type":"tap","key":"search-go"},{"t":0.3,"type":"tap","key":"search-box"},{"t":0.5,"type":"key","name":"d"},{"t":0.6,"type":"key","name":"a"},{"t":0.7,"type":"key","name":"f"},{"t":0.8,"type":"key","name":"t"},{"t":1.0,"type":"mark","name":"end"}]}`.
  Run `scripts/control-opentidal drive SCRIPT.json artifacts/search/filter`.
  Run `scripts/tree-assert.py artifacts/search/filter/tree.json text=search-box=daft has=card-3 has=track-4 absent=card-0`.
  Save the assertion output as `artifacts/search/filter/assert.txt`.
- Check no matches. Replace the query key events with `z`, `z`, and `z`.
  Run `scripts/control-opentidal drive SCRIPT.json artifacts/search/empty`.
  Run `scripts/tree-assert.py artifacts/search/empty/tree.json 'text=search-empty=No results for "zzz"' absent=card-0`.
- Restore browse. Add three `Backspace` key events after the no-match query and before the mark.
  Run `scripts/control-opentidal drive SCRIPT.json artifacts/search/browse`.
  Run `scripts/tree-assert.py artifacts/search/browse/tree.json text=search-box= has=card-0 absent=search-empty`.

## Gotchas

- Result keys keep the original catalog index, not the filtered position.
- The artist catalog has no Daft Punk card. The `daft` query matches an album and a track.
- The field must have focus before the key events.
