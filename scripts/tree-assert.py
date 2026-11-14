#!/usr/bin/env python3
"""Assert properties of an OpenTidal `--tree` JSON dump.

Usage: tree-assert.py TREE.json PREDICATE...

Each predicate is `name=arg` (or `name=key=value`). Prints one PASS/FAIL line per
predicate and exits non-zero if any fails.

Predicates (all keys are element `key` strings from `--tree`):
  title=VALUE           top-level window title equals VALUE
  viewport=WxH          top-level viewport equals [W, H]
  has=KEY               a keyed element named KEY exists
  absent=KEY            no keyed element named KEY exists
  label=KEY=VALUE       element KEY's label equals VALUE
  label-has=KEY=SUB     element KEY's label contains SUB
  text=KEY=VALUE        element KEY's text equals VALUE
  checked=KEY           accessibility node KEY has state "checked"
  unchecked=KEY         accessibility node KEY has state "unchecked"
  fill=KEY=VALUE        element KEY's fill equals VALUE
  fill-not=KEY=VALUE    element KEY's fill differs from VALUE
"""
import json
import sys


def find(elements, key):
    for e in elements:
        if e.get("key") == key:
            return e
    return None


def acc_find(node, key):
    if node.get("key") == key:
        return node
    for child in node.get("children", []):
        found = acc_find(child, key)
        if found is not None:
            return found
    return None


def main():
    path = sys.argv[1]
    predicates = sys.argv[2:]
    data = json.load(open(path))
    elements = data.get("elements", [])
    acc = data.get("accessibility", {})

    failures = 0
    for pred in predicates:
        name, _, arg = pred.partition("=")
        ok = True
        detail = ""

        if name == "title":
            ok = data.get("title") == arg
            detail = f"title {data.get('title')!r} != {arg!r}"
        elif name == "viewport":
            w, h = (float(x) for x in arg.split("x"))
            vp = data.get("viewport")
            ok = vp == [w, h]
            detail = f"viewport {vp!r} != [{w}, {h}]"
        elif name == "has":
            ok = find(elements, arg) is not None
            detail = f"element {arg!r} missing"
        elif name == "absent":
            ok = find(elements, arg) is None
            detail = f"element {arg!r} unexpectedly present"
        elif name == "label":
            key, want = arg.split("=", 1)
            e = find(elements, key)
            ok = e is not None and e.get("label") == want
            detail = f"label {key!r} {(e and e.get('label'))!r} != {want!r}"
        elif name == "label-has":
            key, sub = arg.split("=", 1)
            e = find(elements, key)
            ok = e is not None and sub in (e.get("label") or "")
            detail = f"label {key!r} {(e and e.get('label'))!r} lacks {sub!r}"
        elif name == "text":
            key, want = arg.split("=", 1)
            e = find(elements, key)
            ok = e is not None and e.get("text") == want
            detail = f"text {key!r} {(e and e.get('text'))!r} != {want!r}"
        elif name in ("checked", "unchecked"):
            node = acc_find(acc, arg)
            states = (node or {}).get("states", [])
            ok = name in states
            detail = f"{arg!r} states {states!r} lacks {name!r}"
        elif name == "fill":
            key, want = arg.split("=", 1)
            e = find(elements, key)
            ok = e is not None and e.get("fill") == want
            detail = f"fill {key!r} {(e and e.get('fill'))!r} != {want!r}"
        elif name == "fill-not":
            key, avoid = arg.split("=", 1)
            e = find(elements, key)
            ok = e is not None and e.get("fill") != avoid
            detail = f"fill {key!r} {(e and e.get('fill'))!r} == {avoid!r}"
        else:
            ok = False
            detail = f"unknown predicate {name!r}"

        print(("PASS " if ok else "FAIL ") + pred + ("" if ok else f"  ({detail})"))
        if not ok:
            failures += 1

    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
