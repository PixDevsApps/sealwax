#!/usr/bin/env bash
# Usage: tools/ci.sh [--ui] [--testbed]. --ui adds a headless smoke run of the app, the
# self-test scripts in tests/selftest/ (except preview*, perf-*, live-* and testbed-*) and the
# hostile-HTML suite with the DNS canary (app/examples/hostile.rs), on
# Broadway display $SEALWAX_CI_DISPLAY (default 19). --testbed adds every testbed suite: each
# test file with tests ignored as "needs the testbed", in release, one test at a time. When no
# testbed container is running it starts and seeds the testbed first and stops it at the end
# (keeping its volumes); a running testbed is used and left running.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

ui=false
testbed=false
for arg in "$@"; do
    case $arg in
        --ui) ui=true ;;
        --testbed) testbed=true ;;
        *)
            echo "usage: tools/ci.sh [--ui] [--testbed]" >&2
            exit 2
            ;;
    esac
done
display=${SEALWAX_CI_DISPLAY:-19}

smoke=
stop_testbed=false
cleanup() {
    if [[ -n $smoke ]]; then
        rm -rf -- "$smoke"
    fi
    if $stop_testbed; then
        tools/testbed/down.sh --keep-volumes
    fi
}
trap cleanup EXIT

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
python3 tools/test_licenses.py

if $ui; then
    mkdir -p .local
    log=.local/ci-smoke.log
    # Scratch config, state and cache, so the smoke run never reads or writes the user's files.
    smoke=$(mktemp -d)
    # WebKit's sandbox switch is set for the smoke run and every self-test: Sealwax clears it
    # at start and warns without its value (M9); assert-web-sandbox checks the web processes.
    sandbox_off=WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=ci-sandbox-value
    tools/headless.sh "$display" env "$sandbox_off" SEALWAX_EXIT_AFTER_MS=1500 \
        XDG_CONFIG_HOME="$smoke/config" XDG_STATE_HOME="$smoke/state" \
        XDG_CACHE_HOME="$smoke/cache" XDG_DATA_HOME="$smoke/data" \
        target/debug/sealwax 2>&1 | tee "$log"
    if ! grep -q 'first-frame' "$log"; then
        echo "smoke test: the app never logged first-frame; see $log" >&2
        exit 1
    fi
    if ! grep -q 'WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS was set' "$log" ||
        grep -q 'ci-sandbox-value' "$log"; then
        echo "smoke test: no warning about WebKit's sandbox switch, or one with its value" >&2
        exit 1
    fi
    for script in tests/selftest/*.sealwax-test; do
        name=$(basename -- "$script" .sealwax-test)
        # The preview scripts write screenshots into tools/progress/ for a milestone's gate, and
        # the perf-* scripts need a release build and large fixture stores, the live-* scripts
        # are meant for the live session, and the testbed-* scripts need the Docker mail
        # testbed; run them by hand.
        [[ $name == preview* || $name == perf-* || $name == live-* || $name == testbed-* ]] && continue
        log=.local/selftest-$name.log
        if ! tools/headless.sh "$display" env "$sandbox_off" target/debug/sealwax \
            --self-test "$script" >"$log" 2>&1; then
            grep -v '^ok ' "$log" >&2 || true
            echo "self-test $name failed; see $log" >&2
            exit 1
        fi
        echo "self-test $name: $(grep '^# .* steps passed' "$log")"
    done
    # The hostile-HTML suite (ML-065) against the real reader, with the DNS canary
    # (devtools/dns-canary) preloaded: every name the app or WebKit looks up is recorded and
    # refused. It fails when a pass that must be clean isn't, or when a control sees nothing.
    cargo build -p sealwax --example hostile
    log=.local/ci-hostile.log
    started=$SECONDS
    if ! SEALWAX_HEADLESS_TIMEOUT=900 tools/headless.sh "$display" env \
        LD_PRELOAD="$PWD/target/debug/libsealwax_dns_canary.so" \
        SEALWAX_DNS_CANARY_LOG="$PWD/.local/ci-hostile-dns.log" \
        target/debug/examples/hostile >"$log" 2>&1; then
        grep -E '^(SUMMARY|MATRIX|CID)' "$log" >&2 || true
        echo "hostile suite failed; see $log" >&2
        exit 1
    fi
    echo "hostile suite: $(grep '^SUMMARY production_failures' "$log" | cut -c9-)," \
        "$((SECONDS - started)) s"
fi

if $testbed; then
    mkdir -p .local
    services=$(cd tools/testbed && docker compose config --services | sort)
    running=$(cd tools/testbed && docker compose ps --status running --services | sort)
    if [[ -z $running ]]; then
        tools/testbed/up.sh
        stop_testbed=true
        tools/testbed/seed.sh
    elif [[ $running != "$services" ]]; then
        echo "testbed: only some containers are running; start it with tools/testbed/up.sh" >&2
        exit 1
    fi
    # The suites' proxies need the server certificate as well as the CA.
    if [[ ! -f tools/testbed/certs/server.pem ]]; then
        echo "testbed: tools/testbed/certs/ isn't in this checkout (a worktree?); link it to the" \
            "certs/ of the checkout that started the testbed" >&2
        exit 1
    fi
    export SEALWAX_TESTBED_CA=${SEALWAX_TESTBED_CA:-$PWD/tools/testbed/certs/ca.pem}
    export MAIL_TESTBED_CA=${MAIL_TESTBED_CA:-$SEALWAX_TESTBED_CA}
    # The suites share test users, so they run one file and one test at a time. The soak
    # (ML-049) and the tests that need a Secret Service or the network are ignored for other
    # reasons and stay out.
    mapfile -t suites < <(grep -l 'ignore = "needs the testbed' -- */tests/*.rs spikes/*/tests/*.rs | sort -u)
    for file in "${suites[@]}"; do
        crate=${file%%/tests/*}
        package=$(sed -n 's/^name = "\(.*\)"$/\1/p' "$crate/Cargo.toml" | head -n 1)
        name=$(basename -- "$file" .rs)
        log=.local/testbed-$package-$name.log
        if ! cargo test --release -p "$package" --test "$name" -- --ignored --test-threads=1 \
            --nocapture >"$log" 2>&1; then
            grep -E '^(test |thread |failures)|panicked' "$log" >&2 || true
            echo "testbed suite $package/$name failed; see $log" >&2
            exit 1
        fi
        echo "testbed suite $package/$name: $(grep '^test result' "$log" | tail -n 1)"
    done
fi
