/**
 * Bundles explorer (esbuild). Safe to run with cwd = xfer64 crate root or xfer64/src-tauri.
 */
const path = require("path");
const fs = require("fs");
const { execSync } = require("child_process");

function findXfer64Root() {
  const hasMarker = (p) =>
    fs.existsSync(path.join(p, "package.json")) && fs.existsSync(path.join(p, "src", "explorer.js"));

  let dir = process.cwd();
  for (let i = 0; i < 14; i++) {
    if (hasMarker(dir)) return dir;
    const up = path.dirname(dir);
    if (up === dir) break;
    dir = up;
  }
  const scriptDir = path.resolve(__dirname);
  if (hasMarker(scriptDir)) return scriptDir;
  throw new Error(
    `xfer64: could not find package.json + src/explorer.js (cwd=${process.cwd()}, __dirname=${scriptDir})`
  );
}

execSync("npm run build:explorer", { cwd: findXfer64Root(), stdio: "inherit", shell: true });
