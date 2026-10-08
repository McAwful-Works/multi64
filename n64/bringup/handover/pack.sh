#!/bin/sh
# Builds the cart diagnostics bundle a remote tester gets: run from the repository root on Windows
# (Git Bash), with the Tauri CLI installed in crates/multi64 (`npm install` there once).
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

# multi64d first and in the same profile: Multi64's build copies it into the installer. A workspace
# build leaves a different multi64d, so build it on its own right before.
cargo build --release -p multi64d
cargo build --release -p multi64-test-connector
(cd crates/multi64 && npm run build)

version=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' crates/multi64/src-tauri/tauri.conf.json | head -n 1)
setup=target/release/bundle/nsis/Multi64_${version}_x64-setup.exe

rm -rf "$out" "$out.zip"
mkdir -p "$out"
cp "$setup" target/release/multi64d.exe target/release/multi64-test-connector.exe "$out/"
cp n64/test-rom/multi64_test.z64 n64/bringup/multi64_bringup.z64 "$out/"
cp n64/bringup/baselines/sc64-2026-10-08.json "$out/"
cp n64/bringup/handover/diagnose.ps1 n64/bringup/handover/diagnose.bat n64/bringup/handover/README.txt "$out/"

{
    echo "multi64 $commit ($(git log -1 --format=%cs)), Multi64 $version, packed $(date -u +%Y-%m-%dT%H:%MZ)"
    (cd "$out" && sha256sum -- *.exe *.z64 *.json)
} | sed 's/$/\r/' > "$out/VERSION.txt"

(cd target/handover && powershell -NoProfile -Command "Compress-Archive -Path '$name' -DestinationPath '$name.zip'")
cat "$out/VERSION.txt"
echo "bundle: $out.zip"
