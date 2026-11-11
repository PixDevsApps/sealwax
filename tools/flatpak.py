#!/usr/bin/env python3
"""Sealwax's Flatpak: build it from the commit tagged v<version>, check it, and make the signed
Flatpak repository people install it from (ADR-027). Three steps, in order:

  python3 tools/flatpak.py build    flatpak-builder on `git archive HEAD` with the release manifest
                                    (packaging/flatpak/io.github.pixdevsapps.Sealwax.yml), the
                                    Microsoft client ID as tools/package-source.py finds it; the
                                    result goes into an unsigned repository of its own,
                                    .local/flatpak/release/<version>/repo, and is checked
  python3 tools/flatpak.py site     that commit signed into the published repository,
                                    .local/flatpak/site/repo, with static deltas and the AppStream
                                    data, plus sealwax.flatpakrepo, the app's .flatpakref and an
                                    index.html beside it
  python3 tools/flatpak.py keygen   the repository's signing key, once, in .local/flatpak/gpg
                                    (never committed; back it up, a lost key means a new remote)

build needs HEAD to be the commit tagged v<version> and the working tree clean (--untagged builds
HEAD anyway, for a test). site needs SEALWAX_FLATPAK_URL, the address the site is served from,
ending in "/". Publishing the site is the owner's act.
"""
import base64
import hashlib
import importlib.util
import os
import pathlib
import shutil
import subprocess
import sys
import tomllib

root = pathlib.Path(__file__).resolve().parents[1]
APP_ID = "io.github.pixdevsapps.Sealwax"
BRANCH = "stable"
MANIFEST = root / "packaging" / "flatpak" / f"{APP_ID}.yml"
WORK = root / ".local" / "flatpak"
GPG_HOME = WORK / "gpg"
SITE = WORK / "site"
KEY_NAME = "Sealwax Flatpak repository"
RUNTIME_REPO = "https://dl.flathub.org/repo/flathub.flatpakrepo"


def run(*args: str, cwd: pathlib.Path = root, env: dict | None = None) -> None:
    print("+", " ".join(args), flush=True)
    subprocess.run(args, cwd=cwd, check=True, env=env)


def output(*args: str, cwd: pathlib.Path = root) -> str:
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def version() -> str:
    return tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]


def release_dir(ver: str) -> pathlib.Path:
    return WORK / "release" / ver


def check_head(ver: str) -> None:
    if "--untagged" in sys.argv[2:]:
        return
    tag = f"v{ver}"
    if output("git", "status", "--porcelain", "--untracked-files=no"):
        raise SystemExit("the working tree has uncommitted changes")
    try:
        tagged = output("git", "rev-parse", f"{tag}^{{commit}}")
    except subprocess.CalledProcessError:
        raise SystemExit(f"no tag {tag}: tag the version commit first (or --untagged)")
    if tagged != output("git", "rev-parse", "HEAD"):
        raise SystemExit(f"HEAD isn't {tag}")


