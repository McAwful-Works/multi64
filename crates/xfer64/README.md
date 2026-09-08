# Xfer64 (Windows)

**A Multi64 product** — dual-pane file manager for N64 flash-cart **SD contents over USB serial** (**FAT/exFAT** via [`multi64-sc64-sd`](../multi64-sc64-sd); no Windows drive letter; exclusive COM port).

- **SummerCart64**: vendor `SD_CARD_OP` / `SD_READ` / `MEMORY_READ` (see SC64 USB docs).
- **EverDrive 64 X-series**: Krikzz **usb64**-style serial (different opcodes than SC64). Host-side SD access uses **experimental** `RomRead` at `ed64RomLinearBase + LBA·512`; set `ed64RomLinearBase` in developer settings / `xfer64-settings.json`. See [`docs/spec/ed64-sd-usb-host.md`](../../docs/spec/ed64-sd-usb-host.md).

**Auto-detect** (default in Settings) probes candidate COM ports and picks the first positive cart signature match: SC64 `IDENTIFIER_GET` first, then EverDrive test (`cmd` + `t`). Non-cart serial devices are ignored when no signature matches. Override with **SummerCart64** or **EverDrive 64 (beta)** in Settings.

**Maintainer map:** [`docs/spec/xfer64-cart-serial.md`](../../docs/spec/xfer64-cart-serial.md).

## Appearance

Settings → **Appearance**, in both apps. Changes apply immediately; there is no Save step for them.

| Option | Values |
|--------|--------|
| **Theme** | Dark · Light · **Match system** (default) · High contrast |
| **Text size** | 90% · 100% · 115% · 130% |
| **Motion** | Follow system setting · Reduce animation |

**Match system** follows the OS via `prefers-color-scheme`. **High contrast** is a darker, higher-contrast variant with solid borders; all four themes meet WCAG AA for text contrast.

**Text size** scales the whole UI, not just the glyphs — every dimension in these sheets is in `rem`, and the setting drives the root font size. At 130% Multi64's window (fixed at 560×640, see `tauri.conf.json`) needs about 220px of scrolling to reach the bottom; nothing is clipped.

**Motion** honours the OS reduced-motion setting by default; **Reduce animation** forces it for systems that do not expose one. Animation collapses to 1ms rather than being removed, so `animationend` / `transitionend` still fire.

These preferences live in **`localStorage`**, *not* in the settings file — they must be readable synchronously before the first paint to avoid a flash of the wrong theme, and they are per-machine display choices rather than device configuration. Do not look for them in `gui-settings.json` or `xfer64-settings.json`.

## Windows drivers

**Xfer64 does not ship third‑party cart drivers in the installer.** For normal use it expects the cart to appear as a **USB serial (COM) port** using Windows’ inbox stack:

- **SummerCart64** — typically enumerates as a **CDC serial** device (`usbser.sys`). No vendor installer is required for COM‑based tools. Optional **WinUSB** (e.g. via [Zadig](https://zadig.akeo.ie/)) is mentioned in SC64 docs for **alternative** tooling / throughput experiments; replacing the default driver can **break** standard COM access, so do not use it unless you know you need it.

- **EverDrive 64 X‑series** — usually exposes **USB serial** for `usb64`‑style PC tools. If Windows shows an unknown USB device and no COM port appears, install Krikzz’s **`usb64`** driver package from their support files (see [EverDrive 64 X‑series dev/support](https://krikzz.com/pub/support/everdrive-64/x-series/dev/)), then replug the cart.

Bundling Krikzz or SC64 driver binaries inside Multi64/Xfer64 installers would require **explicit redistribution permission** from the vendors and ongoing updates whenever they ship new INF/USB IDs—we document manual install instead.

Build from the repository root (Cargo package **`xfer64`**):

```sh
cargo build -p xfer64 --release
cd crates/xfer64
npm install
npm run build
```

Installers and the app binary (e.g. `Xfer64.exe` on Windows) land under `target/release/` and `target/release/bundle/`.

## How Multi64 finds this app

The **Multi64** GUI resolves the Xfer64 executable in this order, taking the first that exists (`xfer64_exe_candidates` in `crates/multi64/src-tauri/src/lib.rs`):

1. **The Windows registry** — App Paths and Uninstall entries, which is how a normally installed Xfer64 is found regardless of where it went. Both the NSIS and MSI installers register it.
2. Next to `multi64.exe`.
3. `%LOCALAPPDATA%\multi64\`.
4. `%LOCALAPPDATA%\Programs\Xfer64\` and `%LOCALAPPDATA%\Xfer64\`.

Each directory is tried with `Xfer64.exe`, `xfer64.exe` and the legacy `multi64-cart-explorer.exe`. If none matches, Multi64 offers to run the bundled installer instead.
