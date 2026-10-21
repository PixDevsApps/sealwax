#!/usr/bin/env python3
"""Write THIRD-PARTY-LICENSES.txt: the licence texts of the crates the sealwax binary is built from.

Usage: python3 tools/licenses.py [--output PATH] [--manifest-path Cargo.toml] [--offline]

The crates are the ones `cargo build -p sealwax` compiles for x86_64-unknown-linux-gnu: cargo tree
-p sealwax -e normal,build walks Cargo.lock's graph with the binary's own features, so
dev-dependencies, the spikes, devtools and fuzz, and features only they turn on, stay out.
Sealwax's own crates are left out; the vendored lettre is in. cargo metadata gives each crate's
licence expression and source folder, and the file holds every LICENSE*, LICENCE*, COPYING*,
COPYRIGHT*, NOTICE* and UNLICENSE* file (and LICENSES/) found there, byte-identical texts once
with the crates they belong to. A crate with no such file gets the standard text of its licence
from packaging/licenses/ (the script lists those crates); one whose licence has no text there
stops the script. The output is sorted, so it changes only when the dependencies do. Default
output: target/THIRD-PARTY-LICENSES.txt. Cargo downloads missing crates unless --offline.
"""
import argparse
import json
import pathlib
import re
import subprocess
import sys
import textwrap

root = pathlib.Path(__file__).resolve().parents[1]
BINARY = "sealwax"
TARGET = "x86_64-unknown-linux-gnu"
FALLBACKS = root / "packaging" / "licenses"
LICENSE_NAME = re.compile(r"(?i)^(licen[cs]e|copying|copyright|notice|unlicense)")
TREE_LINE = re.compile(r"^(\S+) v(\S+)((?: \([^)]*\))*)$")
COPYLEFT = re.compile(r"(?i)\b(A?GPL|LGPL|MPL|EPL|CDDL|EUPL|OSL|CC-BY-SA)")
WIDTH = 80
KEEP = "\0"


def cargo(manifest: pathlib.Path, offline: bool, *args: str) -> str:
    command = ["cargo", *args, "--manifest-path", str(manifest), "--locked"]
    if offline:
        command.append("--offline")
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout


def build_set(tree: str) -> set[tuple[str, str, str | None]]:
    """(name, version, local path or None) of every line of `cargo tree --prefix none --format {p}`."""
    crates = set()
    for line in tree.splitlines():
        line = line.strip()
        if not line:
            continue
        match = TREE_LINE.match(line)
        if not match:
            raise SystemExit(f"unexpected cargo tree line: {line}")
        notes = re.findall(r"\(([^)]*)\)", match.group(3))
        path = next((note for note in notes if note.startswith("/")), None)
        crates.add((match.group(1), match.group(2), path))
    return crates


def resolve(metadata: dict, built: set[tuple[str, str, str | None]]) -> list[dict]:
    """The metadata packages of `built`, without the workspace's own, sorted by name and version."""
    members = set(metadata["workspace_members"])
    by_key: dict[tuple[str, str], list[dict]] = {}
    for package in metadata["packages"]:
        by_key.setdefault((package["name"], package["version"]), []).append(package)
    found = {}
    for name, version, path in built:
        candidates = by_key.get((name, version), [])
        if path is not None:
            candidates = [
                package for package in candidates
                if pathlib.Path(package["manifest_path"]).parent == pathlib.Path(path)
            ]
        elif len(candidates) > 1:
            candidates = [package for package in candidates if package["source"]]
        if len(candidates) != 1:
            raise SystemExit(f"cargo metadata has {len(candidates)} packages for {name} {version}")
        package = candidates[0]
        if package["id"] not in members:
            found[package["id"]] = package
    return sorted(found.values(), key=lambda package: (package["name"], version_key(package["version"])))


def version_key(version: str) -> tuple:
    core, _, pre = version.partition("+")[0].partition("-")
    return tuple(int(part) if part.isdigit() else 0 for part in core.split(".")), pre == "", pre


def read_text(path: pathlib.Path) -> str:
    data = path.read_bytes()
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        text = data.decode("latin-1")
    return text.lstrip("﻿").replace("\r\n", "\n").replace("\r", "\n").rstrip() + "\n"


def license_files(package: dict) -> list[tuple[str, str]]:
    """(name relative to the crate's folder, text) of its licence files, sorted by name."""
    folder = pathlib.Path(package["manifest_path"]).parent
    paths = set()
    for entry in folder.iterdir():
        if not LICENSE_NAME.match(entry.name) or entry.suffix == ".rs":
            continue
        if entry.is_dir():
            paths.update(path for path in entry.rglob("*") if path.is_file())
        elif entry.is_file():
            paths.add(entry)
    if package.get("license_file"):
        named = folder / package["license_file"]
        if named.is_file():
            paths.add(named)
    files = [(path.relative_to(folder).as_posix(), read_text(path)) for path in paths]
    return sorted(files, key=lambda file: file[0])


def spdx_ids(expression: str) -> list[str]:
    """The licence identifiers in an SPDX expression (also the old "MIT/Apache-2.0" form)."""
    ids = []
    after_with = False
    for token in re.findall(r"[A-Za-z0-9.+-]+", expression.replace("/", " OR ")):
        if token in ("AND", "OR"):
            after_with = False
        elif token == "WITH":
            after_with = True
        elif not after_with and token not in ids:
            ids.append(token)
    return ids


