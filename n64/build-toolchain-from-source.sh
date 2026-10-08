#!/usr/bin/env bash
# Build the N64 toolchain pinned in toolchain.lock from source, and pack it as the
# tarball setup-toolchain.sh installs.
#
#   ./build-toolchain-from-source.sh [workdir]   # default workdir: ./toolchain-build
#
# This is how the hosted tarball was made, and the way back if it is ever lost.
# It runs libdragon's own tools/build-toolchain.sh at TOOLCHAIN_BUILD_COMMIT
# against upstream sources, every one of them checked against a SHA-256 below.
# Needs a host gcc/g++, make, curl, tar, xz and bzip2; no sudo, nothing from
# the N64 toolchain. Takes 10-30 minutes depending on cores.
#
# The tarball's own SHA-256 depends on the host (its gcc and glibc end up in the
# binaries), so a rebuild elsewhere is not expected to match TOOLCHAIN_SHA256.
# What must match is what it compiles: a rebuilt toolchain is proven when
# n64/test-rom built with it reproduces the committed multi64_test.z64.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCK="$HERE/toolchain.lock"
OUT="$(mkdir -p "${1:-$PWD/toolchain-build}" && cd "${1:-$PWD/toolchain-build}" && pwd)"

[ -f "$LOCK" ] || { echo "missing $LOCK" >&2; exit 1; }
# shellcheck disable=SC1090
. "$LOCK"

need() { command -v "$1" >/dev/null || { echo "required tool not found: $1" >&2; exit 1; }; }
need curl; need sha256sum; need make; need gcc; need g++; need tar; need xz; need bzip2

# The versions libdragon's script at TOOLCHAIN_BUILD_COMMIT downloads. Fetching
# them here first, by checksum, means its own downloads are skipped.
SOURCES="
https://ftp.gnu.org/gnu/binutils/binutils-2.45.tar.gz 8a3eb4b10e7053312790f21ee1a38f7e2bbd6f4096abb590d3429e5119592d96
https://ftp.gnu.org/gnu/gcc/gcc-16.2.0/gcc-16.2.0.tar.gz 071d00a097579e5ef7ce97fc4a9e58e73fd3503c0a013c765c970370a5a53b9b
https://sourceware.org/pub/newlib/newlib-4.4.0.20231231.tar.gz 0c166a39e1bf0951dfafcd68949fe0e4b6d3658081d6282f39aeefc6310f2f13
https://ftp.gnu.org/gnu/gmp/gmp-6.3.0.tar.bz2 ac28211a7cfb609bae2e2c8d6058d66c8fe96434f740cf6fe2e47b000d1c20cb
https://ftp.gnu.org/gnu/mpc/mpc-1.3.1.tar.gz ab642492f5cf882b74aa0cb730cd410a81edcdbec895183ce930e706c1c759b8
https://ftp.gnu.org/gnu/mpfr/mpfr-4.2.1.tar.gz 116715552bd966c85b417c424db1bbdf639f53836eb361549d1f8d6ded5cb4c6
"

SRC="$OUT/src"
BUILD="$OUT/build"
PREFIX="$OUT/prefix"
STUB="$OUT/stub"
mkdir -p "$SRC" "$STUB"

fetch() { # url sha256 dest
  if [ ! -f "$3" ] || ! echo "$2  $3" | sha256sum -c - >/dev/null 2>&1; then
    echo "==> downloading $1"
    curl -fL --retry 3 "$1" -o "$3"
  fi
  echo "$2  $3" | sha256sum -c - >/dev/null || {
    echo "CHECKSUM MISMATCH for $1; refusing to continue." >&2; exit 1; }
}

echo "$SOURCES" | while read -r url sha; do
  if [ -n "$url" ]; then fetch "$url" "$sha" "$SRC/$(basename "$url")"; fi
done

SCRIPT="$OUT/build-toolchain.sh"
fetch "https://raw.githubusercontent.com/DragonMinded/libdragon/$TOOLCHAIN_BUILD_COMMIT/tools/build-toolchain.sh" \
  "$TOOLCHAIN_BUILD_SCRIPT_SHA256" "$SCRIPT"
chmod +x "$SCRIPT"

# newlib's build makes its Info manuals and fails without makeinfo. The manuals
# are not part of the compiler, so a stub that writes empty files stands in for
# texinfo rather than requiring it.
cat > "$STUB/makeinfo" <<'EOF'
#!/bin/sh
out=
while [ $# -gt 0 ]; do
  case "$1" in -o) out="$2"; shift;; --version) echo "makeinfo (GNU texinfo) 7.1"; exit 0;; esac
  shift
done
[ -n "$out" ] && : > "$out"
exit 0
EOF
chmod +x "$STUB/makeinfo"

echo "==> building the toolchain into $PREFIX (log: $OUT/build.log)"
rm -rf "$BUILD" "$PREFIX"
mkdir -p "$BUILD"
( cd "$OUT" && PATH="$STUB:$PATH" N64_INST="$PREFIX" BUILD_PATH="$BUILD" DOWNLOAD_PATH="$SRC" \
    ./build-toolchain.sh ) > "$OUT/build.log" 2>&1 || {
  echo "toolchain build failed; see $OUT/build.log" >&2; exit 1; }

GOT_GCC="$("$PREFIX/bin/mips64-elf-gcc" -dumpversion)"
[ "$GOT_GCC" = "$TOOLCHAIN_GCC_VERSION" ] || {
  echo "GCC version mismatch: expected $TOOLCHAIN_GCC_VERSION, got $GOT_GCC" >&2; exit 1; }

echo "==> packing $TOOLCHAIN_NAME"
# Fixed order, times and owners, so the same prefix always packs to the same bytes.
XZ_OPT=-9 tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  -C "$PREFIX" -cJf "$OUT/$TOOLCHAIN_NAME" .

GOT_SHA="$(sha256sum "$OUT/$TOOLCHAIN_NAME" | cut -d' ' -f1)"
cat <<EOF

Built $OUT/$TOOLCHAIN_NAME
    sha256 $GOT_SHA
    (lock: $TOOLCHAIN_SHA256)

A different checksum is expected on a different host. To prove this build, install
it (tar -xJf into an empty prefix), install libdragon $LIBDRAGON_COMMIT into the
same prefix, and check that n64/test-rom reproduces the committed multi64_test.z64.
EOF
