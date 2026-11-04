#!/usr/bin/env bash
# The clean-container install test (ML-095; the owner's word on 2026-10-21, sign-off sheet D14):
# installs a Sealwax package with pacman in a fresh Arch Linux container, so the package is shown
# to pull in every library it needs on a system that never had Sealwax, Rust or the build tools,
# then starts the installed /usr/bin/sealwax headless (its first frame) and checks it holds no
# test code. The released binary has no self-test runner (ADR-025 amendment), so the smoke script
# (tests/selftest/install-smoke.sealwax-test: the window over a fictional mailbox, a message in
# the reader, the reader's web process in WebKit's sandbox) runs with sealwax-selftest, built from
# the same sources, on the container's libraries.
#
#   bash tools/install-test.sh PACKAGE [IMAGE]
#
# PACKAGE is a .pkg.tar.zst (tools/release.py build puts one in .local/release/<version>/);
# IMAGE defaults to the local archlinux:base, which isn't pulled again. SEALWAX_SELFTEST_BIN is
# the self-test binary (default target/debug/sealwax-selftest: cargo build -p sealwax-uitest).
# The container is removed afterwards; its logs and screenshot go to .local/install-test/<time>/.
# It runs with seccomp and Docker's masked /proc paths lifted for this container only, so
# bubblewrap can make WebKit's sandbox (user namespaces and a /proc of its own) as on a desktop.
# Needs a running Docker daemon and network for pacman's mirrors. Nothing of the user's goes in:
# no account, no mail, no home folder.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

if [[ ${1:-} == --inside ]]; then
    # In the container, as root.
    pacman -Syu --noconfirm --needed dbus
    pacman -U --noconfirm /test/sealwax.pkg.tar.zst
    sealwax --version
    for binary in /usr/bin/sealwax /test/sealwax-selftest; do
        missing=$(ldd "$binary" | grep 'not found' || true)
        if [[ -n $missing ]]; then
            echo "libraries $binary needs that aren't installed:" >&2
            echo "$missing" >&2
            exit 1
        fi
    done
    pacman -Qlq sealwax | grep '^/usr/share/licenses/sealwax/'
    if grep -qaF -e '--self-test' -e 'SEALWAX_SELFTEST_ALLOW_LIVE' -e 'alex.berg@example.test' \
        /usr/bin/sealwax; then
        echo "/usr/bin/sealwax holds self-test code" >&2
        exit 1
    fi
    useradd --create-home tester
    work=/home/tester/sealwax
    install -d -o tester "$work" "$work/tests"
    cp -r /test/tools "$work/"
    cp -r /test/selftest "$work/tests/"
    chown -R tester "$work"
    # The display's socket goes in the runtime folder, which a container has none of.
    runtime=/run/user/$(id -u tester)
    install -d -m 700 -o tester "$runtime"
    status=0
    runuser -u tester -- env -C "$work" XDG_RUNTIME_DIR="$runtime" SEALWAX_BROADWAYD=gtk4-broadwayd \
        SEALWAX_HEADLESS_TIMEOUT=60 SEALWAX_EXIT_AFTER_MS=3000 tools/headless.sh 9 /usr/bin/sealwax \
        >/test/out/start.log 2>&1 || status=$?
    if ((status != 0)) || ! grep -q 'first-frame' /test/out/start.log; then
        echo "/usr/bin/sealwax didn't start (exit $status):" >&2
        cat /test/out/start.log >&2
        chown -R "$HOST_UID" /test/out
        exit 1
    fi
    echo "/usr/bin/sealwax: first frame"
    runuser -u tester -- env -C "$work" XDG_RUNTIME_DIR="$runtime" SEALWAX_BROADWAYD=gtk4-broadwayd \
        SEALWAX_HEADLESS_TIMEOUT=180 tools/headless.sh 9 /test/sealwax-selftest \
        --self-test tests/selftest/install-smoke.sealwax-test || status=$?
    cp "$work/.local/selftest/install-smoke.png" /test/out/ 2>/dev/null || true
    chown -R "$HOST_UID" /test/out
    exit "$status"
fi

if (($# < 1)) || [[ ! -f $1 ]]; then
    echo "usage: $0 PACKAGE.pkg.tar.zst [IMAGE]" >&2
    exit 2
fi
package=$(realpath -- "$1")
image=${2:-archlinux:base}
selftest=$(realpath -- "${SEALWAX_SELFTEST_BIN:-$root/target/debug/sealwax-selftest}")
if [[ ! -x $selftest ]]; then
    echo "no $selftest: cargo build -p sealwax-uitest (or set SEALWAX_SELFTEST_BIN)" >&2
    exit 2
fi
out="$root/.local/install-test/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$out"
smoke=$(mktemp -d)
trap 'rm -rf -- "$smoke"' EXIT
cp "$root/tests/selftest/install-smoke.sealwax-test" "$smoke/"

echo "package: $package" | tee "$out/install-test.log"
echo "self-test binary: $selftest ($("$selftest" --version))" | tee -a "$out/install-test.log"
status=0
docker run --rm --pull never --name "sealwax-install-test-$$" \
    --security-opt seccomp=unconfined --security-opt systempaths=unconfined \
    -v "$package:/test/sealwax.pkg.tar.zst:ro" \
    -v "$selftest:/test/sealwax-selftest:ro" \
    -v "$root/tools:/test/tools:ro" \
    -v "$smoke:/test/selftest:ro" \
    -v "$out:/test/out" -e HOST_UID="$(id -u)" \
    "$image" bash /test/tools/install-test.sh --inside 2>&1 | tee -a "$out/install-test.log" ||
    status=$?
echo "exit $status; log in $out" | tee -a "$out/install-test.log"
exit "$status"
