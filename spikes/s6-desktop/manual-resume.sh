#!/usr/bin/env bash
# S6 manual check: reconnecting after suspend and resume (up to 15 minutes). Starts the local
# mail testbed if it isn't running (and stops it again at the end), runs Mail spike S6 in
# background mode with an IDLE connection to the testbed's Stalwart, and prints logind's
# PrepareForSleep signals, network changes and the time from resume until INBOX is synced and
# idling again (bar: 10 s). You suspend and wake the machine yourself; Ctrl+C ends the check.
# Usage: bash spikes/s6-desktop/manual-resume.sh
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
export CARGO_TARGET_DIR="$root/target/s6"
cargo build --release -p email-spike-s6 --manifest-path "$root/Cargo.toml"
[[ -f $root/target/s6/data/s6-inbox.sqlite ]] || "$root/target/s6/release/s6-bench" build-store
started_testbed=false
if ! (exec 3<>/dev/tcp/127.0.0.1/16993) 2>/dev/null; then
    "$root/tools/testbed/up.sh"
    started_testbed=true
fi
cleanup() {
    if $started_testbed; then
        "$root/tools/testbed/down.sh"
    fi
}
trap cleanup EXIT
scratch="$root/target/s6/xdg-manual"
mkdir -p "$scratch/config" "$scratch/cache" "$scratch/data" "$scratch/state" "$root/target/s6/manual"
log="$root/target/s6/manual/resume-$(date +%Y%m%d-%H%M%S).log"
echo "Events go to $log."
status=0
XDG_CONFIG_HOME="$scratch/config" XDG_CACHE_HOME="$scratch/cache" XDG_DATA_HOME="$scratch/data" \
    XDG_STATE_HOME="$scratch/state" MAIL_S6_MODE=manual-resume MAIL_S6_WINDOWED=1 \
    MAIL_S6_IMAP_SERVER=stalwart MAIL_S6_IMAP_PORT=16993 MAIL_S6_IMAP_USER=spike \
    MAIL_EXIT_AFTER_MS=900000 timeout --kill-after=5 920 \
    "$root/target/s6/release/email-spike-s6" >"$log" || status=$?
grep -E '"event":"(prepare-for-sleep|reconnected|network-changed)"' "$log" || true
exit "$status"
