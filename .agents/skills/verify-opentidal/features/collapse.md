# Sidebar collapse

The top-bar toggle switches the sidebar between the full navigation and a compact logo.

## Sub-features

- `collapse-expanded` starts with the full sidebar.
- `collapse-toggle` hides the navigation groups.
- `collapse-restore` restores the navigation groups after a second click.

## How to get to it (user POV)

- Click Collapse sidebar in the top bar.
- Click Expand sidebar to restore the navigation.

## Driving it with control-opentidal

Preconditions:

- Run `scripts/control-opentidal build` and `scripts/control-opentidal doctor`.

- Check the full sidebar. Run `scripts/control-opentidal drive scripts/verify/mark.json artifacts/collapse/expanded`.
  Run `scripts/tree-assert.py artifacts/collapse/expanded/tree.json 'label=toggle-sidebar=Collapse sidebar' has=nav-Home`.
  Save the assertion output as `artifacts/collapse/expanded/assert.txt`.
- Collapse the sidebar. Save this script as `SCRIPT.json`:
  `{"events":[{"t":0.1,"type":"tap","key":"toggle-sidebar"},{"t":0.5,"type":"mark","name":"end"}]}`.
  Run `scripts/control-opentidal drive SCRIPT.json artifacts/collapse/collapsed`.
  Run `scripts/tree-assert.py artifacts/collapse/collapsed/tree.json 'label=toggle-sidebar=Expand sidebar' absent=nav-Home absent=lib-header`.
  Save the assertion output as `artifacts/collapse/collapsed/assert.txt`.
- Restore the sidebar. Add a second `toggle-sidebar` tap at `t=0.3` before the mark.
  Run `scripts/control-opentidal drive SCRIPT.json artifacts/collapse/restored`.
  Run `scripts/tree-assert.py artifacts/collapse/restored/tree.json 'label=toggle-sidebar=Collapse sidebar' has=nav-Home has=lib-header`.

## Gotchas

- Navigation keys do not exist while the sidebar is collapsed.
- The toggle stays in the top bar. The compact sidebar shows only `T`.
- Collapse keeps the current screen and track selection.
