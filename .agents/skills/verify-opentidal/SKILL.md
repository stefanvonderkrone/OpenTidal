---
name: verify-opentidal
description: Headless verification of OpenTidal, the Rust/Rune Tidal-style desktop music app. Use to prove navigation, play/pause, accent cycling and library rendering, before or after changing src/main.rs.
---

# Verify OpenTidal

OpenTidal is a native desktop music app (wgpu/winit, built on the vendored `rune`). It has no
network, no persistence and no data directory: every run starts from the same in-memory state.
It verifies headless — the app's own tooling renders offscreen and replays synthetic input, so
no display or GPU window is needed. Drive it with `scripts/control-opentidal` (build/doctor/drive),
then assert with `scripts/tree-assert.py`.

Read `features/README.md` first, then the feature file for the journey you are proving.

## Launch

- Build once: `scripts/control-opentidal build` (runs `cargo build` at the repo root; first build
  compiles rune and takes minutes, later builds are incremental).
- Open the window by hand, if you want to look: `cargo run`. Verification itself never opens a
  window; `drive` runs headless and exits when the script ends.
- There is no server, port, seed data or data directory to reach. "Ready" means the binary is
  built and `doctor` passes.

## Doctor

One read-only check that answers "is this instance worth driving?":

```sh
scripts/control-opentidal doctor
```

Expect exactly: `ok: window main, scene default, title OpenTidal, viewport 1280x800`.

It checks the binary exists, the window key is `main`, the `default` scene exists, and a fresh
`--tree` dump reports the title `OpenTidal`, a 1280x800 viewport, and a `play` element. Run it
whenever a drive fails for an unclear reason.

## Drive

The harness is the app's own headless mode. `drive` replays a `--script` JSON, then writes four
artifacts into `OUT_DIR`:

- `script.json` — a copy of the script (the action).
- `tree.json` — the `--tree` element + accessibility dump at the script's end (the resulting state).
- `frame.png` — a `--snap` render of that same moment (visual proof).
- `run.log` — the rune perf line and any errors.

```sh
scripts/control-opentidal drive SCRIPT.json OUT_DIR [--scene NAME]
```

A script is `{"events":[...]}`. Each event is `{"t": seconds, "type": ..., ...}`. The events used
here:

- `{"t":0.1,"type":"tap","key":"nav-Explore"}` taps an element's center by its `key`.
- `{"t":0.2,"type":"key","name":"a"}` sends a text key; `"name":" "` is space.
- `{"t":0.5,"type":"mark","name":"end"}` marks the end time for the dump.

The app's own `--list-scenes` names the scenes (`default`, `accent-blue`, `accent-pink`, `paused`);
`--scene` starts from one of them. A `tap` on an unknown key exits 2 with
`script: no element with key "..."`, so a typo'd selector fails loudly.

Assert the resulting state with `scripts/tree-assert.py`:

```sh
scripts/tree-assert.py OUT_DIR/tree.json checked=nav-Explore unchecked=nav-Home label=play=Pause
```

Predicates: `title=`, `viewport=`, `has=`, `label=KEY=`, `label-has=KEY=`, `checked=`,
`unchecked=`, `fill=KEY=`, `fill-not=KEY=`. It prints `PASS`/`FAIL` per predicate and exits
non-zero on any `FAIL`. Use stable element `key`s from the app (see `features/`), never pixel
coordinates; the `--tree` `elements` array lists every keyed element.

## Evidence

Capture the action and the resulting state, not just the final screen:

- Keep the four `drive` artifacts (`script.json`, `tree.json`, `frame.png`, `run.log`) together in
  `artifacts/<feature>/<case>/`.
- Run the matching `tree-assert.py` and save its output as `artifacts/<feature>/<case>/assert.txt`.
- Screenshots alone are weak: a `frame.png` proves the pixels, but the `tree.json` assertions
  prove the state. Pair them.
- The grid and transport are drawn but static in this build; their proof is `has=` element
  presence plus a non-empty `frame.png`, not a state change.

## Cleanup

Headless runs are short-lived and self-terminating: they exit when the script finishes, so there
is no process to kill. Do not kill by process name. If a run hangs, kill the PID of the
`control-opentidal drive` you started. `doctor` removes its own temp dir. Remove scratch OUT_DIRs
you created, but never remove `artifacts/` — evidence must survive cleanup.

## Helpers

- `scripts/control-opentidal` — build/doctor/drive. Invocation shown above.
- `scripts/tree-assert.py` — predicate assertions on a `--tree` dump. Invocation shown above.
