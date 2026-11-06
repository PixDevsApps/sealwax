#!/usr/bin/env python3
"""Write packaging/flatpak/cargo-sources.json from Cargo.lock, for flatpak-builder.

A Flatpak build has no network, so every crate Cargo.lock names is listed as a source that
flatpak-builder downloads beforehand: the crate's archive from static.crates.io, checked against
the lock file's SHA-256, unpacked into cargo/vendor/<name>-<version> with the
.cargo-checksum.json Cargo's directory sources need, and a cargo/config.toml that points
crates.io at that folder. The build sets CARGO_HOME to the module's cargo/ folder. The output is
the same as flatpak-builder-tools' flatpak-cargo-generator.py writes for registry crates; this
one needs only the standard library and makes no network request. The crates Sealwax vendors
(vendor/, [patch.crates-io]) are path dependencies and come with the source tree.

Usage: tools/flatpak-cargo-sources.py [--check]   (--check: fail if the file is out of date)
"""
import json
import pathlib
import sys
import tomllib

root = pathlib.Path(__file__).resolve().parents[1]
lock_file = root / "Cargo.lock"
output = root / "packaging" / "flatpak" / "cargo-sources.json"

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
VENDOR = "cargo/vendor"
CONFIG = f"""[source.vendored-sources]
directory = "{VENDOR}"

[source.crates-io]
replace-with = "vendored-sources"
"""


def sources(lock: dict) -> list[dict]:
    out: list[dict] = []
    for package in sorted(lock["package"], key=lambda p: (p["name"], p["version"])):
        source = package.get("source")
        if source is None:
            continue
        if source != CRATES_IO:
            raise SystemExit(f"{package['name']} {package['version']}: unsupported source {source}")
        name, version, checksum = package["name"], package["version"], package["checksum"]
        dest = f"{VENDOR}/{name}-{version}"
        out.append(
            {
                "type": "archive",
                "archive-type": "tar-gzip",
                "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                "sha256": checksum,
                "dest": dest,
            }
        )
        out.append(
            {
                "type": "inline",
                "contents": json.dumps({"package": checksum, "files": {}}),
                "dest": dest,
                "dest-filename": ".cargo-checksum.json",
            }
        )
    out.append({"type": "inline", "contents": CONFIG, "dest": "cargo", "dest-filename": "config.toml"})
    return out


def main() -> int:
    with lock_file.open("rb") as f:
        lock = tomllib.load(f)
    text = json.dumps(sources(lock), indent=4) + "\n"
    if "--check" in sys.argv[1:]:
        if not output.is_file() or output.read_text() != text:
            print(f"{output.relative_to(root)} is out of date: run tools/flatpak-cargo-sources.py")
            return 1
        return 0
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(text)
    crates = sum(1 for s in json.loads(text) if s["type"] == "archive")
    print(f"wrote {output.relative_to(root)}: {crates} crates")
    return 0


if __name__ == "__main__":
    sys.exit(main())
