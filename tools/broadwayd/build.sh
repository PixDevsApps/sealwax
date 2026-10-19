#!/usr/bin/env bash
# Builds GTK's gtk4-broadwayd with reuse.patch for the installed GTK and prints its path; prints
# nothing when the installed GTK doesn't need it (before 4.24). tools/headless.sh runs it.
# The daemon from GTK 4.24 aborts when one render node sits twice in a frame (reuse.patch says
# why), which GTK 4.24's icons do. The release tarball comes from download.gnome.org, once per
# version, into .local/broadwayd/, checked against SHA256SUMS; the daemon goes into
# target/broadwayd/<version>/. A version without a line in SHA256SUMS isn't built.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(cd -- "$here/../.." && pwd)

version=$(pkg-config --modversion gtk4)
IFS=. read -r major minor _ <<<"$version"
if ((major == 4 && minor < 24)); then
    exit 0
fi

out="$root/target/broadwayd/$version"
daemon="$out/gtk4-broadwayd"
if [[ -x $daemon && $daemon -nt $here/reuse.patch && $daemon -nt ${BASH_SOURCE[0]} ]]; then
    echo "$daemon"
    exit 0
fi

mkdir -p "$out" "$root/.local/broadwayd"
exec 9>"$out/.lock"
flock 9
if [[ -x $daemon && $daemon -nt $here/reuse.patch && $daemon -nt ${BASH_SOURCE[0]} ]]; then
    echo "$daemon"
    exit 0
fi

tarball="gtk-$version.tar.xz"
expected=$(awk -v f="$tarball" '$1 !~ /^#/ && $2 == f { print $1 }' "$here/SHA256SUMS")
if [[ -z $expected ]]; then
    echo "tools/broadwayd: no checksum for $tarball in SHA256SUMS. Check that GTK $version's" \
        "daemon still needs reuse.patch, then add the tarball's line from download.gnome.org." >&2
    exit 1
fi
cached="$root/.local/broadwayd/$tarball"
if [[ ! -f $cached ]]; then
    echo "tools/broadwayd: downloading $tarball" >&2
    curl -fsSL -A 'mail-dev (automated)' -o "$cached.part" \
        "https://download.gnome.org/sources/gtk/$major.$minor/$tarball"
    mv -- "$cached.part" "$cached"
fi
if [[ $(sha256sum -- "$cached" | cut -d' ' -f1) != "$expected" ]]; then
    echo "tools/broadwayd: $cached doesn't match SHA256SUMS; delete it to download it again" >&2
    exit 1
fi

src="$out/src"
rm -rf -- "$src"
mkdir -p "$src"
tar -xJf "$cached" -C "$src" --strip-components=1 "gtk-$version/gdk"
patch -s -d "$src" -p1 <"$here/reuse.patch"

gen="$src/generated"
mkdir -p "$gen"
# GTK's gen-c-array.py, without running a script from the tarball. The headers GTK generates at
# build time (gdkconfig.h, gdkenumtypes.h, gdkversionmacros.h) come from the installed GTK.
gen_array() {
    python3 -I -c '
import sys
name, path, out = sys.argv[1:]
with open(path, "rb") as f, open(out, "w") as o:
    o.write("static const char %s[] = {\n" % name)
    for byte in f.read():
        o.write("  0x%02x,\n" % byte)
    o.write("};")
' "$@"
}
gen_array client_html "$src/gdk/broadway/client.html" "$gen/clienthtml.h"
gen_array broadway_js "$src/gdk/broadway/broadway.js" "$gen/broadwayjs.h"
printf '%s\n' '#define HAVE_UNISTD_H 1' '#define HAVE_SYS_MMAN_H 1' '#define HAVE_GIO_UNIX 1' \
    '#define GETTEXT_PACKAGE "gtk40"' >"$gen/config.h"

# shellcheck disable=SC2046 # pkg-config prints several flags
"${CC:-cc}" -O2 -o "$daemon.part" -DGTK_COMPILATION '-DG_LOG_DOMAIN="Gdk"' \
    -I"$gen" -I"$src/gdk/broadway" -I"$src/gdk" -I"$src" \
    -I/usr/include/gtk-4.0 -I/usr/include/gtk-4.0/gdk \
    $(pkg-config --cflags gio-unix-2.0 cairo pango) \
    "$src/gdk/broadway/broadwayd.c" "$src/gdk/broadway/broadway-server.c" \
    "$src/gdk/broadway/broadway-output.c" \
    $(pkg-config --libs gio-unix-2.0 cairo) -lrt
mv -- "$daemon.part" "$daemon"
echo "tools/broadwayd: built $daemon" >&2
echo "$daemon"
