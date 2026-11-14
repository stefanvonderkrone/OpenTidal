# Settings subpages

Each settings subpage shows labeled option rows and secondary values. The rows do not change settings.

## Sub-features

- `settings-subpage` opens Playback and shows its option rows.
- `settings-pages` covers Quality, Downloads, Notifications, Import, and About.

## How to get to it (user POV)

- Click Settings in the top bar. Click a page in the settings index.
- Click Back to return to the settings index.

## Driving it with control-opentidal

Preconditions:

- Run `scripts/control-opentidal build` and `scripts/control-opentidal doctor`.

- Open Playback. Save this script as `SCRIPT.json`:
  `{"events":[{"t":0.1,"type":"tap","key":"settings-go"},{"t":0.4,"type":"tap","key":"set-0"},{"t":0.7,"type":"mark","name":"end"}]}`.
  Run `scripts/control-opentidal drive SCRIPT.json artifacts/settings/subpage`.
  Run `scripts/tree-assert.py artifacts/settings/subpage/tree.json text=title=Playback has=opt-0`.
  Save the assertion output as `artifacts/settings/subpage/assert.txt`.
- Check another page. Replace `set-0` with `set-1` through `set-5` in that script.
  Run `scripts/control-opentidal drive SCRIPT.json artifacts/settings/PAGE` with a distinct page name.
  Run `scripts/tree-assert.py artifacts/settings/PAGE/tree.json text=title=TITLE has=opt-0`.
  Use the page title Quality, Downloads, Notifications, Import, or About, in that order.

## Gotchas

- Option keys start again at `opt-0` on each page.
- The values are placeholders. Import rows do not contact external services.
- About shows version `0.1.0`.
