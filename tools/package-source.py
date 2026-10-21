#!/usr/bin/env python3
"""Build a deterministic source tarball of the committed tree (HEAD) and a PKGBUILD for it.

Writes .local/package/sealwax-<version>.tar.gz, .local/package/THIRD-PARTY-LICENSES.txt (made
by the tarball's own tools/licenses.py from its Cargo.lock) and .local/package/PKGBUILD from
packaging/PKGBUILD.in, filling in the version, both files' SHA-256, SOURCE_DATE_EPOCH (the
commit time of HEAD) and the Microsoft client ID the package builds in: SEALWAX_MICROSOFT_CLIENT_ID
if set, else the builder's ~/.config/sealwax/config.toml ([oauth.microsoft] client_id), else none
(--no-client-id leaves it out). The ID is a public client's, not a secret, and never committed:
only the generated PKGBUILD under .local/ holds it, and this script doesn't print it.
Uncommitted changes are not packaged; the script says so. Building the package (makepkg) and
installing it need the user's authorization (AGENTS.md).
"""
import gzip
import hashlib
import io
import os
import pathlib
import re
import subprocess
import sys
import tarfile
import tempfile
import tomllib

root = pathlib.Path(__file__).resolve().parents[1]


def git(*args: str) -> bytes:
    return subprocess.check_output(["git", *args], cwd=root)


CLIENT_ID = re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")


def client_id() -> tuple[str, str]:
    """The Microsoft client ID to build in and where it came from; ("", why) for none."""
    if "--no-client-id" in sys.argv[1:]:
        return "", "left out (--no-client-id)"
    from_env = os.environ.get("SEALWAX_MICROSOFT_CLIENT_ID", "").strip()
    if from_env:
        source, value = "SEALWAX_MICROSOFT_CLIENT_ID", from_env
    else:
        home = os.environ.get("XDG_CONFIG_HOME") or os.path.join(os.path.expanduser("~"), ".config")
        config = pathlib.Path(home) / "sealwax" / "config.toml"
        try:
            value = tomllib.loads(config.read_text()).get("oauth", {}).get("microsoft", {}).get(
                "client_id", ""
            ).strip()
        except (OSError, tomllib.TOMLDecodeError):
            value = ""
        if not value:
            return "", "none (Sign in with Microsoft then needs config.toml's client_id)"
        source = str(config)
    if not CLIENT_ID.fullmatch(value):
        raise SystemExit(f"the Microsoft client ID from {source} isn't a client ID (a GUID)")
    return value.lower(), f"built in, from {source}"


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

    licenses = out / "THIRD-PARTY-LICENSES.txt"
    with tempfile.TemporaryDirectory() as tree:
        with tarfile.open(fileobj=io.BytesIO(tar)) as source:
            source.extractall(tree, filter="data")
        packaged = pathlib.Path(tree) / f"sealwax-{version}"
        subprocess.run(
            [sys.executable, str(packaged / "tools" / "licenses.py"), "--output", str(licenses)],
            check=True,
        )
    licenses_digest = hashlib.sha256(licenses.read_bytes()).hexdigest()

    microsoft, microsoft_source = client_id()
    recipe = (root / "packaging" / "PKGBUILD.in").read_text()
    for placeholder, value in [
        ("@PKGVER@", version),
        ("@SHA256@", digest),
        ("@LICENSES_SHA256@", licenses_digest),
        ("@SOURCE_DATE_EPOCH@", str(epoch)),
        ("@MICROSOFT_CLIENT_ID@", microsoft),
    ]:
        if placeholder not in recipe:
            raise SystemExit(f"{placeholder} is missing from packaging/PKGBUILD.in")
        recipe = recipe.replace(placeholder, value)
    (out / "PKGBUILD").write_text(recipe)

    print(archive)
    print(f"commit {commit}")
    print(f"SHA256 {digest}")
    print(licenses)
    print(f"SHA256 {licenses_digest}")
    print(f"SOURCE_DATE_EPOCH {epoch}")
    print(f"Microsoft client ID: {microsoft_source}")
    print(f"Build: cd {out} && makepkg --cleanbuild --force (--nodeps where Rust comes from rustup; -si also installs; an agent needs authorization)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
