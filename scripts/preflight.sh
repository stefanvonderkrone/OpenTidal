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

# Expanded as `${fix[@]+"${fix[@]}"}` below. An empty array under `set -u` is an
# unbound variable in bash 3.2, which is the bash macOS ships as `/bin/bash` and
# which `/bin/sh` does not cover. The expansion costs nothing on bash 5.
fix=()
if [[ "${1:-}" == "--fix" ]]; then
    fix=(--fix --allow-dirty --allow-staged)
fi

echo "== tests"
cargo test -q

echo "== clippy"
# `--no-deps` keeps the vendored rune warnings out of the result. Only this
# crate must be clean, because this crate is the one a change here touches.
cargo clippy -q -p opentidal --all-targets --no-deps ${fix[@]+"${fix[@]}"} -- -D warnings

echo "== evidence table"
python3 scripts/evidence-status.py

echo "== no copied scope list"
# The list lives in `DEFAULT_SCOPE` and nowhere else. `scripts/scope-check.py` reads
# the working tree, the index and the untracked files, and fails when it cannot read
# something. Every earlier shell version failed open in a different way.
python3 scripts/scope-check.py

echo "== ci runs the scripts"
# CI runs this script and nothing else. A workflow that copied the commands would
# go green while the real gate drifted, which is the failure this whole script
# exists to prevent.
workflow=.github/workflows/preflight.yml
if [[ ! -f $workflow ]]; then
    echo "FAIL: $workflow is missing, so no push runs this script." >&2
    exit 1
fi
if ! grep -qE '^[[:space:]]*(run:[[:space:]]*)?scripts/preflight\.sh[[:space:]]*$' "$workflow"; then
    echo "FAIL: $workflow does not run scripts/preflight.sh. Naming it in a comment is not a call." >&2
    exit 1
fi
if ! grep -qE '^[[:space:]]*(run:[[:space:]]*)?scripts/preflight-linux\.sh[[:space:]]*$' "$workflow"; then
    echo "FAIL: $workflow has no job that runs scripts/preflight-linux.sh, so a Linux-only build break goes unseen." >&2
    exit 1
fi
if grep -nE 'cargo (test|clippy)' "$workflow"; then
    echo "FAIL: $workflow copies a gate command. Call scripts/preflight.sh instead." >&2
    exit 1
fi

echo "preflight: ok"
