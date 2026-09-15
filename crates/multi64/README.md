# Multi64 (Windows)

Desktop app for **`multi64d`**, which the UI calls the **bridge**: serial port (with Auto-detect), start and stop the bridge, health, optional tray. Any **L3** client can use the same WebSocket as the app — see [`daemon-api-v1.md`](../../docs/spec/daemon-api-v1.md).

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

## Appearance

Settings → **Appearance**, in both apps. Changes apply immediately; there is no Save step for them.

| Option | Values |
|--------|--------|
| **Theme** | Dark · Light · **Match system** (default) · High contrast |
| **Text size** | 90% · 100% · 115% · 130% |
| **Motion** | **Match system** (default) · Reduce animation |

**Match system** follows the OS via `prefers-color-scheme`. **High contrast** is a darker, higher-contrast variant with solid borders; all four themes meet WCAG AA for text contrast.

**Text size** scales the whole UI, not just the glyphs — every dimension in these sheets is in `rem`, and the setting drives the root font size. At 130% Multi64's window (fixed at 560×640, see `tauri.conf.json`) needs about 220px of scrolling to reach the bottom; nothing is clipped.

**Motion** honours the OS reduced-motion setting by default; **Reduce animation** forces it for systems that do not expose one. Animation collapses to 1ms rather than being removed, so `animationend` / `transitionend` still fire.

These preferences live in **`localStorage`**, *not* in the settings file — they must be readable synchronously before the first paint to avoid a flash of the wrong theme, and they are per-machine display choices rather than device configuration. Do not look for them in `gui-settings.json` or `xfer64-settings.json`.

## Settings & tray

- Settings: `%APPDATA%\multi64\gui-settings.json`
- **Cart** picks the cart `multi64d` is started for ([daemon API §5.1](../../docs/spec/daemon-api-v1.md) `--cart`): **Auto-detect** (default), **SummerCart64**, **EverDrive-64 X7 (beta)** or **EverDrive-64 PRO (beta)**. Neither EverDrive mapping has run against a cart ([X7 §4.5](../../docs/spec/l3-over-everdrive-x7.md), [PRO §8](../../docs/spec/l3-over-everdrive-pro.md)), so the option, the status panel and the tray mark it *beta*, the Cart hint and the daemon log say it is experimental, and a running daemon is no evidence the cart link works. The PRO runs at its fixed 921600 baud, so **Baud** does not apply to it. Changing the cart restarts a running daemon. Settings files from before the Cart setting read as Auto-detect; a saved cart is kept.
- **Auto-detect** decides at each start, then starts `multi64d` for the cart it found; the daemon itself has no auto mode. First it looks for a SummerCart64 by its USB descriptors, as **Serial port → Auto-detect** does, which sends nothing. Failing that, it sends cart test commands ([`multi64-cart-probe`](../cart-probe/README.md): SC64 `IDENTIFIER_GET`, the PRO's edlink handshake, the X-series `usb64` test) to the chosen serial port, or with the port on Auto-detect to every serial port, USB devices first, until one answers. **Other devices on those ports receive those bytes**, a port another program holds cannot be probed, and the EverDrive checks have never been run against a cart. The status panel shows the cart it chose and the port it is on, and the log says Auto-detect chose it and lists each port it tried.
- **Serial port → Auto-detect** picks only a port whose USB descriptors identify a SummerCart64: FTDI `0403:6014` with an `SC64…` serial number or product string. Nothing is written to a port to find out. Other serial devices are never chosen, and with two carts plugged in neither is: the port stays unset, Settings and the status line say why, and the daemon does not start. Pick a port explicitly to use anything else.
- **With a fixed EverDrive selected, Serial port → Auto-detect picks nothing.** The X7's FT245R (`0403:6001`) is a stock FTDI part with nothing cart-specific in its descriptors, and a PRO can only be recognised by its edlink handshake, which means writing to each port. A fixed cart never probes, and never hands an EverDrive the SC64's port: choose the EverDrive's serial port, or set **Cart** to **Auto-detect**.
- **Autostart** (log in → open this app): uses [`auto-launch`](https://crates.io/crates/auto-launch); still starts **`multi64d`** as a child when **Start the bridge when Multi64 opens** is on — not a Windows Service.
### Tray

**Double-click** the tray icon to raise the window. **Right-click** opens the menu:

| Item | |
|------|---|
| `Bridge: …` | Status line, disabled. While running, names the port the daemon was actually started on (not what the settings would pick now), else the listen address; while stopped, says so when there is no serial port, or when something else already listens on the listen address. Any cart other than the default SummerCart64 is named after it (`· EverDrive-64 X7 (beta)`), again the one the running process was started for. Reflects whether the **process** is alive — the window shows finer-grained health, since a status line that polled `/health` would issue a blocking request on every update |
| **Start / Stop bridge** | One item, whichever applies. Disabled with no serial port configured, because starting would fail; **Stop** stays enabled without one, since the port can disappear while the daemon runs. **Start** stays enabled while the listen address is taken, as the window's Start button does: the menu is only rebuilt when the bridge changes, so a greyed item would stay greyed after the other process exits |
| **Restart bridge** | Disabled while stopped — that case is **Start** |
| **Open Xfer64** | Reads *Install Xfer64…* when only the bundled installer is present, and is greyed when neither is |
| **Show window**, **Exit Multi64** | |

The menu tracks state live: starting or stopping the bridge from the window updates the tray, and vice versa.

**Left-click does not open the menu.** It cannot — the first click of a double-click would pop it, making the double-click unusable. This matches Windows convention, where left-click activates and right-click menus.

Toggling the tray option off and on needs an **app restart** (the icon is created at startup only).
