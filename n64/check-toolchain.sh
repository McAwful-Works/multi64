#!/usr/bin/env sh
# Compare the installed toolchain against n64/toolchain.lock.
#
#   ./check-toolchain.sh [N64_INST]
#
# libdragon records no version of its own at install time, so this compares the
# stamp written by setup-toolchain.sh. A prefix installed some other way has no
# stamp: that is reported as "unknown", not as a failure, because it may well be
# correct -- it just cannot be proven from here.
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
LOCK="$HERE/toolchain.lock"
INST="${1:-${N64_INST:-}}"

[ -f "$LOCK" ] || { echo "missing $LOCK" >&2; exit 1; }
[ -n "$INST" ] || { echo "N64_INST is not set and no prefix was given" >&2; exit 1; }

# shellcheck disable=SC1090
. "$LOCK"
STAMP="$INST/.multi64-toolchain-stamp"

printf 'lock:      libdragon %s\n' "$LIBDRAGON_COMMIT"
printf '           gcc %s (sha256 %s)\n' "$TOOLCHAIN_GCC_VERSION" "$TOOLCHAIN_SHA256"

if [ ! -x "$INST/bin/mips64-elf-gcc" ]; then
  echo "installed:  no mips64-elf-gcc under $INST" >&2
  exit 1
fi
GOT_GCC="$("$INST/bin/mips64-elf-gcc" -dumpversion)"
printf 'installed: gcc %s\n' "$GOT_GCC"

rc=0
proven=1
[ "$GOT_GCC" = "$TOOLCHAIN_GCC_VERSION" ] || {
  echo "MISMATCH: gcc $GOT_GCC != pinned $TOOLCHAIN_GCC_VERSION" >&2; rc=1; }

if [ -f "$STAMP" ]; then
  GOT_COMMIT="$(sed -n 's/^LIBDRAGON_COMMIT=//p' "$STAMP")"
  printf '           libdragon %s\n' "$GOT_COMMIT"
  [ "$GOT_COMMIT" = "$LIBDRAGON_COMMIT" ] || {
    echo "MISMATCH: libdragon $GOT_COMMIT != pinned $LIBDRAGON_COMMIT" >&2; rc=1; }
  # The gcc version alone cannot tell two builds of 16.2.0 apart, and they emit
  # different code. Stamps written before the checksum was recorded name the
  # asset id instead, which identified the same file while it existed.
  GOT_SHA="$(sed -n 's/^TOOLCHAIN_SHA256=//p' "$STAMP")"
  if [ -n "$GOT_SHA" ]; then
    printf '           gcc sha256 %s\n' "$GOT_SHA"
    [ "$GOT_SHA" = "$TOOLCHAIN_SHA256" ] || {
      echo "MISMATCH: toolchain sha256 $GOT_SHA != pinned $TOOLCHAIN_SHA256" >&2; rc=1; }
  else
    GOT_ASSET="$(sed -n 's/^TOOLCHAIN_ASSET_ID=//p' "$STAMP")"
    printf '           gcc asset %s\n' "$GOT_ASSET"
    [ "$GOT_ASSET" = "$TOOLCHAIN_ASSET_ID" ] || {
      echo "MISMATCH: toolchain asset $GOT_ASSET != pinned $TOOLCHAIN_ASSET_ID" >&2; rc=1; }
  fi
else
  proven=0
  echo "           libdragon unknown (no stamp; prefix not installed by setup-toolchain.sh)"
fi

if [ "$rc" -ne 0 ]; then
  exit "$rc"
elif [ "$proven" -eq 1 ]; then
  echo "OK: matches the pin."
else
  # Do not claim the pin is satisfied when half of it could not be checked.
  echo "PARTIAL: gcc version matches; which gcc build, and libdragon's version, unverified."
  echo "  A rebuild may not reproduce the committed multi64_test.z64."
  echo "  Run ./setup-toolchain.sh for a prefix that can be verified."
fi
exit 0
