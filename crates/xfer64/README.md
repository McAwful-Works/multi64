<img src="../../branding/xfer64.svg" alt="" width="72" align="left" />

# Xfer64

**Cart SD over USB** · a Multi64 product

<br clear="left" />

Browse and copy the files on your cart's SD card from Windows, over the USB cable the cart is
already using. Two panes, cart on one side and this PC on the other, with drag and drop between
them.

There is no drive letter: Xfer64 reads FAT32 and exFAT off the card itself and holds the serial
port while it works. When Multi64's bridge is running it steps aside for each operation and
hands the port back afterward.

## Which carts work

- **SummerCart64**: vendor `SD_CARD_OP` / `SD_READ` / `MEMORY_READ` (see SC64 USB docs).
- **EverDrive 64 X-series**: Krikzz `usb64`-style serial (different opcodes than SC64). An experimental SD mode issues `RomRead` at `ed64RomLinearBase + LBA·512` (set under Settings → **EverDrive SD (experimental)**, which is shown only while **Cart** is **EverDrive-64 X7 (beta)**), but `RomRead` reads cart ROM memory rather than the SD card, so it is not expected to list the card and has never done so on hardware. See [`docs/spec/ed64-sd-usb-host.md`](../../docs/spec/ed64-sd-usb-host.md#why-romread-is-not-sd-access).
- **EverDrive-64 PRO** (experimental): file-level SD access over Krikzz's edlink Gen3 protocol, where the cart itself serves the file system ([`docs/spec/ed64-pro-usb-host.md`](../../docs/spec/ed64-pro-usb-host.md)). Browse, copy both ways, create folders, delete and rename. The PRO's link has no rename command, so a rename copies the item and then deletes the original: it takes as long as copying it off the cart and back, and cannot change only the letter case of a name. It has never been tested on a real cart, so Xfer64 asks before the first write each run, and `xfer64 upload` needs `--experimental-ed64pro-writes`.

**Auto-detect** (the default **Cart**) first asks a running `multi64d`, when **Serial port** is on Auto-detect too: if the daemon answers and its `GET /` names a cart and a port that is still plugged in, Xfer64 uses those and probes nothing. Otherwise it looks at each candidate serial port in two tiers ([`multi64-cart-probe`](../cart-probe/README.md)). First the port's USB descriptors, which recognize a SummerCart64 without writing anything — so it is found even while `multi64d` holds the port. Only if that does not match does it probe on the wire: SC64 `IDENTIFIER_GET`, then the EverDrive-64 PRO handshake, then the X-series EverDrive test (`cmd` + `t`) — except on an FTDI `0403:6001` port (the X7's USB bridge chip), where the X-series test goes first. The first positive cart signature wins; non-cart serial devices are ignored when no signature matches. Override with **SummerCart64**, **EverDrive-64 PRO (beta)** or **EverDrive-64 X7 (beta)** from the **Cart** menu in the app bar, which applies at once, or in Settings → **Cart**, which applies on **Save settings**.
**Serial port** sits beside Cart in the app bar and in Settings → **Cart**. Its **Auto-detect** is only as good as the cart choice: with Cart on Auto-detect it probes every port as above, but with a cart chosen it does not probe — it takes the port whose USB name matches that cart, or the only USB serial port — so Settings warns under Serial port when that finds none. Serial port applies at once from the app bar, or on **Save settings** from Settings. Both app-bar menus are disabled while a cart operation runs.

Maintainer map: [`docs/spec/xfer64-cart-serial.md`](../../docs/spec/xfer64-cart-serial.md).

## Copying and closing

Copying onto a file that already exists asks first. **Replace** on This PC writes the new copy
beside the old one and swaps it in only once it is complete, so a cancel or a failure part-way
leaves the original as it was; a crash or power loss can leave a `.<name>.<…>.partial` file next to
it, never a truncated original. A cart import does the same on a SummerCart64. On an EverDrive-64
PRO the cart's own link truncates the file it replaces when it starts, so an interrupted Replace
there reports what is left of the original. Renaming on This PC onto a name the folder already
has is refused; a rename that only changes letter case goes through.

Closing the window while an operation runs does not cut it off: Xfer64 asks whether to keep
waiting or to cancel the operation and close once it has stopped. Either way it exits only after
the cart's SD card has been handed back and the Multi64 bridge resumed. A close during a quick cart
read simply waits for it to finish.

## Drag and drop

Files cross between Windows and Xfer64 by dragging, in both directions.

| Drag | What happens |
|------|--------------|
| Explorer → Cart pane | Imports over serial, exactly as the **Import** button does. |
| Explorer → This PC pane | An ordinary file copy into that folder. |
| Pane → pane | Unchanged: cart ↔ This PC copy in the direction you dragged. |
| This PC pane → Explorer | Hands the OS the real paths; the shell copies them. |
| Cart pane → Explorer | Stages first, then drags — see below. |

A drop lands in the folder row under the pointer; with no row there, it lands in the pane's
current folder. A drop is ignored while either pane has an operation running, as Import and
Export are; wait for it to finish, then drop again. Dragging out starts when the pointer leaves
the Xfer64 window, so a target window that sits *on top of* Xfer64 cannot be dropped on — drag to
a part of it that is outside the Xfer64 window, or use **Export to This PC**.

### Dragging cart files out takes two gestures

