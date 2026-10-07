#!/usr/bin/env python3
"""Match Cargo.lock against a local checkout of the RustSec advisory database.

Usage: python3 tools/advisories.py [-v] <advisory-db> [Cargo.lock]

`cargo audit` would be an install; this reads the public database
(https://github.com/rustsec/advisory-db, cloned by the caller, never committed)
and prints every advisory whose package and version match a locked crate (-v: also the
advisories that name a locked crate at versions it isn't at).
Exit status 1 when a vulnerability (not an informational notice) matches.
"""

import re
import sys
import tomllib
from pathlib import Path


def parse_version(text):
    text = text.strip()
    core, _, pre = text.partition("-")
    core = core.partition("+")[0]
    parts = core.split(".")
    nums = []
    for part in parts:
        if part in ("*", "x", "X"):
            break
        nums.append(int(part))
    return nums, pre


def key(nums, pre):
    padded = (nums + [0, 0, 0])[:3]
    # A pre-release sorts before its release.
    return (*padded, 0 if pre else 1, pre)


def comparator(op, version):
    nums, pre = parse_version(version)
    low = key(nums, pre)

    def bump(index):
        upper = (nums + [0, 0, 0])[:3]
        upper[index] += 1
        for i in range(index + 1, 3):
            upper[i] = 0
        return (*upper, 0, "")

    if op in ("", "^"):
        if not nums:
            return [lambda v: True]
        if nums[0] > 0 or len(nums) == 1:
            high = bump(0)
        elif len(nums) == 2 or nums[1] > 0:
            high = bump(1)
        else:
            high = bump(2)
        return [lambda v: v >= low, lambda v: v < high]
    if op == "~":
        high = bump(0) if len(nums) == 1 else bump(1)
        return [lambda v: v >= low, lambda v: v < high]
    if op == "=":
        if len(nums) == 3:
            return [lambda v: v == low]
        high = bump(len(nums) - 1)
        return [lambda v: v >= low, lambda v: v < high]
    if op == ">=":
        return [lambda v: v >= low]
    if op == ">":
        if len(nums) < 3:
            high = bump(len(nums) - 1)
            return [lambda v: v >= high]
        return [lambda v: v > low]
    if op == "<=":
        if len(nums) < 3:
            high = bump(len(nums) - 1)
            return [lambda v: v < high]
        return [lambda v: v <= low]
    if op == "<":
        return [lambda v: v < low]
    raise ValueError(f"unknown operator {op!r}")


COMPARATOR = re.compile(r"\s*(>=|<=|>|<|=|\^|~)?\s*([0-9][0-9A-Za-z.*+-]*|\*)\s*")


def matches(requirement, version):
    v = key(*parse_version(version))
    for part in requirement.split(","):
        m = COMPARATOR.fullmatch(part)
        if not m:
            raise ValueError(f"cannot read requirement {requirement!r}")
        op, ver = m.group(1) or "", m.group(2)
        if ver == "*":
            continue
        if not all(check(v) for check in comparator(op, ver)):
            return False
    return True


def advisories(db):
    for path in sorted((db / "crates").glob("*/*.md")):
        text = path.read_text(encoding="utf-8")
        m = re.search(r"```toml\n(.*?)\n```", text, re.S)
        if not m:
            continue
        data = tomllib.loads(m.group(1))
        title = next(
            (line[2:].strip() for line in text[m.end():].splitlines() if line.startswith("# ")),
            "",
        )
        yield data, title


def main():
    args = sys.argv[1:]
    verbose = "-v" in args
    args = [arg for arg in args if arg != "-v"]
    if not args:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    db = Path(args[0])
    lock = tomllib.loads(Path(args[1] if len(args) > 1 else "Cargo.lock").read_text())
    locked = {}
    for package in lock["package"]:
        if "source" in package:
            locked.setdefault(package["name"], set()).add(package["version"])

    found = []
    checked = 0
    for data, title in advisories(db):
        advisory = data["advisory"]
        name = advisory["package"]
        if name not in locked:
            continue
        checked += 1
        versions = data.get("versions", {})
        safe = versions.get("patched", []) + versions.get("unaffected", [])
        for version in sorted(locked[name]):
            if any(matches(req, version) for req in safe):
                if verbose:
                    print(f"  not affected: {advisory['id']}  {name} {version}  {title}")
                continue
            found.append((advisory, version, title, versions.get("patched", [])))
    print(f"{checked} advisories name a locked crate")

    vulnerable = 0
    for advisory, version, title, patched in found:
        kind = advisory.get("informational") or "vulnerability"
        if advisory.get("withdrawn"):
            kind = f"withdrawn {advisory['withdrawn']}"
        elif kind == "vulnerability":
            vulnerable += 1
        fixed = ", ".join(patched) or "no fixed release"
        print(f"{advisory['id']}  {advisory['package']} {version}  [{kind}]  {title}  (patched: {fixed})")
    print(f"{len(lock['package'])} locked packages, {len(found)} matches, {vulnerable} vulnerabilities")
    return 1 if vulnerable else 0


if __name__ == "__main__":
    sys.exit(main())
