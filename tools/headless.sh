#!/usr/bin/env bash
# Runs a command against a private GTK Broadway display and a private D-Bus session,
# so nothing opens on the live Wayland display. Usage: headless.sh <display-number> <cmd...>
# MAIL_HEADLESS_TIMEOUT (seconds, default 300) bounds the command's run time.
set -euo pipefail

if (($# < 2)) || [[ ! $1 =~ ^[0-9]+$ ]]; then
    echo "usage: $0 <display-number> <cmd...>" >&2
    exit 2
fi
display=$1
shift
timeout_s=${MAIL_HEADLESS_TIMEOUT:-300}

# A second broadwayd on a busy display would unlink the first one's socket, so refuse early.
if (exec 3<>"/dev/tcp/127.0.0.1/$((8080 + display))") 2>/dev/null; then
    echo "Broadway display :$display (port $((8080 + display))) is already in use" >&2
    exit 1
fi

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
mkdir -p "$root/.local"
log="$root/.local/broadway-$display.log"
gtk4-broadwayd --address 127.0.0.1 ":$display" >"$log" 2>&1 &
broadwayd=$!

cleanup() {
    if kill -0 "$broadwayd" 2>/dev/null; then
        kill "$broadwayd" 2>/dev/null || true
        wait "$broadwayd" 2>/dev/null || true
    fi
    # broadwayd leaves its socket behind when killed; GLib's runtime dir falls back to the cache dir.
    rm -f -- "${XDG_RUNTIME_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}}/broadway$((display + 1)).socket"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for _ in $(seq 100); do
    grep -q '^Listening on' "$log" && break
    if ! kill -0 "$broadwayd" 2>/dev/null; then
        echo "gtk4-broadwayd :$display failed to start (display in use?):" >&2
        cat "$log" >&2
        exit 1
    fi
    sleep 0.05
done
if ! grep -q '^Listening on' "$log"; then
    echo "gtk4-broadwayd :$display did not become ready" >&2
    exit 1
fi

status=0
env -u WAYLAND_DISPLAY -u DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE \
    GDK_BACKEND=broadway BROADWAY_DISPLAY=":$display" \
    GTK_A11Y=none ADW_DISABLE_PORTAL=1 GSETTINGS_BACKEND=memory GIO_USE_VFS=local \
    dbus-run-session --config-file="$root/tools/headless-dbus.conf" -- \
    timeout --kill-after=5 "$timeout_s" "$@" || status=$?
exit "$status"
