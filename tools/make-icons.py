#!/usr/bin/env python3
"""Sealwax app icons, generated from the brand canvas's "Icon tile" and "Seal mark" artboards
(the user's Sealwax Logo canvas, 2026-10-04): an envelope tile with the wax seal and the @ emblem.

    python3 tools/make-icons.py packaging/icons

writes hicolor/scalable/apps/<id>.svg (detailed seal) and hicolor/{16,24,32,48,64}x…/apps/<id>.svg
(the simplified seal the canvas uses at 64 px and below)."""

import pathlib
import sys

APP_ID = "io.github.pixdevsapps.Sealwax"
WAX = "#B3302A"
BLOB = ("M103.29 0C103.22 4.9 102.73 9.83 101.95 14.66C101.17 19.48 100.45 24.48 98.62 28.96C96.79 33.44 93.53 37.45 90.98 41.55C88.42 45.64 85.82 49.51 83.3 53.53C80.78 57.56 79.05 62.32 75.84 65.72C72.63 69.11 68.2 71.65 64.05 73.92C59.91 76.18 54.92 76.9 50.97 79.31C47.02 81.72 44.05 85.25 40.36 88.38C36.67 91.51 33.08 95.77 28.8 98.09C24.53 100.42 19.51 101.48 14.71 102.33C9.91 103.18 4.87 103.44 0 103.18C-4.87 102.92 -9.93 102.43 -14.49 100.77C-19.05 99.11 -23.18 95.54 -27.37 93.21C-31.56 90.87 -35.19 88.3 -39.62 86.77C-44.06 85.23 -49.41 85.56 -54 84.02C-58.59 82.48 -63.47 80.51 -67.16 77.51C-70.86 74.51 -73.23 69.84 -76.18 66.01C-79.14 62.19 -82.1 58.49 -84.91 54.57C-87.72 50.65 -91.05 46.85 -93.06 42.5C-95.07 38.15 -95.92 33.18 -96.97 28.47C-98.02 23.77 -98.83 19.03 -99.38 14.29C-99.92 9.54 -100.97 4.66 -100.27 0C-99.56 -4.66 -96.99 -9.32 -95.14 -13.68C-93.29 -18.04 -90.27 -21.73 -89.17 -26.18C-88.06 -30.64 -88.92 -35.5 -88.51 -40.42C-88.09 -45.34 -88.33 -51.09 -86.68 -55.71C-85.03 -60.32 -81.75 -64.34 -78.63 -68.13C-75.51 -71.92 -71.82 -75.4 -67.96 -78.44C-64.11 -81.47 -59.99 -84.47 -55.5 -86.36C-51.01 -88.25 -45.68 -88.6 -41 -89.78C-36.32 -90.96 -31.89 -91.84 -27.44 -93.45C-22.99 -95.05 -18.87 -98.27 -14.3 -99.43C-9.72 -100.58 -4.69 -100.89 0 -100.37C4.69 -99.86 9.22 -97.29 13.85 -96.33C18.48 -95.36 23.02 -95.18 27.77 -94.58C32.52 -93.98 37.76 -94.16 42.34 -92.72C46.92 -91.28 51.19 -88.59 55.24 -85.96C59.3 -83.33 63.2 -80.28 66.68 -76.96C70.17 -73.63 73.82 -70.18 76.17 -66C78.51 -61.82 79.27 -56.42 80.76 -51.9C82.25 -47.38 82.8 -42.88 85.1 -38.86C87.4 -34.84 91.69 -31.79 94.57 -27.77C97.45 -23.74 100.93 -19.35 102.38 -14.72C103.84 -10.09 103.36 -4.9 103.29 0Z")
AT = "M12 0A12 12 0 1 0 -12 0A12 12 0 1 0 12 0ZM12 -12V5A10 10 0 0 0 32 5V0A32 32 0 1 0 18.35 26.21"


