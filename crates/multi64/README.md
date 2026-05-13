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

Artifacts under `target/release/bundle/`. **`src-tauri/build.rs`** copies one real Xfer64 installer (`xfer64-setup.exe` or `.msi`) and leaves the other as a placeholder. Use **`MULTI64_XFER64_BUNDLE=msi`** when building Multi64 if you built Xfer64 with **`tauri build --bundles msi`**. [Tauri `bundle.resources`](https://v2.tauri.app/reference/config/#bundle) lists `multi64d.exe`, Xfer64 payloads, and **`xfer64-installer-prompt.ps1`**. First-run installer can offer Xfer64 (skipped for silent NSIS **`/S`** or MSI **UILevel** 2).

## Settings & tray

- Settings: `%APPDATA%\multi64\gui-settings.json`
- **Autostart** (log in → open this app): uses [`auto-launch`](https://crates.io/crates/auto-launch); still starts **`multi64d`** as a child when “start daemon automatically” is on — not a Windows Service.
- **Tray:** toggling the tray option needs an **app restart** (icon is created at startup only).
