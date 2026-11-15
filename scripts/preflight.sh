#!/usr/bin/env bash
# The one check to run before every commit. It runs the same commands CI runs,
# so a local pass means the push is green.
#
#   scripts/preflight.sh
#
# Pass `--fix` to apply the fixes Cargo can apply itself.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

fix=()
if [[ "${1:-}" == "--fix" ]]; then
    fix=(--fix --allow-dirty --allow-staged)
fi

echo "== tests"
cargo test -q

echo "== clippy"
# `--no-deps` keeps the vendored rune warnings out of the result. Only this
# crate must be clean, because this crate is the one a change here touches.
cargo clippy -q -p opentidal --all-targets --no-deps "${fix[@]}" -- -D warnings

echo "== evidence table"
python3 scripts/evidence-status.py

echo "== no copied scope list"
# The scope list lives in `DEFAULT_SCOPE` and nowhere else. It was pasted into
# four files and drifted twice. An empty scope already means every read scope,
# so no doc needs the literal string.
if grep -rn --include='*.md' --include='*.sh' --include='*.json' \
    -e 'user\.read collection\.read' . 2>/dev/null | grep -v '^\./target/'; then
    echo "FAIL: the scope list is copied outside src/auth.rs. Leave TIDAL_SCOPE unset." >&2
    exit 1
fi

echo "preflight: ok"
