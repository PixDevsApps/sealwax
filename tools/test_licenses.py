#!/usr/bin/env python3
"""Tests for tools/licenses.py on a small offline workspace: python3 tools/test_licenses.py"""
import pathlib
import subprocess
import sys
import tempfile
import textwrap
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent / "licenses.py"
NOTICE = "NOTICE: builddep includes code by Example Corp.\n"
SHARED = "Copyright (c) 2026 Shared Author\n\nPermission is granted.\n"


def write(path: pathlib.Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(textwrap.dedent(text).lstrip())


def crate(folder: pathlib.Path, name: str, license: str, deps: str = "", files: dict | None = None) -> None:
    write(folder / "Cargo.toml", f"""
        [package]
        name = "{name}"
        version = "0.1.0"
        edition = "2021"
        license = "{license}"
        authors = ["{name} Author <{name}@example.invalid>"]
        {deps}
    """)
    write(folder / "src" / "lib.rs", "")
    for file, text in (files or {}).items():
        (folder / file).write_text(text)


class Walk(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        ws = pathlib.Path(cls.tmp.name)
        vendor = ws / "vendor"
        write(ws / "Cargo.toml", """
            [workspace]
            members = ["app", "member", "spike"]
            exclude = ["vendor"]
            resolver = "2"
        """)
        crate(ws / "app", "sealwax", "MIT", deps="""
            [dependencies]
            member = { path = "../member" }
            libdep = { path = "../vendor/libdep" }
            nofile = { path = "../vendor/nofile" }
            [build-dependencies]
            builddep = { path = "../vendor/builddep" }
            [dev-dependencies]
            devdep = { path = "../vendor/devdep" }
            [target.'cfg(windows)'.dependencies]
            windep = { path = "../vendor/windep" }
        """)
        crate(ws / "member", "member", "MIT")
        crate(ws / "spike", "spike", "MIT", deps="""
            [dependencies]
            libdep = { path = "../vendor/libdep", features = ["extra"] }
        """)
        crate(vendor / "libdep", "libdep", "MIT", deps="""
            [dependencies]
            shared = { path = "../shared" }
            featdep = { path = "../featdep", optional = true }
            [features]
            extra = ["dep:featdep"]
        """, files={"LICENSE": SHARED})
        crate(vendor / "shared", "shared", "MIT OR Apache-2.0", files={"LICENSE-MIT": SHARED})
        crate(vendor / "builddep", "builddep", "Apache-2.0", files={
            "LICENSE-APACHE": "Apache text of builddep\n", "NOTICE": NOTICE,
        })
        crate(vendor / "nofile", "nofile", "MIT")
        for name in ("devdep", "windep", "featdep"):
            crate(vendor / name, name, "MIT", files={"LICENSE": f"{name} license\n"})
        manifest = ws / "Cargo.toml"
        subprocess.run(
            ["cargo", "generate-lockfile", "--offline", "--manifest-path", str(manifest)],
            check=True, capture_output=True,
        )
        cls.output = ws / "out.txt"
        cls.result = subprocess.run(
            [sys.executable, str(SCRIPT), "--offline", "--manifest-path", str(manifest),
             "--output", str(cls.output)],
            check=True, capture_output=True, text=True,
        )
        cls.text = cls.output.read_text()
        cls.listed = {
            line.split()[0]
            for line in cls.text.split("\nCrates\n\n", 1)[1].split("\n\n", 1)[0].splitlines()
        }

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_normal_and_build_dependencies_only(self):
        self.assertEqual(self.listed, {"builddep", "libdep", "nofile", "shared"})

    def test_no_dev_platform_or_feature_only_dependencies(self):
        for name in ("devdep", "windep", "featdep"):
            self.assertNotIn(f"{name} license", self.text)

    def test_workspace_crates_left_out(self):
        self.assertNotIn("member", self.listed)
        self.assertNotIn("sealwax", self.listed)
        self.assertNotIn("spike", self.listed)

    def test_notice_kept(self):
        self.assertIn("NOTICE of builddep 0.1.0", self.text)
        self.assertIn(NOTICE.strip(), self.text)

    def test_identical_texts_once(self):
        self.assertEqual(self.text.count("Shared Author"), 1)
        self.assertIn("LICENSE of libdep 0.1.0; LICENSE-MIT of shared 0.1.0", self.text)

    def test_standard_text_for_crate_without_license_file(self):
        self.assertIn("standard text, no license file in the crate: nofile 0.1.0 (MIT)", self.result.stdout)
        self.assertIn("packaging/licenses/MIT.txt) for crates whose source has no license file: "
                      "nofile 0.1.0 (nofile Author)", " ".join(self.text.split()))
        self.assertNotIn("example.invalid", self.text)

    def test_vendored_crates_marked(self):
        self.assertIn("vendor/libdep in Sealwax's source", self.text)

    def test_deterministic(self):
        again = pathlib.Path(self.tmp.name) / "again.txt"
        subprocess.run(
            [sys.executable, str(SCRIPT), "--offline", "--manifest-path",
             str(pathlib.Path(self.tmp.name) / "Cargo.toml"), "--output", str(again)],
            check=True, capture_output=True,
        )
        self.assertEqual(again.read_text(), self.text)


if __name__ == "__main__":
    unittest.main()