def mix(a, b, t):
    out = "#"
    for i in range(3):
        x = int(a[1 + i * 2:3 + i * 2], 16)
        y = int(b[1 + i * 2:3 + i * 2], 16)
        out += f"{round(x * (1 - t) + y * t):02x}"
    return out


def seal(simple):
    """The seal mark's layers, in its own -120..120 coordinate space."""
    emblem_w = 9 if simple else 6.5
    ring_w = 4 if simple else 2.2
    line = mix(WAX, "#000000", 0.46)
    beads = "" if simple else (
        f'<circle r="57" fill="none" stroke="{line}" stroke-width="3.6" stroke-linecap="round" '
        'stroke-dasharray="0 7.46"/>')
    emblem = (f'<g transform="scale(1.15)" fill="none" stroke-width="{emblem_w}" stroke-linecap="round" '
              f'stroke-linejoin="round"><path d="{AT}" stroke="{mix(WAX, "#FFFFFF", 0.2)}" '
              f'transform="translate(1.2 1.6)"/><path d="{AT}" stroke="{line}"/></g>')
    return "".join([
        f'<path d="{BLOB}" fill="#000" fill-opacity="0.28" transform="translate(0 6)"/>',
        f'<path d="{BLOB}" fill="{mix(WAX, "#000000", 0.38)}" transform="translate(2 3)"/>',
        f'<path d="{BLOB}" fill="{mix(WAX, "#FFFFFF", 0.16)}" transform="translate(-2 -2)"/>',
        f'<path d="{BLOB}" fill="{WAX}"/>',
        f'<circle cx="-2.5" cy="-2.5" r="72" fill="{mix(WAX, "#000000", 0.42)}"/>',
        f'<circle cx="2.5" cy="2.5" r="72" fill="{mix(WAX, "#FFFFFF", 0.22)}"/>',
        f'<circle r="72" fill="{mix(WAX, "#000000", 0.08)}"/>',
        f'<circle r="65" fill="none" stroke="{line}" stroke-width="{ring_w}"/>',
        beads,
        emblem,
    ])


def tile(size, simple):
    """The paper tile at `size` px: envelope flap and folds, then the seal at 56 % height."""
    radius = round(size * 0.225)
    fold_w = 1.6 if simple else 0.9
    seal_size = round(size * 0.56)
    left = round(size / 2 - seal_size / 2)
    top = round(size * 0.56 - seal_size / 2)
    k = size / 100
    s = seal_size / 240
    cx, cy = left + seal_size / 2, top + seal_size / 2
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">'
        f'<defs><clipPath id="t"><rect width="{size}" height="{size}" rx="{radius}"/></clipPath></defs>'
        f'<g clip-path="url(#t)"><rect width="{size}" height="{size}" fill="#F2EADB"/>'
        f'<g transform="scale({k:g})"><path d="M0 100L50 60L100 100" fill="none" stroke="#DCCCB0" '
        f'stroke-width="{fold_w}"/><path d="M0 0H100L50 56Z" fill="#E5D8C0"/>'
        f'<path d="M0 0L50 56L100 0" fill="none" stroke="#CDBB9C" stroke-width="{fold_w}" '
        'stroke-linejoin="round"/></g>'
        f'<g transform="translate({cx:g} {cy:g}) scale({s:.6g})">{seal(simple)}</g>'
        f'<rect x="0.5" y="0.5" width="{size - 1}" height="{size - 1}" rx="{radius - 0.5:g}" fill="none" '
        'stroke="#15151C" stroke-opacity="0.08"/></g></svg>\n')


def main():
    out = pathlib.Path(sys.argv[1])
    targets = [("scalable", 512, False)] + [(f"{n}x{n}", n, True) for n in (16, 24, 32, 48, 64)]
    for folder, size, simple in targets:
        path = out / "hicolor" / folder / "apps" / f"{APP_ID}.svg"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(tile(size, simple))
        print(path)


if __name__ == "__main__":
    main()