def client_id() -> tuple[str, str]:
    spec = importlib.util.spec_from_file_location("package_source", root / "tools" / "package-source.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.client_id()


def build(ver: str) -> None:
    check_head(ver)
    out = release_dir(ver)
    if out.exists():
        shutil.rmtree(out)
    src = out / "src"
    src.mkdir(parents=True)
    archive = subprocess.run(["git", "archive", "--format=tar", "HEAD"], cwd=root, check=True,
                             capture_output=True).stdout
    subprocess.run(["tar", "-x", "-C", str(src)], input=archive, check=True)
    run(sys.executable, "-I", "tools/flatpak-cargo-sources.py", "--check", cwd=src)

    # The client ID goes into a copy of the manifest beside the sources, never into git.
    value, source = client_id()
    print(f"Microsoft client ID: {source}")
    manifest = (src / MANIFEST.relative_to(root)).read_text()
    anchor = "    CARGO_NET_OFFLINE: 'true'\n"
    if manifest.count(anchor) != 1:
        raise SystemExit(f"{MANIFEST.name}: build-options.env has no CARGO_NET_OFFLINE line")
    if value:
        manifest = manifest.replace(anchor, anchor + f"    SEALWAX_MICROSOFT_CLIENT_ID: '{value}'\n")
    build_manifest = src / "packaging" / "flatpak" / f"{APP_ID}.build.yml"
    build_manifest.write_text(manifest)

    run(
        "flatpak-builder", "--force-clean",
        f"--state-dir={WORK / 'state'}", f"--default-branch={BRANCH}",
        f"--repo={out / 'repo'}", str(out / "build"), str(build_manifest),
    )
    check(ver)


def check(ver: str) -> None:
    files = release_dir(ver) / "build" / "files"
    metainfo = files / "share" / "metainfo" / f"{APP_ID}.metainfo.xml"
    desktop = files / "share" / "applications" / f"{APP_ID}.desktop"
    for path in (metainfo, desktop, files / "bin" / "sealwax"):
        if not path.exists():
            raise SystemExit(f"the build has no {path.relative_to(files)}")
    run("appstreamcli", "validate", "--no-net", "--explain", str(metainfo))
    if f'<release version="{ver}"' not in metainfo.read_text():
        raise SystemExit(f"{metainfo.name} has no <release> for {ver}")
    run("desktop-file-validate", str(desktop))
    data = (files / "bin" / "sealwax").read_bytes()
    for marker in (b"--self-test", b"SEALWAX_SELFTEST_ALLOW_LIVE", b"alex.berg@example.test",
                   str(pathlib.Path.home()).encode()):
        if marker in data:
            raise SystemExit(f"/app/bin/sealwax holds {marker.decode()!r}")
    reported = output("flatpak-builder", "--version")
    print(f"checked: {metainfo.name}, {desktop.name}, /app/bin/sealwax ({len(data):,} bytes); {reported}")


def gpg(*args: str) -> str:
    return output("gpg", "--homedir", str(GPG_HOME), "--batch", *args)


def key_id() -> str:
    if not GPG_HOME.exists():
        raise SystemExit("no signing key: python3 tools/flatpak.py keygen")
    for line in gpg("--with-colons", "--list-secret-keys").splitlines():
        fields = line.split(":")
        if fields[0] == "fpr":
            return fields[9]
    raise SystemExit(f"no secret key in {GPG_HOME}")


def keygen() -> None:
    if GPG_HOME.exists() and any(GPG_HOME.iterdir()):
        raise SystemExit(f"{GPG_HOME} already holds a key; remove it by hand to start over")
    GPG_HOME.mkdir(parents=True, exist_ok=True)
    GPG_HOME.chmod(0o700)
    gpg("--passphrase", "", "--quick-gen-key", KEY_NAME, "rsa4096", "sign", "never")
    print(f"signing key {key_id()} in {GPG_HOME}: back the folder up somewhere safe")


def public_key() -> str:
    exported = subprocess.check_output(
        ["gpg", "--homedir", str(GPG_HOME), "--batch", "--export", key_id()]
    )
    return base64.b64encode(exported).decode()


def site(ver: str) -> None:
    url = os.environ.get("SEALWAX_FLATPAK_URL", "")
    if not url.startswith("https://") or not url.endswith("/"):
        raise SystemExit("SEALWAX_FLATPAK_URL must be the site's https:// address, ending in /")
    source = release_dir(ver) / "repo"
    if not source.exists():
        raise SystemExit(f"run build first: {source} is missing")
    fingerprint = key_id()
    signing = [f"--gpg-sign={fingerprint}", f"--gpg-homedir={GPG_HOME}"]
    repo = SITE / "repo"
    if not repo.exists():
        run("ostree", "init", "--mode=archive-z2", f"--repo={repo}")
    ref = f"app/{APP_ID}/x86_64/{BRANCH}"
    run("flatpak", "build-commit-from", *signing, f"--src-repo={source}", str(repo), ref)
    run(
        "flatpak", "build-update-repo", *signing, "--generate-static-deltas", "--prune",
        "--prune-depth=3", "--title=Sealwax", f"--homepage=https://github.com/PixDevsApps/sealwax",
        f"--default-branch={BRANCH}", str(repo),
    )
    key = public_key()
    (SITE / "sealwax.flatpakrepo").write_text(
        "[Flatpak Repo]\n"
        "Title=Sealwax\n"
        f"Url={url}repo/\n"
        "Homepage=https://github.com/PixDevsApps/sealwax\n"
        "Comment=Sealwax, the email client, from its own repository\n"
        f"DefaultBranch={BRANCH}\n"
        f"GPGKey={key}\n"
    )
    (SITE / f"{APP_ID}.flatpakref").write_text(
        "[Flatpak Ref]\n"
        "Title=Sealwax\n"
        f"Name={APP_ID}\n"
        f"Branch={BRANCH}\n"
        f"Url={url}repo/\n"
        "SuggestRemoteName=sealwax\n"
        "IsRuntime=false\n"
        f"RuntimeRepo={RUNTIME_REPO}\n"
        f"GPGKey={key}\n"
    )
    (SITE / "index.html").write_text(index_html(url, ver))
    (SITE / ".nojekyll").write_text("")
    digest = hashlib.sha256(key.encode()).hexdigest()[:16]
    print(f"site for {ver} in {SITE}; key {fingerprint} (base64 sha256 {digest}); serve it at {url}")


def index_html(url: str, ver: str) -> str:
    ref = f"{url}{APP_ID}.flatpakref"
    return f"""<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sealwax Flatpak repository</title>
<style>
  body {{ font: 16px/1.5 system-ui, sans-serif; max-width: 42rem; margin: 2rem auto; padding: 0 1rem; }}
  pre {{ background: #8881; padding: .75rem; overflow-x: auto; }}
</style>
<h1>Sealwax Flatpak repository</h1>
<p>Sealwax {ver}, the email client, as a Flatpak for Linux distributions other than Omarchy and Arch.
On Omarchy, install the Arch package instead: see
<a href="https://github.com/PixDevsApps/sealwax/blob/main/INSTALL.md">INSTALL.md</a>.</p>
<p>Install it, and the GNOME runtime it needs from Flathub, with one line:</p>
<pre>flatpak install --user {ref}</pre>
<p>Updates then come with <code>flatpak update</code>, or from GNOME Software or KDE Discover.
The repository on its own: <a href="sealwax.flatpakrepo">sealwax.flatpakrepo</a>.</p>
"""


def main() -> int:
    commands = {"build": build, "check": check, "site": site, "keygen": keygen}
    if len(sys.argv) < 2 or sys.argv[1] not in commands:
        print(__doc__, file=sys.stderr)
        return 2
    command = commands[sys.argv[1]]
    if command is keygen:
        keygen()
    else:
        command(version())
    return 0


if __name__ == "__main__":
    sys.exit(main())
