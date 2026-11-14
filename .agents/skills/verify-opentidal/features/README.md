# OpenTidal verification map

This directory is the maintained source for verifying OpenTidal's user-facing behavior. Read this
index before driving the app, then use the matching feature file as the recipe.

## Baseline preconditions

- Built once: `scripts/control-opentidal build`.
- Run `scripts/control-opentidal doctor` and require its `ok:` line before every drive.
- Each `drive` writes only to its own `OUT_DIR`; there is no shared state, port or data directory,
  so runs are safe to parallelise.
- A `tap` on an unknown key exits 2; a bad selector fails loudly, never silently skips.

## Driving conventions

- Start every recipe from the baseline state (a fresh run) unless its preconditions say otherwise.
- Drive through `scripts/control-opentidal drive SCRIPT.json OUT_DIR [--scene NAME]`.
- Assert through `scripts/tree-assert.py OUT_DIR/tree.json PREDICATE...`.
- A no-input baseline case uses `scripts/verify/mark.json`
  (`{"events":[{"t":0.0,"type":"mark","name":"end"}]}`), which dumps the initial state.
- Use element `key`s from the app, never pixel coordinates. The `--tree` `elements` array lists
  every keyed element; `--list-scenes` and `--list-windows` list the scenes and window key.
- Treat every command as literal. Keep key names and flags unchanged.

## Proof and skip reporting

- Capture the user action and the resulting state, not only the final screen.
- Keep the four `drive` artifacts (`script.json`, `tree.json`, `frame.png`, `run.log`) plus the
  `assert.txt` output in `artifacts/<feature>/<case>/`.
- Pair a `frame.png` with `tree.json` assertions; the tree proves state, the PNG proves pixels.
- Record the feature ID and entry point used with every artifact.
- Report an unreachable path with the attempted command and the unmet precondition.
- Do not report a skipped entry point as verified through a different path.

## Feature entry contract

Each feature file starts with an H1 title and one paragraph describing the user-visible behavior,
then exactly four H2 sections in this order:

1. `Sub-features` lists short IDs with one line for each behavior.
2. `How to get to it (user POV)` lists every user entry point.
3. `Driving it with control-opentidal` starts with `Preconditions:` and uses labeled bullets that
   pair each user action with an exact command and observable result.
4. `Gotchas` lists traps that can waste or invalidate a verification run.

Keep implementation details out of the map. Name only user paths, stable keys, required state,
commands, and observable proof.

## Features

- [Navigate](./navigate.md) — sidebar selection, card drill into details, and back.
- [Play / pause](./play-pause.md) — the transport play button and the space key toggle state.
- [Accent](./accent.md) — the `a` key cycles the accent colour.
- [Library](./library.md) — the shell structure: sidebar, top bar, Home shelves, now-playing.
- [Now-playing selector](./now-playing.md) selects a track from a track list.
- [Search filters](./search.md) filters albums, artists, and tracks as you type.
- [Settings subpages](./settings.md) shows option rows for each settings page.
- [Sidebar collapse](./collapse.md) hides and restores sidebar navigation.
