#!/usr/bin/env bash
# S7 manual check: opens the composer on the live session for up to five minutes with on-screen
# instructions (fcitx5, Norwegian layout, dead keys, Compose), times every key from press to the
# painted frame and prints the results. For the user to run; it injects nothing.
# Usage: bash spikes/s7-composer/manual-check.sh [--accelerated] [--minutes N]
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
flags=(--no-acceleration)
if [[ ${1:-} == --accelerated ]]; then
    flags=()
    shift
fi
CARGO_TARGET_DIR="$root/target/s7" cargo build --release -p email-spike-s7 --manifest-path "$root/Cargo.toml"
scratch="$root/.local/s7/xdg-manual"
mkdir -p "$scratch/cache" "$scratch/data" "$scratch/state"
MAIL_S7_ALLOW_LIVE=1 XDG_CACHE_HOME="$scratch/cache" XDG_DATA_HOME="$scratch/data" \
    XDG_STATE_HOME="$scratch/state" \
    "$root/target/s7/release/email-spike-s7" manual "${flags[@]}" "$@"
