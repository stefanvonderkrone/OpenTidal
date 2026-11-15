#!/usr/bin/env bash
# Build, ad-hoc sign, and run OpenTidal. Signing gives the dev binary a stable
# identity, so the macOS keychain does not ask for the token on every launch.
# For live mode set OPENTIDAL_LIVE=1 and the TIDAL_* env vars:
#   OPENTIDAL_LIVE=1 TIDAL_CLIENT_ID=... TIDAL_CLIENT_SECRET=... \
#   TIDAL_REDIRECT_URI=http://127.0.0.1:8080/callback \
#   TIDAL_SCOPE="user.read collection.read" scripts/run.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Resolve the target directory Cargo actually uses (CARGO_TARGET_DIR or a Cargo
# config can move it), so the sign step and the launch use the same binary.
TARGET_DIR="$(cd "$ROOT" && cargo metadata --format-version 1 --no-deps -q \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
BIN="$TARGET_DIR/debug/opentidal"

(cd "$ROOT" && cargo build)

if [[ "$(uname -s)" == "Darwin" ]]; then
    codesign --force -s - "$BIN"
fi

exec "$BIN" "$@"
