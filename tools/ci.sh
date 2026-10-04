#!/usr/bin/env bash
# Usage: tools/ci.sh [--ui]. --ui adds a headless smoke run of the app and the self-test scripts
# in tests/selftest/ (except preview*), on Broadway display $SEALWAX_CI_DISPLAY (default 19).
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

ui=false
for arg in "$@"; do
    case $arg in
        --ui) ui=true ;;
        *)
            echo "usage: tools/ci.sh [--ui]" >&2
            exit 2
            ;;
    esac
done
display=${SEALWAX_CI_DISPLAY:-19}

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace

if $ui; then
    mkdir -p .local
    log=.local/ci-smoke.log
    # Scratch config, state and cache, so the smoke run never reads or writes the user's files.
    smoke=$(mktemp -d)
    trap 'rm -rf -- "$smoke"' EXIT
    tools/headless.sh "$display" env SEALWAX_EXIT_AFTER_MS=1500 XDG_CONFIG_HOME="$smoke/config" \
        XDG_STATE_HOME="$smoke/state" XDG_CACHE_HOME="$smoke/cache" XDG_DATA_HOME="$smoke/data" \
        target/debug/sealwax 2>&1 | tee "$log"
    if ! grep -q 'first-frame' "$log"; then
        echo "smoke test: the app never logged first-frame; see $log" >&2
        exit 1
    fi
    for script in tests/selftest/*.sealwax-test; do
        name=$(basename -- "$script" .sealwax-test)
        # The preview scripts write screenshots into tools/progress/ for a milestone's gate;
        # run them by hand.
        [[ $name == preview* ]] && continue
        log=.local/selftest-$name.log
        if ! tools/headless.sh "$display" target/debug/sealwax --self-test "$script" >"$log" 2>&1; then
            grep -v '^ok ' "$log" >&2 || true
            echo "self-test $name failed; see $log" >&2
            exit 1
        fi
        echo "self-test $name: $(grep '^# .* steps passed' "$log")"
    done
fi
