#!/usr/bin/env bash
# Install the exact N64 toolchain pinned in toolchain.lock, into a prefix you own.
#
#   ./setup-toolchain.sh [prefix]      # default prefix: $HOME/n64inst
#
# Installs into the prefix rather than /opt, so no sudo is required. Prints the
# N64_INST export you need at the end. Safe to re-run: work is skipped when the
# pinned versions are already in place.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCK="$HERE/toolchain.lock"
PREFIX="${1:-$HOME/n64inst}"
WORK="$PREFIX/.build"
STAMP="$PREFIX/.multi64-toolchain-stamp"

[ -f "$LOCK" ] || { echo "missing $LOCK" >&2; exit 1; }
# shellcheck disable=SC1090
set -a; . "$LOCK"; set +a

need() { command -v "$1" >/dev/null || { echo "required tool not found: $1" >&2; exit 1; }; }
need curl; need git; need make; need sha256sum; need dpkg-deb

if [ -f "$STAMP" ] && grep -qx "LIBDRAGON_COMMIT=$LIBDRAGON_COMMIT" "$STAMP" \
   && grep -qx "TOOLCHAIN_ASSET_ID=$TOOLCHAIN_ASSET_ID" "$STAMP"; then
  echo "Already at the pinned versions. N64_INST=$PREFIX"
  exit 0
fi

mkdir -p "$WORK"

# --- 1. toolchain ------------------------------------------------------------
DEB="$WORK/$TOOLCHAIN_ASSET_NAME"
if [ ! -f "$DEB" ] || ! echo "$TOOLCHAIN_SHA256  $DEB" | sha256sum -c - >/dev/null 2>&1; then
  echo "==> downloading toolchain asset $TOOLCHAIN_ASSET_ID"
  # By asset id: the release tag is rolling, ids are immutable. See toolchain.lock.
  curl -fL --retry 3 \
    -H "Accept: application/octet-stream" \
    "https://api.github.com/repos/DragonMinded/libdragon/releases/assets/$TOOLCHAIN_ASSET_ID" \
    -o "$DEB"
fi

echo "==> verifying checksum"
echo "$TOOLCHAIN_SHA256  $DEB" | sha256sum -c - || {
  echo "CHECKSUM MISMATCH. The pinned asset is not what was downloaded; refusing to continue." >&2
  exit 1
}

echo "==> extracting toolchain into $PREFIX"
rm -rf "$WORK/x"; mkdir -p "$WORK/x"
dpkg-deb -x "$DEB" "$WORK/x"
# The .deb lays the toolchain out under opt/libdragon; that subtree IS the prefix.
mkdir -p "$PREFIX"
cp -a "$WORK/x/opt/libdragon/." "$PREFIX/"

GOT_GCC="$("$PREFIX/bin/mips64-elf-gcc" -dumpversion)"
[ "$GOT_GCC" = "$TOOLCHAIN_GCC_VERSION" ] || {
  echo "GCC version mismatch: expected $TOOLCHAIN_GCC_VERSION, got $GOT_GCC" >&2
  exit 1
}
echo "    mips64-elf-gcc $GOT_GCC"

# --- 2. libdragon ------------------------------------------------------------
SRC="$WORK/libdragon"
if [ ! -d "$SRC/.git" ]; then
  echo "==> cloning libdragon"
  git clone "$LIBDRAGON_REPO" "$SRC"
fi
echo "==> checking out $LIBDRAGON_COMMIT"
git -C "$SRC" fetch --all --tags --quiet
git -C "$SRC" checkout --quiet --detach "$LIBDRAGON_COMMIT"

echo "==> building libdragon (this takes a few minutes)"
( cd "$SRC" && N64_INST="$PREFIX" PATH="$PREFIX/bin:$PATH" ./build.sh )

# --- 3. stamp ----------------------------------------------------------------
# libdragon records no version of its own, so this is the only way a later build
# can tell what it is linked against.
{
  echo "# Written by n64/setup-toolchain.sh -- do not edit."
  echo "LIBDRAGON_COMMIT=$LIBDRAGON_COMMIT"
  echo "TOOLCHAIN_ASSET_ID=$TOOLCHAIN_ASSET_ID"
  echo "TOOLCHAIN_GCC_VERSION=$GOT_GCC"
  echo "INSTALLED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$STAMP"

cat <<EOF

Done. Add this to your shell (or prefix your make invocation):

    export N64_INST="$PREFIX"
    export PATH="\$N64_INST/bin:\$PATH"

Then:  cd $HERE/test-rom && make
Verify against the lock at any time:  make check-toolchain
EOF
