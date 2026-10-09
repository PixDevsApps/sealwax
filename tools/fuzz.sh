#!/usr/bin/env bash
# Usage: tools/fuzz.sh list | build | <target> [seconds] [libFuzzer options...]
#        tools/fuzz.sh all [seconds] [jobs] | repro <target> <file>
#
# Coverage-guided fuzzing (ML-093) on the stable toolchain, installing nothing: the targets in
# fuzz/ link libFuzzer through the libfuzzer-sys crate, and rustc instruments them with LLVM's
# SanitizerCoverage (-C passes=sancov-module). There is no AddressSanitizer (it needs nightly);
# Rust's own checks, debug assertions and overflow checks are on (fuzz/Cargo.toml), and a
# panic is a crash.
#
# <target> runs for [seconds] (default 600) at low priority, with a 10 s limit per input and
# 2 GiB of memory. The working corpus is target/fuzz-corpus/<target>, seeded from the
# checked-in fuzz/corpus/<target> and, where they exist, the repository's samples and the
# open-licensed templates in .local/corpus/real (never committed; SEALWAX_CORPUS_REAL names
# another folder, such as the main checkout's from a worktree). Crashes, hangs and slow
# inputs land in target/fuzz-artifacts/<target>/; the log in target/fuzz-logs/<target>.log.
# A crash found once goes into fuzz/corpus/<target>/, where `cargo test` keeps it fixed.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

targets=(message html text links attachment autoconfig search mailto config theme pop3 imap sieve)
triple=x86_64-unknown-linux-gnu
export CARGO_TARGET_DIR=$PWD/target/fuzz
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
real=${SEALWAX_CORPUS_REAL:-.local/corpus/real}
binary() { echo "$CARGO_TARGET_DIR/$triple/release/$1"; }

build() {
    # The flags cargo-fuzz uses with --sanitizer none. --target keeps them
    # off build scripts and proc macros.
    RUSTFLAGS="-Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=4 \
-Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-pc-table \
-Cllvm-args=-sanitizer-coverage-trace-compares --cfg fuzzing" \
        cargo build --manifest-path fuzz/Cargo.toml --release --locked --target "$triple" \
        --bins
}

# Seeds beyond the checked-in corpus, from files the repository (or .local) already has.
seed() {
    local target=$1 corpus=$2 file
    case $target in
        message)
            for file in spikes/s7-composer/fixtures/*.eml; do
                cp -- "$file" "$corpus/"
            done
            for file in "$real"/*.html; do
                [[ -f $file ]] || continue
                {
                    printf 'From: Fjord Weekly <news@example.test>\r\nSubject: Template\r\n'
                    printf 'MIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n'
                    cat -- "$file"
                } >"$corpus/real-$(basename -- "$file" .html).eml"
            done
            ;;
        html)
            for file in "$real"/*.html; do
                [[ -f $file ]] || continue
                cp -- "$file" "$corpus/"
            done
            ;;
        theme)
            for file in tests/theme/*/colors.toml; do
                cp -- "$file" "$corpus/$(basename -- "$(dirname -- "$file")").toml"
            done
            ;;
    esac
}

run() {
    local target=$1 seconds=${2:-600}
    shift $(($# < 2 ? $# : 2))
    local corpus=target/fuzz-corpus/$target artifacts=target/fuzz-artifacts/$target
    mkdir -p -- "$corpus" "$artifacts" target/fuzz-logs
    cp -- fuzz/corpus/"$target"/* "$corpus/"
    seed "$target" "$corpus"
    local max_len=16384
    [[ $target == message || $target == html ]] && max_len=131072
    nice -n 10 "$(binary "$target")" "$corpus" -max_total_time="$seconds" -timeout=10 \
        -rss_limit_mb=2048 -max_len="$max_len" -report_slow_units=2 -print_final_stats=1 \
        -artifact_prefix="$artifacts/" "$@" 2>&1 | tee "target/fuzz-logs/$target.log"
}

case ${1:-} in
    list) printf '%s\n' "${targets[@]}" ;;
    build) build ;;
    all)
        # Every target for [seconds] each, [jobs] at a time (default 4), quietly; prints each
        # target's final line and exit status.
        seconds=${2:-600} jobs=${3:-4}
        build
        for target in "${targets[@]}"; do
            while (($(jobs -rp | wc -l) >= jobs)); do
                wait -n || true
            done
            (
                status=0
                run "$target" "$seconds" >/dev/null 2>&1 || status=$?
                echo "$target: exit $status, $(grep -E '^(Done|==[0-9]+== ERROR|SUMMARY)' \
                    "target/fuzz-logs/$target.log" | tail -1)"
            ) &
        done
        wait
        ;;
    repro)
        [[ $# -eq 3 ]] || { echo "usage: tools/fuzz.sh repro <target> <file>" >&2; exit 2; }
        build
        "$(binary "$2")" "$3"
        ;;
    "" | -h | --help)
        sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
        ;;
    *)
        if [[ ! " ${targets[*]} " == *" $1 "* ]]; then
            echo "unknown target $1; tools/fuzz.sh list names them" >&2
            exit 2
        fi
        build
        run "$@"
        ;;
esac