def label(package: dict, space: str = " ") -> str:
    return f"{package['name']}{space}{package['version']}"


def authors(package: dict) -> str:
    names = [re.sub(r"\s*<[^>]*>", "", author).strip() for author in package.get("authors") or []]
    return ", ".join(name for name in names if name)


def wrap(text: str) -> str:
    return textwrap.fill(text, WIDTH, break_long_words=False, break_on_hyphens=False).replace(KEEP, " ")


def render(metadata: dict, packages: list[dict]) -> tuple[str, list[str], list[str]]:
    """The file's text, the crates given a standard text, and the crates under a copyleft or no licence."""
    workspace = pathlib.Path(metadata["workspace_root"])
    groups: dict[str, list[tuple[dict, str]]] = {}
    fallback_texts: dict[str, list[dict]] = {}
    fallbacks, flagged, rows = [], [], []
    for package in packages:
        expression = package.get("license") or ""
        if not expression or COPYLEFT.search(expression):
            flagged.append(f"{label(package)} ({expression or 'no licence expression'})")
        files = license_files(package)
        for name, text in files:
            groups.setdefault(text, []).append((package, name))
        if not files:
            ids = spdx_ids(expression)
            missing = [spdx for spdx in ids if not (FALLBACKS / f"{spdx}.txt").is_file()]
            if not ids or missing:
                raise SystemExit(
                    f"{label(package)} has no license file and packaging/licenses/ has no text for "
                    f"{', '.join(missing) or 'its licence'}"
                )
            for spdx in ids:
                fallback_texts.setdefault(spdx, []).append(package)
            fallbacks.append(f"{label(package)} ({expression})")
        note = ""
        if not package["source"]:
            location = pathlib.Path(package["manifest_path"]).parent
            try:
                location = location.relative_to(workspace)
            except ValueError:
                pass
            note = f"  [{location.as_posix()} in Sealwax's source]"
        rows.append((label(package), (expression or "see its license file") + note))

    sections = []
    for text, users in groups.items():
        by_file: dict[str, list[str]] = {}
        for package, name in users:
            by_file.setdefault(name, []).append(label(package, KEEP))
        heading = "; ".join(f"{name} of {', '.join(crates)}" for name, crates in sorted(by_file.items()))
        first = min((package["name"], version_key(package["version"]), name) for package, name in users)
        sections.append((first, heading, text))
    for spdx, users in fallback_texts.items():
        crates = ", ".join(
            label(package, KEEP) + (f" ({authors(package)})" if authors(package) else "") for package in users
        )
        heading = (
            f"The standard {spdx} text (packaging/licenses/{spdx}.txt) for crates whose source has "
            f"no license file: {crates}"
        )
        first = min((package["name"], version_key(package["version"]), spdx) for package in users)
        sections.append((first, heading, read_text(FALLBACKS / f"{spdx}.txt")))
    sections.sort(key=lambda section: (section[0], section[1]))

    width = max(len(name) for name, _ in rows) + 2
    lines = [
        "Third-party licenses of Sealwax",
        "",
        wrap(
            "Sealwax itself is under the MIT license (LICENSE, beside this file). It is built "
            f"from the {len(packages)} Rust crates listed below, each under its own license; "
            "their license, copyright and notice files follow, as found in each crate's source. "
            "A text that several crates ship byte for byte is printed once, with the crates it "
            "belongs to."
        ),
        "",
        wrap(
            "Made by tools/licenses.py from Cargo.lock: the crates the sealwax binary is built from "
            f"for {TARGET} (normal and build dependencies), without Sealwax's own. Each crate's "
            "source is on crates.io (https://crates.io/crates/<name>/<version>) unless marked."
        ),
        "",
        "Crates",
        "",
        *(f"{name.ljust(width)}{expression}".rstrip() for name, expression in rows),
    ]
    for _, heading, text in sections:
        lines += ["", "", "=" * WIDTH, wrap(heading), "=" * WIDTH, "", text.rstrip("\n")]
    return "\n".join(lines) + "\n", fallbacks, flagged


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--output", type=pathlib.Path, help="default: target/THIRD-PARTY-LICENSES.txt")
    parser.add_argument("--manifest-path", type=pathlib.Path, default=root / "Cargo.toml")
    parser.add_argument("--offline", action="store_true", help="don't let Cargo download crates")
    args = parser.parse_args()
    manifest = args.manifest_path.resolve()

    metadata = json.loads(cargo(
        manifest, args.offline, "metadata", "--format-version", "1", "--filter-platform", TARGET,
    ))
    tree = cargo(
        manifest, args.offline, "tree", "-p", BINARY, "-e", "normal,build", "--target", TARGET,
        "--prefix", "none", "--format", "{p}",
    )
    packages = resolve(metadata, build_set(tree))
    text, fallbacks, flagged = render(metadata, packages)

    output = args.output or pathlib.Path(metadata["target_directory"]) / "THIRD-PARTY-LICENSES.txt"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(text)
    print(f"{output}: {len(packages)} crates, {len(text.encode())} bytes")
    for line in fallbacks:
        print(f"standard text, no license file in the crate: {line}")
    for line in flagged:
        print(f"copyleft or no licence expression, check: {line}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
