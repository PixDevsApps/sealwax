#!/usr/bin/env python3
"""Fetches M5's real-world corpus (ML-059): the open-licensed HTML email templates that
tests/corpus/real/SOURCES.toml lists, into .local/corpus/real/<id>.html (git-ignored), each
checked against its SHA-256. Anonymous HTTPS GETs of public files only, with a generic
User-Agent; nothing about the user is sent. Standard library only.

Usage: tools/corpus/fetch.py [--licences] [--force]
  --licences  also fetch each licence file once and check that it names the declared licence
  --force     download again even when a file with the right hash is already there
"""

import hashlib
import sys
import tomllib
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "tests/corpus/real/SOURCES.toml"
OUT = ROOT / ".local/corpus/real"
USER_AGENT = "mail-dev (automated)"
ALLOWED_HOST = "https://raw.githubusercontent.com/"
LICENCE_WORDS = {
    "MIT": "permission is hereby granted, free of charge",
    "Apache-2.0": "apache license",
}


def get(url: str) -> bytes:
    if not url.startswith(ALLOWED_HOST):
        raise ValueError(f"not a raw.githubusercontent.com URL: {url}")
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=30) as response:
        return response.read()


def main() -> int:
    args = set(sys.argv[1:])
    unknown = args - {"--licences", "--force"}
    if unknown:
        print(__doc__, file=sys.stderr)
        return 2
    templates = tomllib.loads(MANIFEST.read_text())["template"]
    OUT.mkdir(parents=True, exist_ok=True)
    fetched = kept = failed = 0
    for template in templates:
        path = OUT / f"{template['id']}.html"
        if "--force" not in args and path.is_file():
            if hashlib.sha256(path.read_bytes()).hexdigest() == template["sha256"]:
                kept += 1
                continue
        try:
            data = get(template["url"])
        except (OSError, ValueError) as error:
            print(f"FAIL {template['id']}: {error}")
            failed += 1
            continue
        digest = hashlib.sha256(data).hexdigest()
        if digest != template["sha256"] or len(data) != template["bytes"]:
            print(f"FAIL {template['id']}: sha256 {digest} ({len(data)} bytes) doesn't match")
            failed += 1
            continue
        path.write_bytes(data)
        fetched += 1
    if "--licences" in args:
        for url, licence in sorted({(t["licence_url"], t["licence"]) for t in templates}):
            words = LICENCE_WORDS.get(licence)
            try:
                text = get(url).decode("utf-8", "replace").lower()
            except (OSError, ValueError) as error:
                print(f"LICENCE FAIL {url}: {error}")
                failed += 1
                continue
            found = words is not None and words in text
            print(f"LICENCE {'ok' if found else 'CHECK'} {licence} {url}")
            failed += 0 if found else 1
    print(
        f"corpus: {len(templates)} templates, {fetched} fetched, {kept} already there, "
        f"{failed} failed, in {OUT.relative_to(ROOT)}"
    )
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
