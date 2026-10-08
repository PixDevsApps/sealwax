#!/usr/bin/env python3
"""Sealwax app icons from the wax seal master (the user's request, 2026-10-08: "only the red
Sealwax with the "@" symbol inside, it should look authentic, like it was done in the old days.
No background or frame around it").

    python3 tools/make-icons.py packaging/icons

reads packaging/icons/source/sealwax-seal.png (1024 px, transparent around the seal) and writes
hicolor/{16,24,32,48,64,128,256,512}x…/apps/<id>.png, scaled down with Lanczos; at 64 px and
below the wax gets a little more contrast and sharpening, so the @ still shows. The old
envelope-tile SVGs are removed. Needs ImageMagick 7 (`magick`).

The master is a photographic seal made with Higgsfield (GPT Image 2.5, high quality, 2K,
transparent background; job 5001b9d2-677b-459d-9c5e-1831a2486dbc, 2026-10-08), trimmed to the
seal and scaled to 1024 px. Raster sizes, not an SVG: lit like a photograph, it can't be redrawn
the same at every size, and a scalable SVG would be preferred by GTK between the fixed sizes."""

import pathlib
import subprocess
import sys

APP_ID = "io.github.pixdevsapps.Sealwax"
SIZES = (16, 24, 32, 48, 64, 128, 256, 512)
# The sizes that get the extra contrast and sharpening.
SMALL = 64


def main():
    out = pathlib.Path(sys.argv[1])
    master = out / "source" / "sealwax-seal.png"
    if not master.is_file():
        sys.exit(f"{master} is missing")
    for old in out.glob(f"hicolor/*/apps/{APP_ID}.svg"):
        old.unlink()
        print(f"removed {old}")
    for size in SIZES:
        path = out / "hicolor" / f"{size}x{size}" / "apps" / f"{APP_ID}.png"
        path.parent.mkdir(parents=True, exist_ok=True)
        command = ["magick", str(master), "-filter", "Lanczos", "-resize", f"{size}x{size}"]
        if size <= SMALL:
            command += ["-channel", "RGB", "-sigmoidal-contrast", "2.5,45%",
                        "-unsharp", "0x0.6+1.0+0.0", "+channel"]
        command += ["-strip", "-define", "png:compression-level=9", f"PNG32:{path}"]
        subprocess.run(command, check=True)
        print(path)
    for empty in sorted(out.glob("hicolor/*/apps"), reverse=True):
        if not any(empty.iterdir()):
            empty.rmdir()
            empty.parent.rmdir()


if __name__ == "__main__":
    main()
