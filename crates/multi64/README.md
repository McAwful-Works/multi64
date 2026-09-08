# Multi64 (Windows)

Desktop app for **`multi64d`**: COM port (with auto-detect), start/stop the daemon, health, optional tray. Any **L3** client can use the same WebSocket as the app — see [`daemon-api-v1.md`](../../docs/spec/daemon-api-v1.md).

**Xfer64** (package **`xfer64`**) is a separate installer: SD over USB file manager — [`../xfer64/README.md`](../xfer64/README.md). Multi64 can bundle the Xfer64 installer (NSIS or MSI pairing — see **Release** below).

## Prerequisites

Rust + Cargo, Node + npm, **WebView2** (current Windows 10/11). Build **`multi64d`** before the GUI so `multi64d.exe` is found next to the app (`cargo build -p multi64d`).

## Develop

```sh
cargo build -p multi64d
cd crates/multi64
npm install
npm run dev
```

## Release (Windows)

```sh
cargo build -p multi64d --release
cargo build -p xfer64 --release
cd crates/xfer64 && npm install && npm run build
cd ../multi64 && npm install && npm run build
```

**Both installers stop `multi64d` before touching files**, on install and uninstall — NSIS via `NSIS_HOOK_PREINSTALL` / `PREUNINSTALL`, MSI via a custom action sequenced before `InstallValidate`. Without it the daemon holds `resources\multi64d.exe` open and the install fails on a locked file, which is not rare: the installer terminates the GUI, so the GUI's own `kill_daemon` never runs and the daemon is orphaned. Both match by **image name**, so a `multi64d` you are running from `cargo run` is killed too.

Installer graphics come from `windows/*.bmp`, regenerated from the brand masters by [`branding/installer-images`](../../branding/installer-images/README.md).

Artifacts under `target/release/bundle/`. **`src-tauri/build.rs`** copies one real Xfer64 installer (`xfer64-setup.exe` or `.msi`) and leaves the other as a placeholder. Use **`MULTI64_XFER64_BUNDLE=msi`** when building Multi64 if you built Xfer64 with **`tauri build --bundles msi`**. [Tauri `bundle.resources`](https://v2.tauri.app/reference/config/#bundle) lists `multi64d.exe`, Xfer64 payloads, and **`xfer64-installer-prompt.ps1`**. First-run installer can offer Xfer64 (skipped for silent NSIS **`/S`** or MSI **UILevel** 2).

## Settings & tray

- Settings: `%APPDATA%\multi64\gui-settings.json`
- **Autostart** (log in → open this app): uses [`auto-launch`](https://crates.io/crates/auto-launch); still starts **`multi64d`** as a child when “start daemon automatically” is on — not a Windows Service.
### Tray

**Double-click** the tray icon to raise the window. **Right-click** opens the menu:

| Item | |
|------|---|
| `Daemon: …` | Status line, disabled. Names the port when running, else the listen address. Reflects whether the **process** is alive — the window shows finer-grained health, since a status line that polled `/health` would issue a blocking request on every update |
| **Start / Stop daemon** | One item, whichever applies. Disabled with no serial port configured, because starting would fail; **Stop** stays enabled without one, since the port can disappear while the daemon runs |
| **Restart daemon** | Disabled while stopped — that case is **Start** |
| **Open Xfer64** | Reads *Install Xfer64…* when only the bundled installer is present, and is greyed when neither is |
| **Show window**, **Exit Multi64** | |

The menu tracks state live: starting or stopping the daemon from the window updates the tray, and vice versa.

**Left-click does not open the menu.** It cannot — the first click of a double-click would pop it, making the double-click unusable. This matches Windows convention, where left-click activates and right-click menus.

Toggling the tray option off and on needs an **app restart** (the icon is created at startup only).
