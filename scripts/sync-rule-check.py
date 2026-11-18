#!/usr/bin/env python3
"""Fail when a design doc describes the retired whole-library sync as current.

The app once read every page of the Tidal library before it wrote one row. It
now reads one page at a time and applies each page as it lands. The code
changed, but the design prose kept the old claims, and the next agent read the
prose as the design. Each phrase below is copied from the retired text, so a
doc that brings one back fails here with the file, the line and the phrase.

The match ignores case and line wrapping, because prose is rewrapped.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = ["docs/tidal-api.md", "docs/user-state.md", "GLOSSARY.md"]

RETIRED = [
    "it writes only after every page arrives",
    "no partial read is applied",
    "fails the whole fetch",
    "covers the whole fetch",
    "fetch everything, then apply once",
    "there is no page-by-page merge",
    "replace, never merge",
    "reads the whole Tidal library",
    "read the whole Tidal library",
    "a library larger than the page cap never syncs",
    "a fetch that fails on any page changes nothing",
]

FIX = "Describe one pass per remote list that applies each page as it lands. See the Sync section of docs/user-state.md."


def flexible(phrase: str) -> re.Pattern:
    return re.compile(r"\s+".join(map(re.escape, phrase.split())), re.IGNORECASE)


def main() -> int:
    found = []
    for name in DOCS:
        path = ROOT / name
        try:
            text = path.read_text(encoding="utf-8")
        except OSError as error:
            print(f"FAIL: cannot read {name}: {error}", file=sys.stderr)
            return 1
        for phrase in RETIRED:
            for match in flexible(phrase).finditer(text):
                line = text.count("\n", 0, match.start()) + 1
                found.append(f"{name}:{line}: retired sync claim \"{phrase}\"")
    if found:
        for entry in found:
            print(f"FAIL: {entry}", file=sys.stderr)
        print(FIX, file=sys.stderr)
        return 1
    print(f"sync rule: ok ({len(RETIRED)} retired phrases, {len(DOCS)} docs)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