Windows will not start a drag for a file that does not exist, and the cart's SD card is not a
drive letter. So the first drag out of the Cart pane exports the selection to a staging
directory (`%TEMP%\xfer64-drag\…`) with the usual progress bar and Cancel; a 64 MB ROM over
serial takes as long as it takes. The pointer is long released by then, so the status line says
*Ready — drag … out again*, and the second drag goes straight to the shell.

Staged files are copies. They are deleted when Xfer64 exits. A staging directory left behind by a
crash is pruned when a later run first stages a cart drag-out, once it is more than 24 hours old,
and never while another running Xfer64 still holds it.

The cart lists no modified times, so a staged copy cannot be checked against the card. Instead the
selection is exported again, rather than the staged copy reused, after anything that may have
changed the cart: an import, delete, rename or new folder from this window, a refresh of the Cart
pane (F5 or Refresh, or a serial port appearing or disappearing), or a change of serial port or
cart type, since the staged copies may then be from another card. A change made elsewhere — by
the console, or by Quick upload — is noticed only once the Cart pane is refreshed.

The main window sets `dragDropEnabled: true` (`tauri.conf.json`) — that is the only way Tauri
reports the dropped paths, and WebView2's own HTML5 drop reports none. It also switches HTML5
drag-and-drop off inside the webview on Windows, which is why the pane-to-pane drag is built on
pointer events and the outbound drag on [`tauri-plugin-drag`](https://crates.io/crates/tauri-plugin-drag).
See the drag-and-drop section comment at the top of [`src/explorer.js`](src/explorer.js) and
[`src-tauri/src/drag_out.rs`](src-tauri/src/drag_out.rs). The gestures are checked headlessly in
[`e2e/`](e2e/README.md) — that covers which backend command each drag reaches, not whether Windows
accepts the drag.

## Quick upload

Quick upload sends files to the cart without opening the main window: a small window lists the
cart's folders, and uploads the files it was given into the one you choose.

- **Send to.** Settings → **Windows integration** → **Add to Send to** puts *Xfer64 upload* in your
  Send to menu (right-click a file → Send to). It runs `xfer64 upload --picker` with the files.
  The button reads **Remove from Send to** once the shortcut is there.
- **Context menu.** [`windows/upload-to-sd.reg`](windows/upload-to-sd.reg) adds *Upload to SD card
  (Xfer64)* to every file's right-click menu, also through `--picker`, once its path is edited to
  point at `xfer64.exe`.

The window opens in the folder last opened in the Cart pane or in Quick upload (the card root if
there is none, or if that folder is gone). **Once the cart has listed it, the upload starts by
itself after 5 seconds** unless you open a subfolder, go up a folder, or press the × beside the
countdown (*Cancel automatic upload*); changing **Overwrite existing** restarts the countdown. After any of those, press **Upload here**. With no cart
connected it keeps checking, and the countdown starts once the cart answers. While the upload
runs, **Close** becomes **Cancel**, and closing the window cancels it too. **Overwrite existing**
is remembered between runs; the folder you end up in is saved for the next one.

Without `--picker`, `xfer64 upload` runs headless, with no window. See
[`windows/README-upload-shell.txt`](windows/README-upload-shell.txt) for its options, environment
variables and settings, or run `xfer64 upload --help`.

## Settings file

`%APPDATA%\multi64\xfer64-settings.json` holds the cart, serial port, EverDrive SD base, developer
mode and Quick upload's folder and Overwrite choice. A file that cannot be parsed is set aside as
`xfer64-settings.json.corrupt` and Xfer64 starts from defaults; an older
`cart-explorer-settings.json` is copied into place when there is no usable settings file.

## Appearance

Settings → **Appearance**: theme, text size and motion, applied as you pick them. Multi64, AP64
and Multi64 Test carry the same sheet and the same options, described in
[Multi64's README](../multi64/README.md#appearance).

## Windows drivers

Xfer64 does not ship third‑party cart drivers in the installer. For normal use it expects the cart to appear as a USB serial (COM) port using Windows’ inbox stack:

- **SummerCart64** — typically enumerates as a CDC serial device (`usbser.sys`). No vendor installer is required for COM‑based tools. Optional WinUSB (e.g. via [Zadig](https://zadig.akeo.ie/)) is mentioned in SC64 docs for alternative tooling / throughput experiments; replacing the default driver can break standard COM access, so do not use it unless you know you need it.

- **EverDrive 64 X‑series** — usually exposes USB serial for `usb64`‑style PC tools. If Windows shows an unknown USB device and no COM port appears, install Krikzz’s `usb64` driver package from their support files (see [EverDrive 64 X‑series dev/support](https://krikzz.com/pub/support/everdrive-64/x-series/dev/)), then replug the cart.

Bundling Krikzz or SC64 driver binaries inside Multi64/Xfer64 installers would require explicit redistribution permission from the vendors and ongoing updates whenever they ship new INF/USB IDs—we document manual install instead.

## Building from source

Build from the repository root (Cargo package `xfer64`):

```sh
cargo build -p xfer64 --release
cd crates/xfer64
npm install
npm run build
```

Installers and the app binary (e.g. `Xfer64.exe` on Windows) land under `target/release/` and `target/release/bundle/`. Xfer64 has its own installer; Multi64's installer does not include it.
