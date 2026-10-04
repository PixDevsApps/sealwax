#!/usr/bin/env bash
# Opens Sealwax on the live desktop over a fictional mailbox, for the user's own checks at a
# preview gate: `omarchy theme set`, `omarchy font set`, `omarchy display text size` and the
# corners, seen in a running window. Run it yourself; it is not run by agents.
#
# Data, state and cache are scratch folders under target/try/ (two fictional accounts, dated
# around today). The configuration home stays the real one, so fontconfig sees the Omarchy font;
# Sealwax only reads ~/.config/sealwax/ if it exists. The theme is read live and never changed.
# Close the window to quit.
#
#   bash tools/live/try.sh            # reuse the mailbox in target/try/
#   bash tools/live/try.sh --fresh    # import a new one first
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
scratch=$root/target/try
cargo build --release --manifest-path "$root/Cargo.toml" -p sealwax -p sealwax-fixtures

if [[ ${1:-} == --fresh ]]; then
  rm -rf "$scratch"
fi
data=$scratch/data/sealwax
if [[ ! -e $data/store.sqlite ]]; then
  mkdir -p "$data"
  for account in "Personal 2000 7" "Work 400 8"; do
    read -r name count seed <<<"$account"
    "$root/target/release/sealwax-fixtures" import --store "$data/store.sqlite" \
      --count "$count" --seed "$seed" --now now --folders spread --account "$name"
  done
fi
mkdir -p "$scratch/state" "$scratch/cache"

exec env XDG_DATA_HOME="$scratch/data" XDG_STATE_HOME="$scratch/state" \
  XDG_CACHE_HOME="$scratch/cache" "$root/target/release/sealwax"
