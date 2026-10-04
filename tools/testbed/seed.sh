#!/usr/bin/env bash
# Seeds the running testbed (users, folders incl. Norwegian and non-ASCII names, a small mixed
# mailbox for alice and 10,000 synthetic messages for perf). Idempotent; see seed.py --help.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
exec python3 seed.py "$@"
