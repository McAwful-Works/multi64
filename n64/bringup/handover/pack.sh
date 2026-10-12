#!/bin/sh
# Builds the cart diagnostics bundle a remote tester gets: run from the repository root on Windows
# (Git Bash), with the Tauri CLI installed in crates/multi64 and crates/ap64 (`npm install` in each
# once).
#
#   sh n64/bringup/handover/pack.sh [name]
#
# Writes target/handover/<name>/ and <name>.zip (default name: cart-diagnostics-<commit>). The
# ROMs are the committed ones, so every bundle from one commit carries the same ROM bytes.
set -eu

root=$(git rev-parse --show-toplevel)
cd "$root"
commit=$(git rev-parse --short HEAD)
name=${1:-cart-diagnostics-$commit}
out=target/handover/$name

if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    echo "pack.sh: the working tree has changes; a bundle must match a commit" >&2
    exit 1
fi

# AP64's installer, for diagnose.ps1's phase 3. The agents AP64 patches into a game are the ones
# committed under crates/ap64-core/profiles, so a seed patched with this installer carries this
# commit's agent, where one from an AP64 the tester already had may carry an older one. It needs no
# multi64d, and is built before it so that nothing comes between multi64d's build and Multi64's.
(cd crates/ap64 && npm run build)

# multi64d first and in the same profile: Multi64's build copies it into the installer. A workspace
# build leaves a different multi64d, so build it on its own right before.
cargo build --release -p multi64d
cargo build --release -p multi64-test-connector
(cd crates/multi64 && npm run build)

version=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' crates/multi64/src-tauri/tauri.conf.json | head -n 1)
setup=target/release/bundle/nsis/Multi64_${version}_x64-setup.exe
ap64_version=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' crates/ap64/src-tauri/tauri.conf.json | head -n 1)
ap64_setup=target/release/bundle/nsis/AP64_${ap64_version}_x64-setup.exe

rm -rf "$out" "$out.zip"
mkdir -p "$out"
cp "$setup" "$ap64_setup" target/release/multi64d.exe target/release/multi64-test-connector.exe "$out/"
cp n64/test-rom/multi64_test.z64 n64/bringup/multi64_bringup.z64 "$out/"
# The newest SummerCart64 baseline, under one name so that diagnose.ps1 need not know its date.
# Baselines are named sc64-<date>.json, so the last in order is the newest.
baseline=$(ls n64/bringup/baselines/sc64-*.json | sort | tail -n 1)
cp "$baseline" "$out/sc64-baseline.json"
cp n64/bringup/handover/diagnose.ps1 n64/bringup/handover/diagnose.bat n64/bringup/handover/play-session.bat \
    n64/bringup/handover/zip-folder.ps1 n64/bringup/handover/README.txt "$out/"

{
    echo "multi64 $commit ($(git log -1 --format=%cs)), Multi64 $version, AP64 $ap64_version, packed $(date -u +%Y-%m-%dT%H:%MZ)"
    echo "sc64-baseline.json is $(basename "$baseline")"
    (cd "$out" && sha256sum -- *.exe *.z64 *.json)
} | sed 's/$/\r/' > "$out/VERSION.txt"

# The same zip routine diagnose.ps1 uses for its results. -Strict: a bundle with a file left out is
# no bundle.
powershell -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$root/n64/bringup/handover/zip-folder.ps1")" \
    -Dir "$(cygpath -w "$root/$out")" -Zip "$(cygpath -w "$root/$out.zip")" -Strict
cat "$out/VERSION.txt"
echo "bundle: $out.zip"
