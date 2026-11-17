#!/usr/bin/env python3
"""Fail when the Tidal scope list is copied, or a scope is assigned, outside `src/auth.rs`.

The list lives in `DEFAULT_SCOPE` and nowhere else. It was pasted into four files
and drifted twice, so a copy is a mistake that has already happened more than once.

Two mistakes are caught. The literal list anywhere is a copy. An assignment of the
variable to any value, in a doc or a script, is the same mistake in another form: it
states a scope the app does not use, and it can drift from `DEFAULT_SCOPE` exactly as
the pasted lists did. The check first looked only for the exact list, so an assignment
naming some other scope in a README passed it.

This file must not spell either mistake out, so both patterns are built from parts.

This reads every tracked file, every untracked but not ignored file, and the staged
copy of each, because all three are what a commit can carry:

- the working tree, because a copy is a mistake before it is staged;
- the index, because a copy staged and then removed from disk still commits;
- untracked files, because a new file is not in `git ls-files` yet.

Whatever it cannot read, it reports as a failure. A scan that cannot run must never
look like a clean tree. That is how the shell version failed three times: a pipeline
into `xargs` turned a missing file into "no copy", `git ls-files` without `-z`
skipped a filename with a tab, and `grep` with the error redirected read an
unreadable file as no match.
"""
import os
import re
import subprocess
import sys
from pathlib import Path

# Built from parts, so this file does not contain the copy it looks for.
NEEDLE = b"user.read" + b" collection.read"
# An assignment of the variable to anything, in prose, in a script, or as a quoted
# config key. Built from parts, because a rule that spells out what it forbids matches
# its own source, and a check that fails on itself is a check nobody keeps.
#
# The value is optional: an empty assignment is the same mistake, because it states a
# scope the app should have left unset. A quote either side of the name is allowed, so
# a JSON config example is caught too.
ASSIGNMENT = re.compile(rb"[\"']?" + rb"TIDAL" + rb"_SCOPE[\"']?\s*[=:]")
# Only the file that owns the list. Nothing else is skipped: vendored code is still
# code this repository ships, so a scope list in it is still a second copy.
SKIP_FILES = ("src/auth.rs",)


def why(content: bytes) -> str:
    """The reason this content fails, or an empty string when it is clean."""
    reasons = []
    if NEEDLE in content:
        reasons.append("the list")
    if ASSIGNMENT.search(content):
        reasons.append("an assignment")
    return " and ".join(reasons)


def git_env() -> dict[str, str]:
    """The environment for a git call: the caller's, minus the two variables that
    point git at a different repository.

    With `GIT_DIR` or `GIT_WORK_TREE` set, git enumerates another repository while the
    file reads still use this checkout, so the scan could pass without looking at the
    files it is meant to check. `GIT_INDEX_FILE` is kept: a pre-commit hook runs this
    with a temporary index, and the staged read is only right if it reads that one.
    """
    env = dict(os.environ)
    for name in ("GIT_DIR", "GIT_WORK_TREE"):
        env.pop(name, None)
    return env


def git(*args: str) -> bytes:
    """Run git and return its stdout, or stop the check."""
    result = subprocess.run(["git", *args], capture_output=True, env=git_env())
    if result.returncode != 0:
        message = result.stderr.decode(errors="replace").strip()
        print(f"FAIL: git {' '.join(args)} failed, so the scope check did not run:", file=sys.stderr)
        print(f"  {message}", file=sys.stderr)
        sys.exit(1)
    return result.stdout


def paths() -> tuple[list[str], set[str]]:
    """The paths to scan, and which of them the index knows about.

    The two lists are kept apart because they answer different questions later: a path
    the index has no entry for is an untracked file, and a path it does have an entry
    for must be readable from the index. Treating them as one list hides a failed
    index read behind a working-tree file.
    """
    tracked = [p.decode(errors="surrogateescape") for p in git("ls-files", "-z").split(b"\0") if p]
    untracked = [
        p.decode(errors="surrogateescape")
        for p in git("ls-files", "-z", "--others", "--exclude-standard").split(b"\0")
        if p
    ]
    # An unmerged path is listed once per stage, so it repeats here.
    names = dict.fromkeys(tracked + untracked)
    return [n for n in names if n not in SKIP_FILES], set(tracked)


def main() -> int:
    names, tracked = paths()
    if not names:
        print("FAIL: the scope check found no files, so it did not run.", file=sys.stderr)
        return 1

    found = []
    for name in names:
        where = []
        path = Path(name)
        if path.is_file():
            try:
                reason = why(path.read_bytes())
                if reason:
                    where.append(f"working tree: {reason}")
            except OSError as error:
                print(f"FAIL: cannot read {name} ({error}), so the scope check did not finish.", file=sys.stderr)
                return 1
        # The staged copy, which the next commit carries even when the working tree
        # no longer has the file. A path the index does not know has no staged copy,
        # and that is the only reason a missing one is allowed: `git show :name` also
        # fails for an unmerged path, whose index entry has no stage 0, and passing
        # that as "no staged copy" would skip a file the next commit carries.
        staged = subprocess.run(["git", "show", f":{name}"], capture_output=True, env=git_env())
        if staged.returncode == 0:
            reason = why(staged.stdout)
            if reason:
                where.append(f"staged: {reason}")
        elif name in tracked:
            print(
                f"FAIL: the index has {name} but its content could not be read, so the\n"
                "scope check did not finish. An unmerged path is one cause: resolve it first.",
                file=sys.stderr,
            )
            return 1
        if where:
            found.append(f"{name} ({', '.join(where)})")

    if found:
        print(
            "FAIL: the scope list is copied, or a scope is assigned, outside src/auth.rs.\n"
            "Leave the variable unset: DEFAULT_SCOPE is the one list.",
            file=sys.stderr,
        )
        for entry in found:
            print(f"  {entry}", file=sys.stderr)
        return 1

    print(f"scope check: ok ({len(names)} files)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
