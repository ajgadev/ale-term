#!/usr/bin/env python3
"""Draws the ale-term app icon and writes assets/ale-term.icns.

The icon is a dark rounded square with a prompt chevron and a cursor block,
in the app's own colors. It is drawn from formulas (no image library), then
macOS's `sips` and `iconutil` make the sizes and the .icns file.
"""
import os
import struct
import subprocess
import sys
import tempfile
import zlib

SIZE = 1024
BACKGROUND = (14, 14, 16)
EDGE = (58, 58, 64)
ACCENT = (217, 119, 87)
TEXT = (228, 228, 231)


def rounded_box(x, y, cx, cy, half_w, half_h, radius):
    """Signed distance to a rounded rectangle (negative inside)."""
    dx = abs(x - cx) - (half_w - radius)
    dy = abs(y - cy) - (half_h - radius)
    outside = (max(dx, 0) ** 2 + max(dy, 0) ** 2) ** 0.5
    return outside + min(max(dx, dy), 0) - radius


def segment(x, y, ax, ay, bx, by):
    """Distance to the line segment a-b."""
    px, py, vx, vy = x - ax, y - ay, bx - ax, by - ay
    t = max(0.0, min(1.0, (px * vx + py * vy) / (vx * vx + vy * vy)))
    return ((px - vx * t) ** 2 + (py - vy * t) ** 2) ** 0.5


def coverage(distance):
    """Turns a signed distance into 0..1 coverage with a one-pixel soft edge."""
    return max(0.0, min(1.0, 0.5 - distance))


def over(base, color, alpha):
    return tuple(b + (c - b) * alpha for b, c in zip(base, color))


def render():
    rows = []
    # macOS icons leave a margin around the shape.
    half, radius = SIZE * 0.40, SIZE * 0.18
    for y in range(SIZE):
        row = bytearray([0])
        for x in range(SIZE):
            px, py = x + 0.5, y + 0.5
            body = rounded_box(px, py, SIZE / 2, SIZE / 2, half, half, radius)
            alpha = coverage(body)
            if alpha <= 0:
                row += b"\x00\x00\x00\x00"
                continue
            # A thin lighter ring just inside the edge.
            color = over(EDGE, BACKGROUND, coverage(body + 10)) if body > -10 else BACKGROUND
            # Prompt chevron: two strokes meeting at a point.
            stroke = 44
            tip = (SIZE * 0.47, SIZE * 0.50)
            chevron = min(
                segment(px, py, SIZE * 0.30, SIZE * 0.36, *tip),
                segment(px, py, SIZE * 0.30, SIZE * 0.64, *tip),
            ) - stroke
            color = over(color, ACCENT, coverage(chevron))
            # Cursor block.
            cursor = rounded_box(px, py, SIZE * 0.635, SIZE * 0.50, SIZE * 0.075, SIZE * 0.15, 10)
            color = over(color, TEXT, coverage(cursor))
            row += bytes(int(round(c)) for c in color) + bytes([int(round(alpha * 255))])
        rows.append(bytes(row))
    return b"".join(rows)


def write_png(path, pixels):
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    with open(path, "wb") as out:
        out.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(pixels, 9)) + chunk(b"IEND", b""))


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    target = os.path.join(root, "assets", "ale-term.icns")
    with tempfile.TemporaryDirectory() as work:
        master = os.path.join(work, "icon.png")
        write_png(master, render())
        iconset = os.path.join(work, "ale-term.iconset")
        os.mkdir(iconset)
        for size in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                name = f"icon_{size}x{size}{'@2x' if scale == 2 else ''}.png"
                pixels = str(size * scale)
                subprocess.run(
                    ["sips", "-z", pixels, pixels, master, "--out", os.path.join(iconset, name)],
                    check=True,
                    stdout=subprocess.DEVNULL,
                )
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", target], check=True)
        # A copy of the 256px version, handy for previews and the README.
        subprocess.run(
            ["sips", "-z", "256", "256", master, "--out", os.path.join(root, "assets", "icon-256.png")],
            check=True,
            stdout=subprocess.DEVNULL,
        )
    print("wrote", target)


if __name__ == "__main__":
    sys.exit(main())
