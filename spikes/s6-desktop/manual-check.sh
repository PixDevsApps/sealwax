#!/usr/bin/env bash
# S6 manual check: notification clicks on the live session (about one minute, at most three).
# Opens the "Mail spike S6" window over the cached INBOX with on-screen instructions, sends two
# low-urgency fictional notifications and records what a click does: whether the default action
# arrives, whether the window comes to the front with keyboard focus, and how long it takes until
# the message is painted. It injects nothing and ends by itself.
# Usage: bash spikes/s6-desktop/manual-check.sh
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
export CARGO_TARGET_DIR="$root/target/s6"
cargo build --release -p email-spike-s6 --manifest-path "$root/Cargo.toml"
[[ -f $root/target/s6/data/s6-inbox.sqlite ]] || "$root/target/s6/release/s6-bench" build-store
if gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
    --method org.freedesktop.DBus.NameHasOwner io.github.pixdevsapps.Mail.SpikeS6 | grep -q true; then
    echo "Mail spike S6 is already running; close it first." >&2
    exit 1
fi
scratch="$root/target/s6/xdg-manual"
mkdir -p "$scratch/config" "$scratch/cache" "$scratch/data" "$scratch/state" "$root/target/s6/manual"
log="$root/target/s6/manual/notify-$(date +%Y%m%d-%H%M%S).log"
echo "Events go to $log; instructions follow here and in the window."
XDG_CONFIG_HOME="$scratch/config" XDG_CACHE_HOME="$scratch/cache" XDG_DATA_HOME="$scratch/data" \
    XDG_STATE_HOME="$scratch/state" MAIL_S6_MODE=manual-notify MAIL_S6_WINDOWED=1 \
    MAIL_EXIT_AFTER_MS=180000 timeout --kill-after=5 200 \
    "$root/target/s6/release/email-spike-s6" >"$log"
