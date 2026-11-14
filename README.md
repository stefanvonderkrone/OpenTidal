# OpenTidal

OpenTidal is a Tidal-style desktop music app written in Rust. It runs on
[Rune](https://github.com/anomalyco/rune), a wgpu and winit UI framework that
is vendored under `rune/`.

This is a deep UI shell. It has a grouped sidebar, a top bar, section and
detail screens, live search, settings subpages, a collapsible sidebar, and a
now-playing bar. The catalog is placeholder data, so there is no network, no
playback, and no Tidal account. Those are later work.

## Build and run

```sh
cargo run
```

The toolchain is pinned in `rust-toolchain.toml`.

## Verify

The app verifies headless. No display or GPU window is needed.

```sh
cargo test                       # the state-machine invariant proof
scripts/control-opentidal doctor # one readiness check
```

Drive a screen and assert its state:

```sh
scripts/control-opentidal drive SCRIPT.json out/ --scene default
scripts/tree-assert.py out/tree.json has=play checked=nav-Home
```

See `.agents/skills/verify-opentidal/SKILL.md` for the full harness and the
feature map under `features/`.

## Structure

`src/state.rs` owns the `Route` model, the navigation stack, and the state
transitions. Its tests prove the safety invariant exhaustively over the
finite state space.

`src/app.rs` is the rune app state. It holds the proven `State` core plus the
search field's edit buffer, which keeps the proof bounded.

`src/screens.rs` builds every screen from shared card, shelf, grid, and track
row helpers.

`src/data.rs` is the placeholder catalog.
