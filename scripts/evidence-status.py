#!/usr/bin/env python3
"""Check that every test named in docs/user-state.md exists.

Usage: scripts/evidence-status.py

The design note's evidence table is the promise that the work is verified. A row
that names a test that no longer exists, or was renamed, makes the whole table
worthless. So a rename has to update the table in the same change.

A Status cell is either `test: <name>` (one or more, comma-separated), `live:
<date>`, `pending: <what>`, or `not yet run`. Only the `test:` names are checked
here. This runs `cargo test -- --list`, so it agrees with the code rather than a
hand-kept list.
"""
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "user-state.md"


def real_tests() -> set[str]:
    out = subprocess.run(
        ["cargo", "test", "--quiet", "--", "--list"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        print("FAIL: cargo test --list did not run", file=sys.stderr)
        sys.exit(1)
    return {
        line.split(": test")[0].split("::")[-1]
        for line in out.stdout.splitlines()
        if line.endswith(": test")
    }


def claimed_tests(doc: str) -> list[str]:
    names: list[str] = []
    for line in doc.splitlines():
        if not line.startswith("|") or "test:" not in line:
            continue
        status = line.rsplit("|", 2)[1]
        for part in status.split("test:", 1)[1].split(","):
            name = part.strip().strip("`").split()[0] if part.strip() else ""
            if name:
                names.append(name)
    return names


def main() -> int:
    tests = real_tests()
    claimed = claimed_tests(DOC.read_text())
    missing = [name for name in claimed if name not in tests]

    if missing:
        print("FAIL: the evidence table names tests that do not exist:", file=sys.stderr)
        for name in missing:
            print(f"  {name}", file=sys.stderr)
        return 1

    print(f"evidence-status: ok ({len(claimed)} test names checked)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
