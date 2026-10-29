#!/usr/bin/env bash
# Opens Sealwax on the live desktop over a fictional mailbox, for the user's own checks at a
# preview gate: `omarchy theme set`, `omarchy font set`, `omarchy display text size` and the
# corners, seen in a running window. Run it yourself; it is not run by agents.
#
# Data, state and cache are scratch folders under target/try/ (two fictional accounts, dated
# around today). The configuration home is target/try/config, with the real omarchy, fontconfig
# and gtk-4.0 folders linked in, so the theme and font are live and never changed, and your
# accounts.toml, config.toml and keys.toml are never read.
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
mkdir -p "$scratch/state" "$scratch/cache" "$scratch/config"
# A configuration home of its own, so this window never sees your real accounts.toml (it would
# list your accounts, asking to sign in); only what the look needs is linked from the real one.
real_config=${XDG_CONFIG_HOME:-$HOME/.config}
for linked in omarchy fontconfig gtk-4.0; do
  if [[ -e $real_config/$linked && ! -e $scratch/config/$linked ]]; then
    ln -s "$real_config/$linked" "$scratch/config/$linked"
  fi
done
# An app-id of its own: with the installed id, a launch asks the session bus for the installed
# Sealwax's service (background mode), which would show its window, over your real mail, instead.
exec env SEALWAX_APP_ID=io.github.pixdevsapps.Sealwax.Try \
  XDG_CONFIG_HOME="$scratch/config" XDG_DATA_HOME="$scratch/data" XDG_STATE_HOME="$scratch/state" \
  XDG_CACHE_HOME="$scratch/cache" "$root/target/release/sealwax"
