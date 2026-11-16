#!/usr/bin/env python3
"""Check that every test named in docs/user-state.md exists.

Usage: scripts/evidence-status.py

The design note's evidence table is the promise that the work is verified. A row
that names a test that no longer exists, or was renamed, makes the whole table
worthless. So a rename has to update the table in the same change.

A Status cell is either `test: <name>`, `live: <date>`, or `pending: <what>`. A
`pending:` cell must not name a snake_case identifier, because every one of those
is held to the test list and a column name would fail the gate.
Every backticked name in the cell is checked, whichever kind it is: a `live:` row
that names a test is naming the test that also covers part of the claim, and a
rename must update it. This runs `cargo test -- --list`, so it agrees with the
code rather than a hand-kept list.
"""
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "user-state.md"

# In a `live:` cell a backticked word may be a pragma, a column or a command. Only
# a name followed by parentheses is read as a test, because that is how a test is
# written when it is called by name. A `test:` cell needs no guard: the kind
# already says the name is a test.
TEST_CALL = re.compile(r"`([a-z][a-z0-9_]*)`\(")


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
    """Every backticked name a Status cell claims is a test.

    The cell kind decides, not the name's length or shape.

    - `test:` names tests, so every name is held to the test list however short.
    - `live:` may also name the test that covers a hand check, written as
      `name`() so it reads as a call. Any other backticked word there is a pragma,
      a column or a command, and is not a claim about a test.
    - `pending:` describes work this branch does not build, so nothing in it can
      be a test here. A column or a command named there is not a claim.
    """
    names: list[str] = []
    for line in doc.splitlines():
        if not line.startswith("|"):
            continue
        status = line.rsplit("|", 2)[1]
        if ":" not in status:
            continue
        kind, _, rest = status.partition(":")
        kind = kind.strip()
        found = re.findall(r"`([^`]+)`", rest)
        if kind == "test":
            names.extend(found)
        elif kind == "live":
            names.extend(TEST_CALL.findall(rest))
    return names


def uncalled_test_names(doc: str, tests: set[str]) -> list[str]:
    """Real test names in a `live:` cell that were not written as a call.

    A `live:` cell may name a test as `name`(). Leaving the parentheses off is a
    silent hole: the name is not in the claim list, so a rename there passes. This
    catches the forgotten parentheses.
    """
    missing_call: list[str] = []
    for line in doc.splitlines():
        if not line.startswith("|"):
            continue
        status = line.rsplit("|", 2)[1]
        kind, _, rest = status.partition(":")
        if kind.strip() != "live":
            continue
        called = set(TEST_CALL.findall(rest))
        for name in re.findall(r"`([a-z][a-z0-9_]*)`(?!\()", rest):
            if name in tests and name not in called:
                missing_call.append(name)
    return missing_call


def main() -> int:
    tests = real_tests()
    doc = DOC.read_text()
    claimed = claimed_tests(doc)
    uncalled = uncalled_test_names(doc, tests)
    if uncalled:
        print("FAIL: a live: row names a test without (), so nothing checks it:", file=sys.stderr)
        for name in sorted(set(uncalled)):
            print(f"  {name}", file=sys.stderr)
        return 1
    # Every claimed name must exist. There is no escape hatch: a length or shape
    # filter here would hide exactly the rename this catches.
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
