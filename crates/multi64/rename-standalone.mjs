// Rename the standalone bundles so they cannot overwrite the full ones.
//
// Tauri derives an installer's filename from `productName`, and the standalone build is the same
// app with the same name — so both write Multi64_0.1.0_x64-setup.exe and whichever runs second
// silently replaces the first. Changing productName would rename the app itself: different window
// title, different install directory, two entries in the Start menu. Renaming the artifact
// afterwards keeps the app identical and makes the file on disk say which build it is.
//
// Run by `npm run build:standalone`; harmless on its own.

import { readdir, rename } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const bundle = join(here, "..", "..", "target", "release", "bundle");

/** `Multi64_0.1.0_x64-setup.exe` -> `Multi64-standalone_0.1.0_x64-setup.exe`. */
const standaloneName = (name) => name.replace(/^Multi64_/, "Multi64-standalone_");

let renamed = 0;
for (const [dir, ext] of [
  ["nsis", ".exe"],
  ["msi", ".msi"],
]) {
  const path = join(bundle, dir);
  let entries;
  try {
    entries = await readdir(path);
  } catch {
    // That bundle target was not built; nothing to rename.
    continue;
  }
  for (const name of entries) {
    // Only the freshly built full-name artifacts: anything already carrying the standalone name is
    // from an earlier run and renaming it again would be a no-op at best.
    if (!name.startsWith("Multi64_") || !name.endsWith(ext)) continue;
    const to = standaloneName(name);
    await rename(join(path, name), join(path, to));
    console.log(`${dir}/${name} -> ${to}`);
    renamed++;
  }
}

if (renamed === 0) {
  // Louder than silence: the likely cause is running this after a *full* build, which would leave
  // the full installer sitting under the name the standalone one is expected to have.
  console.warn(
    "rename-standalone: found no Multi64_* bundles to rename. Did the standalone build run?",
  );
  process.exit(1);
}
