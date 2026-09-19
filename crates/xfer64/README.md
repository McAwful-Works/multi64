# Xfer64 (Windows)

**A Multi64 product** — dual-pane file manager for N64 flash-cart **SD contents over USB serial** (**FAT/exFAT** via [`multi64-sc64-sd`](../multi64-sc64-sd); no Windows drive letter; exclusive serial port).

- **SummerCart64**: vendor `SD_CARD_OP` / `SD_READ` / `MEMORY_READ` (see SC64 USB docs).
- **EverDrive 64 X-series**: Krikzz **usb64**-style serial (different opcodes than SC64). An **experimental** SD mode issues `RomRead` at `ed64RomLinearBase + LBA·512` (set in developer settings / `xfer64-settings.json`), but `RomRead` reads cart ROM memory rather than the SD card, so it is not expected to list the card and has never done so on hardware. See [`docs/spec/ed64-sd-usb-host.md`](../../docs/spec/ed64-sd-usb-host.md#why-romread-is-not-sd-access).
- **EverDrive-64 PRO** (experimental): file-level SD access over Krikzz's edlink Gen3 protocol, where the cart itself serves the file system ([`docs/spec/ed64-pro-usb-host.md`](../../docs/spec/ed64-pro-usb-host.md)). Browse, copy both ways, create folders, delete and rename. The PRO's link has no rename command, so a rename **copies the item and then deletes the original**: it takes as long as copying it off the cart and back, and cannot change only the letter case of a name. It has never been tested on a real cart, so Xfer64 asks before the first write each run, and `xfer64 upload` needs `--experimental-ed64pro-writes`.

**Auto-detect** (the default **Cart**) looks at each candidate serial port in two tiers ([`multi64-cart-probe`](../cart-probe/README.md)). First the port's **USB descriptors**, which recognise a SummerCart64 without writing anything — so it is found even while `multi64d` holds the port. Only if that does not match does it probe on the wire, in order: SC64 `IDENTIFIER_GET`, then the EverDrive-64 PRO handshake, then the X-series EverDrive test (`cmd` + `t`). The first positive cart signature wins; non-cart serial devices are ignored when no signature matches. Override with **SummerCart64**, **EverDrive-64 PRO (beta)** or **EverDrive-64 X7 (beta)** from the **Cart** menu in the app bar, which applies at once, or in Settings → **Cart**, which applies on **Save settings**.

**Serial port** sits beside Cart in the app bar and in Settings → **Cart**. Its **Auto-detect** is only as good as the cart choice: with Cart on Auto-detect it probes every port as above, but with a cart chosen it does not probe — it takes the port whose USB name matches that cart, or the only USB serial port — so Settings warns under Serial port when that finds none. Serial port applies at once from the app bar, or on **Save settings** from Settings. Both app-bar menus are disabled while a cart operation runs.

**Maintainer map:** [`docs/spec/xfer64-cart-serial.md`](../../docs/spec/xfer64-cart-serial.md).

## Drag and drop

Files cross between Windows and Xfer64 by dragging, in both directions.

| Drag | What happens |
|------|--------------|
| **Explorer → Cart pane** | Imports over serial, exactly as the **Import** button does. |
| **Explorer → This PC pane** | An ordinary file copy into that folder. |
| **Pane → pane** | Unchanged: cart ↔ This PC copy in the direction you dragged. |
| **This PC pane → Explorer** | Hands the OS the real paths; the shell copies them. |
| **Cart pane → Explorer** | Stages first, then drags — see below. |

A drop lands in the **folder row under the pointer**; with no row there, it lands in the pane's
current folder. Dragging out starts when the pointer leaves the Xfer64 window, so a target window
that sits *on top of* Xfer64 cannot be dropped on — drag to a part of it that is outside the
Xfer64 window, or use **Export to This PC**.

### Dragging cart files out takes two gestures

Windows will not start a drag for a file that does not exist, and the cart's SD card is not a
drive letter. So the first drag out of the Cart pane **exports the selection to a staging
directory** (`%TEMP%\xfer64-drag\…`) with the usual progress bar and Cancel; a 64 MB ROM over
serial takes as long as it takes. The pointer is long released by then, so the status line says
*Ready — drag … out again*, and the second drag goes straight to the shell.

Staged files are copies. They are deleted when Xfer64 exits, one left behind by a crash is pruned
on a later start, and a selection is re-exported if the file changed on the cart meanwhile —
or if the serial port or cart type changed, since the staged copies may be from another card.

**The main window sets `dragDropEnabled: true`** (`tauri.conf.json`) — that is the only way Tauri
reports the dropped paths, and WebView2's own HTML5 drop reports none. It also switches HTML5
drag-and-drop off inside the webview on Windows, which is why the pane-to-pane drag is built on
pointer events and the outbound drag on [`tauri-plugin-drag`](https://crates.io/crates/tauri-plugin-drag).
See the drag-and-drop section comment at the top of [`src/explorer.js`](src/explorer.js) and
[`src-tauri/src/drag_out.rs`](src-tauri/src/drag_out.rs). The gestures are checked headlessly in
[`e2e/`](e2e/README.md) — that covers which backend command each drag reaches, not whether Windows
accepts the drag.

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

Installers and the app binary (e.g. `Xfer64.exe` on Windows) land under `target/release/` and `target/release/bundle/`. Xfer64 has its own installer; Multi64's installer does not include it.
