#!/usr/bin/env bash
# Stops and removes the mail testbed containers and network. Volumes (test mail only) are removed
# too unless --keep-volumes is given; images stay.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

volumes=(--volumes)
for arg in "$@"; do
    case $arg in
        --keep-volumes) volumes=() ;;
        *)
            echo "usage: tools/testbed/down.sh [--keep-volumes]" >&2
            exit 2
            ;;
    esac
done

docker compose down --remove-orphans "${volumes[@]}"
