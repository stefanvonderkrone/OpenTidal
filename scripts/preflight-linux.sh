#!/usr/bin/env bash
# The part of the gate that runs off macOS. Not the full check: the macOS-only
# tests do not exist here, and clippy runs on the Mac.
#
#   scripts/preflight-linux.sh
#
# This exists because the crate stopped compiling on Linux twice, and nothing else
# in this repo noticed. `cargo test` is the whole point: a macOS-only item with no
# counterpart for other targets has no definition here at all.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "== build and test off macOS"
cargo test -q

echo "preflight-linux: ok"
