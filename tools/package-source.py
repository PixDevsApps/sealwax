#!/usr/bin/env python3
"""Build a deterministic source tarball of the committed tree (HEAD) and a PKGBUILD for it.

Writes .local/package/sealwax-<version>.tar.gz and .local/package/PKGBUILD from
packaging/PKGBUILD.in, filling in the version, the tarball's SHA-256 and SOURCE_DATE_EPOCH
(the commit time of HEAD). Uncommitted changes are not packaged; the script says so. Building
the package (makepkg) and installing it need the user's authorization (AGENTS.md).
"""
import gzip
import hashlib
import pathlib
import subprocess
import sys
import tomllib

root = pathlib.Path(__file__).resolve().parents[1]


def git(*args: str) -> bytes:
    return subprocess.check_output(["git", *args], cwd=root)


def main() -> int:
    version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    commit = git("rev-parse", "HEAD").decode().strip()
    epoch = int(git("log", "-1", "--format=%ct", "HEAD").decode().strip())
    if git("status", "--porcelain", "--untracked-files=no").strip():
        print("warning: the working tree has uncommitted changes; only HEAD is packaged",
              file=sys.stderr)

    out = root / ".local" / "package"
    out.mkdir(parents=True, exist_ok=True)
    tar = git("archive", "--format=tar", f"--prefix=sealwax-{version}/", "HEAD")
    archive = out / f"sealwax-{version}.tar.gz"
    with archive.open("wb") as output, gzip.GzipFile(
        filename="", fileobj=output, mode="wb", mtime=0
    ) as compressed:
        compressed.write(tar)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()

    recipe = (root / "packaging" / "PKGBUILD.in").read_text()
    for placeholder, value in [
        ("@PKGVER@", version),
        ("@SHA256@", digest),
        ("@SOURCE_DATE_EPOCH@", str(epoch)),
    ]:
        if placeholder not in recipe:
            raise SystemExit(f"{placeholder} is missing from packaging/PKGBUILD.in")
        recipe = recipe.replace(placeholder, value)
    (out / "PKGBUILD").write_text(recipe)

    print(archive)
    print(f"commit {commit}")
    print(f"SHA256 {digest}")
    print(f"SOURCE_DATE_EPOCH {epoch}")
    print(f"Build: cd {out} && makepkg --cleanbuild --force (--nodeps where Rust comes from rustup; -si also installs; an agent needs authorization)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
