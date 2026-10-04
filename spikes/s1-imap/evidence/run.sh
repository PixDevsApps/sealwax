#!/usr/bin/env bash
# The S1 evidence run behind docs/spikes/S1-imap.md (output: 2026-10-04.log). It recreates the
# testbed (down.sh removes the volumes), seeds it and runs every measurement and suite.
# Build first: CARGO_TARGET_DIR=target/s1 cargo build --release -p email-spike-s1
# Run: spikes/s1-imap/evidence/run.sh > evidence.log 2>&1
set -uo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.."
B=target/s1/release
run() { echo; echo "\$ $*"; echo "# $(date -Is)"; "$@" 2>&1; echo "# exit $?"; }
echo "# S1 evidence run $(date -Is) on $(uname -r), $(nproc) CPUs, $(rustc --version)"
run bash tools/testbed/down.sh
run bash tools/testbed/up.sh
run bash tools/testbed/seed.sh
run python3 tools/testbed/check.py
for s in dovecot dovecot-rev1 stalwart; do run $B/s1 probe $s; done
for s in dovecot stalwart dovecot-rev1; do run $B/s1 first-sync $s --runs 4 --batch 500; done
for s in dovecot stalwart dovecot-rev1; do run $B/s1 first-sync $s --runs 2 --batch 500 --rtt-ms 50; done
for s in dovecot stalwart dovecot-rev1; do run $B/s1 fetch-only $s --runs 3; done
for s in dovecot stalwart dovecot-rev1; do run $B/s1-ioimap $s; done
for s in dovecot stalwart dovecot-rev1; do run $B/s1 resync $s --runs 20; done
for s in dovecot stalwart dovecot-rev1; do run $B/s1 resync $s --runs 10 --rtt-ms 50; done
for s in dovecot stalwart; do run $B/s1 idle-latency $s --samples 20; done
for s in dovecot dovecot-rev1 stalwart; do run $B/s1 random $s --seeds 1-10 --ops 1000; done
for s in dovecot stalwart; do run $B/s1 random $s --seeds 1-10 --ops 1000 --inbox-backstop no; done
run $B/s1 random dovecot --seeds 1-2 --ops 1000 --sabotage ignore-vanished
run $B/s1 random dovecot --seeds 1-2 --ops 1000 --sabotage ignore-vanished-no-count-check
for s in dovecot dovecot-rev1 stalwart; do run $B/s1 faults $s; done
echo "# done $(date -Is)"
