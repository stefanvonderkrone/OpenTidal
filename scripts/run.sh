#!/usr/bin/env bash
# Build, ad-hoc sign, and run OpenTidal. Signing gives the dev binary a stable
# identity, so the macOS keychain does not ask for the token on every launch.
# Run with the TIDAL_* env vars set for live mode.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug/opentidal"

(cd "$ROOT" && cargo build)

if [[ "$(uname -s)" == "Darwin" ]]; then
    codesign --force -s - "$BIN"
fi

exec "$BIN" "$@"
