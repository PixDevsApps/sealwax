#!/usr/bin/env bash
# The kill -9 exit criterion (PRD C5: a crash while composing loses at most 5 s of typing).
# Usage: tools/kill9-compose.sh <display> [runs] (default 10). Build first: cargo build.
# Each run starts Sealwax headless on a scratch store (.local/kill9/run-N), opens a composer
# on the real store backend with a local, server-less account (drafts save; nothing can be
# sent), types a character every 200 ms through the editor's helper, kills the process with
# SIGKILL 4-20 s after the typing started, starts Sealwax again on the same folders, and reads
# the draft that reopened. Loss: the time from the kill back to when the last character the
# reopened draft holds was typed. Not in tools/ci.sh: it takes about 40 s a run.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

display=${1:?usage: tools/kill9-compose.sh <display> [runs]}
runs=${2:-10}
base=.local/kill9
rm -rf -- "$base"
mkdir -p "$base"
printf 'run\tkilled_after_s\ttyped\trecovered\tlost_chars\tlost_s\tprefix\n' | tee "$base/summary.tsv"
for run in $(seq 1 "$runs"); do
    dir=$base/run-$run
    root=$PWD/$dir/root
    mkdir -p "$root"
    SEALWAX_SELFTEST_ROOT=$root SEALWAX_HEADLESS_TIMEOUT=240 \
        tools/headless.sh "$display" target/debug/sealwax --self-test tests/kill9/type.sealwax-test \
        >"$dir/type.log" 2>&1 &
    wrapper=$!
    log=$root/typed.log
    for _ in $(seq 600); do
        if [[ -f $log ]] && (($(wc -l <"$log") > 2)); then
            break
        fi
        sleep 0.05
    done
    if [[ ! -f $log ]]; then
        echo "run $run: the typing never started; see $dir/type.log" >&2
        kill "$wrapper" 2>/dev/null || true
        wait "$wrapper" 2>/dev/null || true
        exit 1
    fi
    pid=$(awk 'NR == 1 { print $2 }' "$log")
    started=$(awk 'NR == 2 { print $2 }' "$log")
    delay_ms=$((4000 + RANDOM % 16001))
    sleep "$(printf '%d.%03d' $((delay_ms / 1000)) $((delay_ms % 1000)))"
    killed=$(date +%s%3N)
    kill -9 "$pid"
    wait "$wrapper" 2>/dev/null || true
    if ! SEALWAX_SELFTEST_ROOT=$root SEALWAX_HEADLESS_TIMEOUT=120 \
        tools/headless.sh "$display" target/debug/sealwax --self-test tests/kill9/check.sealwax-test \
        >"$dir/check.log" 2>&1; then
        echo "run $run: the draft didn't reopen; see $dir/check.log" >&2
        exit 1
    fi
    python3 tools/kill9-loss.py "$log" "$root/recovered.txt" "$killed" "$started" "$run" |
        tee -a "$base/summary.tsv"
done
python3 tools/kill9-loss.py --summary "$base/summary.tsv"
