#!/usr/bin/env python3
"""Fail when a Tidal endpoint OpenTidal relies on is not a third-party endpoint.

I claimed three times that an endpoint was usable because a summary doc, a
generated client, or a third-party library listed it. Each claim was wrong. The
spec marks those operations INTERNAL, and one of them needs a scope this app
cannot request. The summary did not say so. This check reads the access tier
from the pinned spec snapshot and compares it with the endpoints the app uses.

The snapshot lives in `docs/tidal-api-access.json`. Run this file with
`--refresh` to rebuild the snapshot from the sources named in it. Every other
run is offline and fails closed. A snapshot that disagrees with an endpoint in
the manifest is a failure, not a reason to guess.

The `used` list names operations the code may call. Each must be THIRD_PARTY.
The `forbidden` list names operations that look useful but are INTERNAL. Each
must stay INTERNAL. That second list catches a future edit that promotes one in
the snapshot to make a check pass.
"""
import json
import re
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ACCESS = ROOT / "docs" / "tidal-api-access.json"

PATH_LINE = re.compile(r"^  (/[^:]*):$")
METHOD_LINE = re.compile(r"^    (get|post|patch|delete|put):$")
TIER_LINE = re.compile(r"^\s+required-access-tier:\s*(\w+)\s*$")


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def load() -> dict:
    try:
        return json.loads(ACCESS.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        fail(f"cannot read {ACCESS.relative_to(ROOT)}: {exc}")


def tiers_from(text: str) -> dict:
    """Read each operation's access tier from one generated OpenAPI file."""
    out: dict = {}
    path = None
    method = None
    for line in text.splitlines():
        match = PATH_LINE.match(line)
        if match:
            path = match.group(1)
            method = None
            continue
        match = METHOD_LINE.match(line)
        if match:
            method = match.group(1)
            continue
        match = TIER_LINE.match(line)
        if match and path and method:
            out.setdefault(path, {})[method] = match.group(1)
    return out


def refresh(data: dict) -> None:
    snapshot: dict = {}
    for name, url in data["sources"].items():
        try:
            with urllib.request.urlopen(url, timeout=30) as response:
                text = response.read().decode("utf-8")
        except Exception as exc:
            fail(f"cannot read the {name} spec at {url}: {exc}")
        snapshot[name] = tiers_from(text)
    data["tiers"] = snapshot
    ACCESS.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"refreshed {ACCESS.relative_to(ROOT)} from {len(snapshot)} specs")


def check(data: dict) -> None:
    tiers = data["tiers"]
    bad = []
    for kind, expected in (("used", "THIRD_PARTY"), ("forbidden", "INTERNAL")):
        for entry in data[kind]:
            name, path, method = entry["spec"], entry["path"], entry["method"]
            actual = tiers.get(name, {}).get(path, {}).get(method)
            if actual != expected:
                bad.append(
                    f"{kind} entry {name} {method.upper()} {path}: "
                    f"expected {expected}, snapshot says {actual or 'nothing'}"
                )
    if bad:
        for line in bad:
            print(f"FAIL: {line}", file=sys.stderr)
        print("Fix the endpoint list. Do not edit the snapshot by hand.", file=sys.stderr)
        raise SystemExit(1)
    print(f"tidal api: {len(data['used'])} used and {len(data['forbidden'])} forbidden paths agree with the snapshot")


def main() -> None:
    args = sys.argv[1:]
    if args == ["--refresh"]:
        refresh(load())
        return
    if args:
        fail(f"unknown arguments: {' '.join(args)}")
    check(load())


if __name__ == "__main__":
    main()
