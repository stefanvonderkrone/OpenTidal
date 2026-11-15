# OpenTidal

OpenTidal is a Tidal-style desktop music app written in Rust. It runs on
[Rune](https://github.com/anomalyco/rune), a wgpu and winit UI framework that
is vendored under `rune/`.

This is a deep UI shell. It has a grouped sidebar, a top bar, section and
detail screens, live search, settings subpages, a collapsible sidebar, and a
now-playing bar.

The user's data is real and mutable now: like tracks, save albums, follow
artists, download albums, and create, rename, reorder and delete playlists and
collections. All of it lives in memory and every operation is proven to keep
the library valid. The catalogue is still placeholder content, so there is no
network, no playback, and no Tidal account. Those are later work.

## Build and run

```sh
cargo run
```

The toolchain is pinned in `rust-toolchain.toml`.

## Verify

The app verifies headless. No display or GPU window is needed.

```sh
cargo test                       # the invariant proofs
scripts/control-opentidal doctor # one readiness check
```

Drive a screen and assert its state:

```sh
scripts/control-opentidal drive SCRIPT.json out/ --scene default
scripts/tree-assert.py out/tree.json has=play checked=nav-Home
```

The user-management journeys live under `features/`; each has a `script.json`
you can drive, and `features/README.md` is the map. The library and state
invariant proofs run in `cargo test`.

See `.agents/skills/verify-opentidal/SKILL.md` for the full harness.

## Structure

`src/state.rs` owns the `Route` model, the navigation stack, and the state
transitions, and now holds the user's `Library`. Its tests prove the combined
nav-and-library invariant: no route, playlist, history entry or saved item
references an id that does not exist.

`src/catalog.rs` is the immutable catalogue: artists, albums and tracks as a
graph (a track names its album and artist), plus the read-only editorial
playlists, with id newtypes.

`src/library.rs` is the single local user's data — liked/saved/followed/
downloaded bitsets, history, user playlists and collections — and its
validated operations, with an exhaustive invariant proof.

`src/app.rs` is the rune app state. It holds the proven `State` core plus the
edit buffers (search and rename), which keeps the proof bounded.

`src/screens.rs` builds every screen from shared card, shelf, grid, and track
row helpers, reading and mutating real state.
