#!/usr/bin/env python3
"""Make a Sealwax release from the commit tagged v<version> (ADR-021). Three steps, in order:

  python3 tools/release.py build    the Arch package, a tarball of its files for the AUR, the
                                    package's THIRD-PARTY-LICENSES.txt and their SHA-256 sums, in
                                    .local/release/<version>/
  python3 tools/release.py github   the GitHub release v<version> with those files and the notes
                                    from CHANGELOG.md
  python3 tools/release.py aur      sealwax-bin's PKGBUILD and .SRCINFO for <version>, committed
                                    and pushed to the AUR

The version is Cargo.toml's. HEAD must be the commit tagged v<version>, the working tree clean,
and CHANGELOG.md must have a "## <version>" section. build runs tools/package-source.py and
makepkg (the Microsoft client ID as package-source.py finds it). github needs the tag pushed;
aur needs the owner's AUR account and SSH key (SEALWAX_AUR_SSH_KEY names the key file if ssh
doesn't find it). Publishing is the owner's act: an agent runs github and aur only on the
owner's word.
"""
import gzip
import hashlib
import io
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib

root = pathlib.Path(__file__).resolve().parents[1]
AUR_REPO = "ssh://aur@aur.archlinux.org/sealwax-bin.git"
AUR_TEMPLATE = root / "packaging" / "aur" / "sealwax-bin" / "PKGBUILD.in"
STABLE_NAME = "sealwax-x86_64.pkg.tar.zst"


def run(*args: str, cwd: pathlib.Path = root, env: dict | None = None) -> None:
    subprocess.run(args, cwd=cwd, check=True, env=env)


def output(*args: str, cwd: pathlib.Path = root) -> str:
    return subprocess.check_output(args, cwd=cwd).decode().strip()


def version() -> str:
    return tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]


def release_dir(ver: str) -> pathlib.Path:
    return root / ".local" / "release" / ver


def files(ver: str) -> list[pathlib.Path]:
    out = release_dir(ver)
    return [
        out / f"sealwax-{ver}-1-x86_64.pkg.tar.zst",
        out / STABLE_NAME,
        out / f"sealwax-{ver}-x86_64.tar.gz",
        out / "THIRD-PARTY-LICENSES.txt",
        out / "SHA256SUMS",
    ]


def notes(ver: str) -> str:
    """CHANGELOG.md's section for `ver`, without its heading."""
    text = (root / "CHANGELOG.md").read_text()
    match = re.search(rf"^## {re.escape(ver)}\b.*?\n(.*?)(?=^## |\Z)", text, re.M | re.S)
    if not match or not match.group(1).strip():
        raise SystemExit(f'CHANGELOG.md has no "## {ver}" section')
    return match.group(1).strip()


def check_release(ver: str) -> None:
    tag = f"v{ver}"
    if output("git", "status", "--porcelain", "--untracked-files=no"):
        raise SystemExit("the working tree has uncommitted changes")
    tagged = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"{tag}^{{commit}}"],
        cwd=root, capture_output=True, text=True,
    ).stdout.strip()
    if tagged != output("git", "rev-parse", "HEAD"):
        raise SystemExit(f"HEAD isn't the commit tagged {tag}")
    notes(ver)


def package_list(text: str, name: str) -> list[str]:
    match = re.search(rf"^{name}=\((.*?)\)", text, re.M | re.S)
    if not match:
        raise SystemExit(f"no {name}=(…) in a PKGBUILD")
    return re.findall(r"'([^']*)'", match.group(1))


