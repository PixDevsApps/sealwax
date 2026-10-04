#!/usr/bin/env python3
"""Startup with a cached INBOX, measured as S6 measured it: process start (the launcher's
CLOCK_MONOTONIC just before the spawn, passed as SEALWAX_EXEC_NS) to the first painted frame
(`first-frame-from-exec`). The first window waits for the list's first rows (≤ 300 ms), so the
frame has rows unless the log says the wait ran out. S6 measured painted → on screen at about
12 ms more (Vulkan); that part isn't included here.

Every run uses scratch config, data, state and cache folders and a scratch Omarchy theme
(matte-black from tests/theme/) under target/startup-live/; the desktop's theme is never read
or changed. Runs open a fullscreen window that closes itself after SEALWAX_EXIT_AFTER_MS.

    cargo build --release -p sealwax -p sealwax-fixtures
    python3 tools/live/startup.py [--runs 21] [--count 10000] [--headless DISPLAY]

On the live session this needs the user's live-window authorization (PLAN.md's log). With
--headless it runs through tools/headless.sh as a dry run (Broadway; not the bar).
"""

import argparse
import os
import pathlib
import re
import shutil
import statistics
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRATCH = ROOT / "target" / "startup-live"


def percentile(values, p):
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, round(p / 100 * len(ordered) + 0.5) - 1))
    return ordered[index]


def prepare(count):
    data = SCRATCH / "data" / "sealwax"
    store = data / "store.sqlite"
    marker = SCRATCH / f"store-{count}"
    if not marker.exists():
        shutil.rmtree(SCRATCH / "data", ignore_errors=True)
        data.mkdir(parents=True)
        subprocess.run(
            [ROOT / "target/release/sealwax-fixtures", "import", "--count", str(count),
             "--seed", "1", "--store", store],
            check=True)
        for old in SCRATCH.glob("store-*"):
            old.unlink()
        marker.touch()
    theme = SCRATCH / "omarchy" / "current"
    if not (theme / "theme.name").exists():
        (theme / "theme").mkdir(parents=True, exist_ok=True)
        shutil.copy(ROOT / "tests/theme/matte-black/colors.toml", theme / "theme/colors.toml")
        shutil.copy(ROOT / "tests/theme/shell.toml", theme / "theme/shell.toml")
        (theme / "theme.name").write_text("matte-black\n")
    for folder in ("config", "state", "cache"):
        (SCRATCH / folder).mkdir(parents=True, exist_ok=True)


def run_once(headless):
    env = dict(os.environ)
    env.update({
        "XDG_CONFIG_HOME": str(SCRATCH / "config"),
        "XDG_DATA_HOME": str(SCRATCH / "data"),
        "XDG_STATE_HOME": str(SCRATCH / "state"),
        "XDG_CACHE_HOME": str(SCRATCH / "cache"),
        "SEALWAX_OMARCHY_STATE": str(SCRATCH / "omarchy"),
        "SEALWAX_EXIT_AFTER_MS": "1500",
        "SEALWAX_START_FULLSCREEN": "1",
        "RUST_LOG": "info,sealwax_app::application=debug",
    })
    command = [str(ROOT / "target/release/sealwax")]
    if headless is not None:
        command = [str(ROOT / "tools/headless.sh"), str(headless)] + command
    env["SEALWAX_EXEC_NS"] = str(time.clock_gettime_ns(time.CLOCK_MONOTONIC))
    done = subprocess.run(command, env=env, capture_output=True, text=True, timeout=60)
    log = done.stdout + done.stderr
    exec_ms = re.search(r"first-frame-from-exec ([0-9.]+)", log)
    main_ms = re.search(r"first-frame ([0-9.]+)", log)
    no_rows = "stopped waiting for the list's first rows" in log
    return (float(exec_ms.group(1)) if exec_ms else None,
            float(main_ms.group(1)) if main_ms else None, no_rows, done.returncode)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--runs", type=int, default=21)
    parser.add_argument("--count", type=int, default=10000)
    parser.add_argument("--headless", type=int)
    args = parser.parse_args()
    prepare(args.count)
    results = []
    for index in range(args.runs):
        exec_ms, main_ms, no_rows, status = run_once(args.headless)
        print(f"run {index + 1}: from exec {exec_ms} ms, from main {main_ms} ms, "
              f"rows waited out {no_rows}, exit {status}", flush=True)
        results.append((exec_ms, main_ms, no_rows))
        time.sleep(0.5)
    kept = [r for r in results[1:] if r[0] is not None]
    if not kept:
        sys.exit("no run logged first-frame-from-exec")
    exec_values = [r[0] for r in kept]
    main_values = [r[1] for r in kept if r[1] is not None]
    print(f"kept {len(kept)} of {len(results)} runs (the first is discarded); "
          f"rows waited out in {sum(r[2] for r in kept)}")
    print(f"process start → first painted frame: p50 {statistics.median(exec_values):.1f} ms, "
          f"p95 {percentile(exec_values, 95):.1f} ms, max {max(exec_values):.1f} ms")
    print(f"main() → first painted frame: p50 {statistics.median(main_values):.1f} ms, "
          f"p95 {percentile(main_values, 95):.1f} ms")


if __name__ == "__main__":
    main()
