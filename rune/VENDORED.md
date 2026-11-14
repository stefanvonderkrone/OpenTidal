# Vendored rune

Rune is vendored from <https://github.com/anomalyco/rune>, branch `dev`.

- Upstream commit: `4970cffe6a4e90c7d613796be8ce39eb3dcb281b` (`4970cff`).
- The nested git repository was removed (flattened). To update rune, re-clone upstream and replace this directory.

The app depends on `rune` via `path = "rune/crates/rune"` and patches the same four
crates rune patches, as rune's "Depending on Rune" README section describes. See
`[patch.crates-io]` in the root `Cargo.toml`.