def build(ver: str) -> None:
    check_release(ver)
    run(sys.executable, "tools/package-source.py")
    package_dir = root / ".local" / "package"
    makepkg = ["makepkg", "--cleanbuild", "--force"]
    # Rust from rustup's own installer isn't a pacman package (INSTALL.md).
    if subprocess.run(["pacman", "-T", "rust", "cargo"], capture_output=True).returncode != 0:
        makepkg.append("--nodeps")
    run(*makepkg, cwd=package_dir)
    package = package_dir / f"sealwax-{ver}-1-x86_64.pkg.tar.zst"
    out = release_dir(ver)
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    named, stable, tarball, licenses, sums = files(ver)
    shutil.copy2(package, named)
    shutil.copy2(package, stable)
    shutil.copy2(package_dir / licenses.name, licenses)

    # The package's own files (usr/…) as a plain tarball, for sealwax-bin: owned by root, in
    # name order, with the package's times, so the same package gives the same bytes.
    buffer = io.BytesIO()
    with tarfile.open(package, "r:zst") as source, tarfile.open(
        fileobj=buffer, mode="w", format=tarfile.GNU_FORMAT
    ) as target:
        members = sorted(
            (member for member in source.getmembers() if member.name.startswith("usr")),
            key=lambda member: member.name,
        )
        if not any(member.name == "usr/bin/sealwax" for member in members):
            raise SystemExit(f"{package.name} has no usr/bin/sealwax")
        for member in members:
            member.uid = member.gid = 0
            member.uname = member.gname = "root"
            data = source.extractfile(member) if member.isfile() else None
            target.addfile(member, data)
    with tarball.open("wb") as raw, gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0) as gz:
        gz.write(buffer.getvalue())

    lines = [
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}"
        for path in (named, stable, tarball, licenses)
    ]
    sums.write_text("\n".join(lines) + "\n")
    for path in (named, stable, tarball, licenses, sums):
        print(path)


def github(ver: str) -> None:
    check_release(ver)
    tag = f"v{ver}"
    missing = [path for path in files(ver) if not path.exists()]
    if missing:
        raise SystemExit(f"run build first: {', '.join(path.name for path in missing)} missing")
    if not output("git", "ls-remote", "--tags", "origin", f"refs/tags/{tag}"):
        raise SystemExit(f"{tag} isn't on origin yet: git push origin {tag}")
    body = notes(ver) + (
        "\n\n---\n\n**Install, update or uninstall:** see "
        "[INSTALL.md](https://github.com/PixDevsApps/sealwax/blob/main/INSTALL.md), "
        "in plain words.\n"
    )
    with tempfile.NamedTemporaryFile("w", suffix=".md", delete=False) as handle:
        handle.write(body)
        notes_file = handle.name
    try:
        run(
            "gh", "release", "create", tag, "--verify-tag", "--title", f"Sealwax {ver}",
            "--notes-file", notes_file, *(str(path) for path in files(ver)),
        )
    finally:
        os.unlink(notes_file)


def aur(ver: str) -> None:
    check_release(ver)
    tarball = files(ver)[2]
    if not tarball.exists():
        raise SystemExit("run build first: the AUR tarball is missing")
    template = AUR_TEMPLATE.read_text()
    source = (root / "packaging" / "PKGBUILD.in").read_text()
    for name in ("depends", "optdepends"):
        if package_list(template, name) != package_list(source, name):
            raise SystemExit(f"{AUR_TEMPLATE.relative_to(root)}'s {name} differs from packaging/PKGBUILD.in's")
    recipe = template.replace("@PKGVER@", ver).replace(
        "@SHA256@", hashlib.sha256(tarball.read_bytes()).hexdigest()
    )
    env = dict(os.environ)
    key = os.environ.get("SEALWAX_AUR_SSH_KEY")
    if key:
        env["GIT_SSH_COMMAND"] = f"ssh -i {key} -o IdentitiesOnly=yes"
    checkout = root / ".local" / "aur" / "sealwax-bin"
    if checkout.exists():
        run("git", "pull", "--ff-only", cwd=checkout, env=env)
    else:
        checkout.parent.mkdir(parents=True, exist_ok=True)
        run("git", "clone", AUR_REPO, str(checkout), env=env)
    (checkout / "PKGBUILD").write_text(recipe)
    # The release's tarball must be what the checksum says, downloaded as an AUR user would.
    run("makepkg", "--verifysource", cwd=checkout)
    (checkout / tarball.name).unlink(missing_ok=True)
    srcinfo = output("makepkg", "--printsrcinfo", cwd=checkout)
    (checkout / ".SRCINFO").write_text(srcinfo + "\n")
    run("git", "add", "PKGBUILD", ".SRCINFO", cwd=checkout)
    run("git", "commit", "-m", f"Update to {ver}", cwd=checkout)
    run("git", "push", "origin", "HEAD:master", cwd=checkout, env=env)


def main() -> int:
    steps = {"build": build, "github": github, "aur": aur}
    if len(sys.argv) != 2 or sys.argv[1] not in steps:
        print(__doc__, file=sys.stderr)
        return 2
    steps[sys.argv[1]](version())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
