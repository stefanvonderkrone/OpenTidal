#!/usr/bin/env python3
"""Assert that a keyed element in a headless snap shows one colour.

Usage:
  pixel-assert.py FRAME.png TREE.json KEY RRGGBB SHARE
  pixel-assert.py --self-test

The tree comes from `--tree` and the frame from `--snap` of the same run. The
element's `rect` is in points and the frame is in device pixels, so the rect is
scaled by the frame width over the viewport width. The check samples the inner
80 percent of the rect and passes when at least SHARE of those pixels are
within 8 of RRGGBB in every channel.

A missing key fails. The cover element is keyed `cover-image` only when its
image decoded with no error, so a placeholder can never pass. `--self-test`
proves the check passes on a matching rect and fails on a wrong one and on a
missing key. The preflight runs it.
"""
import json
import struct
import sys
import tempfile
import zlib
from pathlib import Path

TOLERANCE = 8


class Fail(Exception):
    pass


def read_png(path):
    data = Path(path).read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise Fail(f"{path} is not a PNG")
    pos, idat, header = 8, b"", None
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    if header is None:
        raise Fail(f"{path} has no header")
    width, height, depth, colour, _, _, interlace = header
    channels = {2: 3, 6: 4}.get(colour)
    if depth != 8 or channels is None or interlace != 0:
        raise Fail(f"{path}: only 8-bit RGB or RGBA without interlace is read")
    raw = zlib.decompress(idat)
    stride = width * channels
    rows, prior, at = [], bytearray(stride), 0
    for _ in range(height):
        kind, line = raw[at], bytearray(raw[at + 1:at + 1 + stride])
        at += 1 + stride
        unfilter(kind, line, prior, channels)
        rows.append(line)
        prior = line
    return width, height, channels, rows


def unfilter(kind, line, prior, bpp):
    if kind == 0:
        return
    if kind == 2:
        for i in range(len(line)):
            line[i] = (line[i] + prior[i]) & 0xFF
        return
    for i in range(len(line)):
        left = line[i - bpp] if i >= bpp else 0
        up = prior[i]
        if kind == 1:
            add = left
        elif kind == 3:
            add = (left + up) // 2
        elif kind == 4:
            corner = prior[i - bpp] if i >= bpp else 0
            p = left + up - corner
            pa, pb, pc = abs(p - left), abs(p - up), abs(p - corner)
            add = left if pa <= pb and pa <= pc else up if pb <= pc else corner
        else:
            raise Fail(f"unknown PNG filter {kind}")
        line[i] = (line[i] + add) & 0xFF


def share(frame, tree, key, colour):
    width, height, channels, rows = read_png(frame)
    data = json.loads(Path(tree).read_text())
    element = next((e for e in data.get("elements", []) if e.get("key") == key), None)
    if element is None:
        raise Fail(f"no element keyed {key}")
    scale = width / data["viewport"][0]
    x, y, w, h = (v * scale for v in element["rect"])
    left, right = int(x + w * 0.1), int(x + w * 0.9)
    top, bottom = int(y + h * 0.1), int(y + h * 0.9)
    left, top = max(left, 0), max(top, 0)
    right, bottom = min(right, width), min(bottom, height)
    if right <= left or bottom <= top:
        raise Fail(f"{key} is outside the frame")
    want = bytes.fromhex(colour)
    hits = total = 0
    for row in rows[top:bottom]:
        for px in range(left, right):
            at = px * channels
            total += 1
            if all(abs(row[at + c] - want[c]) <= TOLERANCE for c in range(3)):
                hits += 1
    return hits / total


def check(frame, tree, key, colour, need):
    got = share(frame, tree, key, colour)
    if got < need:
        raise Fail(f"{key}: {got:.2f} of the pixels are #{colour}, need {need:.2f}")
    return got


def write_png(path, width, height, pixel):
    """An RGB PNG whose rows cycle through every filter type, so the reader's
    unfiltering is tested, not only the unfiltered case."""
    raw, prior = b"", bytes(width * 3)
    for y in range(height):
        line = b"".join(bytes(pixel(x, y)) for x in range(width))
        kind = y % 5
        out = bytearray()
        for i, value in enumerate(line):
            left = line[i - 3] if i >= 3 else 0
            up = prior[i]
            corner = prior[i - 3] if i >= 3 else 0
            if kind == 0:
                add = 0
            elif kind == 1:
                add = left
            elif kind == 2:
                add = up
            elif kind == 3:
                add = (left + up) // 2
            else:
                p = left + up - corner
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - corner)
                add = left if pa <= pb and pa <= pc else up if pb <= pc else corner
            out.append((value - add) & 0xFF)
        raw += bytes([kind]) + bytes(out)
        prior = line

    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    Path(path).write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )


def self_test():
    with tempfile.TemporaryDirectory() as tmp:
        frame, tree = Path(tmp) / "frame.png", Path(tmp) / "tree.json"
        # A 10 x 10 pt viewport at 2x. Magenta fills pixels 4 to 13, which is
        # the rect 2, 2, 5, 5 in points. The rest is a gradient.
        write_png(frame, 20, 20, lambda x, y: (255, 0, 255) if 4 <= x < 14 and 4 <= y < 14 else (x * 9, y * 9, 40))
        tree.write_text(json.dumps({
            "viewport": [10.0, 10.0],
            "elements": [
                {"key": "cover-image", "rect": [2.0, 2.0, 5.0, 5.0]},
                {"key": "cover", "rect": [5.0, 5.0, 4.0, 4.0]},
            ],
        }))
        check(frame, tree, "cover-image", "ff00ff", 0.9)
        for key in ("cover", "hero-cover-image"):
            try:
                check(frame, tree, key, "ff00ff", 0.9)
            except Fail:
                continue
            raise Fail(f"self-test: {key} passed and must fail")
    print("pixel-assert: self-test ok")


def main():
    args = sys.argv[1:]
    try:
        if args == ["--self-test"]:
            self_test()
            return
        if len(args) != 5:
            raise Fail("usage: pixel-assert.py FRAME.png TREE.json KEY RRGGBB SHARE")
        frame, tree, key, colour, need = args
        got = check(frame, tree, key, colour, float(need))
        print(f"PASS {key}: {got:.2f} of the pixels are #{colour}")
    except Fail as error:
        print(f"FAIL {error}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
