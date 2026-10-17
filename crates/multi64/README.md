<img src="../../branding/multi64.svg" alt="" width="72" align="left" />

# Multi64

**N64 flash cart bridge** · Windows

<br clear="left" />

Multi64 runs the **bridge**: the link between a flash cart on USB and the tools on this PC.
Pick a serial port, press **Start**, and leave it in the tray. [Xfer64](../xfer64/README.md)
and [AP64](../ap64/README.md) talk to a cart through it, and each is installed on its own —
Multi64's installer carries only Multi64 and `multi64d`.

A native L3 client, such as `multi64-test-connector`, can use the same WebSocket AP64 does; the
daemon it runs is specified in [`daemon-api-v1.md`](../../docs/spec/daemon-api-v1.md). A browser
page cannot unless its origin is allowed: it sends an `Origin` header, and the daemon answers `403`
to any origin it was not told to allow ([§1.4](../../docs/spec/daemon-api-v1.md#14-origin-policy)). The daemon Multi64
starts allows none. It runs on what Multi64 passes it: the serial port, baud, cart and listen
address on the command line, and `--no-config-file`, so it loads neither a `multi64d.toml` nor
`%APPDATA%\multi64d\config.toml` ([§5](../../docs/spec/daemon-api-v1.md#5-cli-and-configuration-reference)).
Every `MULTI64D_*` variable is removed from its environment, and it runs in its own directory. A
config file is for a daemon you run yourself, as in
[Running `multi64d` as a background service](../../docs/run-as-service.md); stop Multi64's bridge
first, since both want the cart's serial port and the listen address.

## Status

| Row | |
|-----|---|
| **Bridge** | **Running**, **Starting…** while a start is under way (Auto-detect can take a few seconds per serial port), or **Stopped** |
| **Cart** | While running, the cart the process was started for and the port it holds (`SummerCart64 on COM4`); while stopped, the Cart setting |
| **Health** | **OK** when the bridge answers `GET /` and holds the cart's serial port (`serialActive`, [daemon API §1.1](../../docs/spec/daemon-api-v1.md#1-transport)). **No cart link** when it answers without the port: either the cart is not connected on that port and the bridge keeps trying to open it, or a program such as Xfer64 has paused the bridge to use the cart. `GET /` does not say which, so Note names both. **Not responding yet** when it does not answer |
| **Listen** | The Listen setting |
| **Note** | While stopped, what **Start bridge** would do or why it cannot, and what auto-start is waiting for; while running, why there is no cart link |

**Start bridge** is offered only while the bridge is stopped and no start is under way: a second
start would kill the daemon the first one spawned. **Stop bridge** is offered only while it runs.

## Settings & tray

- Settings: `%APPDATA%\multi64\gui-settings.json`. A file that cannot be parsed is renamed to `gui-settings.json.corrupt` beside it and Multi64 starts from the defaults, so the old settings can be recovered by hand rather than overwritten by the next Save. A file from an older build that lacks newer settings keeps the ones it has.
- **Cart** picks the cart `multi64d` is started for ([daemon API §5.1](../../docs/spec/daemon-api-v1.md) `--cart`): **Auto-detect** (default), **SummerCart64**, **EverDrive-64 X7 (beta)** or **EverDrive-64 PRO (beta)**. Neither EverDrive mapping is validated: the X7's has run on one cart and the PRO's on none ([X7 §4.5](../../docs/spec/l3-over-everdrive-x7.md), [PRO §8](../../docs/spec/l3-over-everdrive-pro.md)), so the option, the status panel and the tray mark it *beta*, the Cart hint and the daemon log say it is experimental, and a running daemon is no evidence the cart link works. The PRO runs at its fixed 921600 baud, so **Baud** does not apply to it. Changing the cart restarts a running daemon. Settings files from before the Cart setting read as Auto-detect; a saved cart is kept.
- **Auto-detect** decides at each start, then starts `multi64d` for the cart it found; the daemon itself has no auto mode. First it looks for a SummerCart64 by its USB descriptors, as **Serial port → Auto-detect** does, which sends nothing. Failing that, it sends cart test commands ([`multi64-cart-probe`](../cart-probe/README.md): SC64 `IDENTIFIER_GET`, the PRO's edlink handshake, the X-series `usb64` test) to the chosen serial port, or with the port on Auto-detect to every serial port, USB devices first, until one answers. **Other devices on those ports receive those bytes**, a port another program holds cannot be probed, and the EverDrive checks have never been run against a cart. The status panel shows the cart it chose and the port it is on, and the log says Auto-detect chose it and lists each port it tried.
- **Serial port → Auto-detect** picks only a port whose USB descriptors identify a SummerCart64: FTDI `0403:6014` with an `SC64…` serial number or product string. Nothing is written to a port to find out. Other serial devices are never chosen, and with two carts plugged in neither is: the port stays unset, Settings and the status line say why, and the daemon does not start. Pick a port explicitly to use anything else.
- **With a fixed EverDrive selected, Serial port → Auto-detect picks nothing.** The X7's FT245R (`0403:6001`) is a stock FTDI part with nothing cart-specific in its descriptors, and a PRO can only be recognized by its edlink handshake, which means writing to each port. A fixed cart never probes, and never hands an EverDrive the SC64's port: choose the EverDrive's serial port, or set **Cart** to **Auto-detect**.
- **Baud** (default 115200) is passed as `multi64d --baud`. The PRO ignores it.
- **Listen** (default `127.0.0.1:38765`) is the address `multi64d` serves HTTP and the WebSocket on, as `host:port`. A leading `http://` and a trailing `/` are dropped; Save refuses anything that is not `host:port` with a port from 1 to 65535, such as an address with no port, port 0, `https://` or a path; a host name is not looked up. A wildcard (`0.0.0.0`, `[::]`) is checked on loopback, since Windows will not connect to a wildcard.
- **Saving restarts a running bridge** when the serial port, baud, cart, Listen or **Bridge logging** changed: they are command-line arguments fixed when `multi64d` starts. A Save made while a start is under way is applied by that start, which starts again with what was saved.
- **Start the bridge when Multi64 opens** (default on) starts it as **Start bridge** would. When that fails, for instance because the cart is unplugged at sign-in or its USB has not appeared yet, Multi64 keeps looking every 2 seconds until something has started the bridge, Multi64 exits, or the setting is turned off. A look only lists the serial ports, which sends nothing. A failed start is tried again only when something it depended on changed: a SummerCart64 found by its USB IDs, the chosen port appearing, or, for an Auto-detect probe of every port, a new serial port appearing. So the probe's test commands are never repeated on a timer. While the Listen address is taken by something else, or Start cannot work at all (the cases under **Tray** below), auto-start does not try; it logs why once and starts the bridge when that clears. Once anything has started the bridge, auto-start is done, and a bridge you stop stays stopped. With the cart and port already known, the bridge is started at once even if the cart is unplugged, since `multi64d` waits for it ([daemon API §1.3.1](../../docs/spec/daemon-api-v1.md#131-a-missing-cart-at-startup-is-not-fatal)).
- **Start Multi64 when you sign in to Windows** uses [`auto-launch`](https://crates.io/crates/auto-launch) to write a Run entry named `Multi64`, with the program path in quotes. While it is on, each launch rewrites the entry with the current path, so one from an older build or an install that has moved is corrected. Multi64 still starts `multi64d` as a child when **Start the bridge when Multi64 opens** is on; it is not a Windows Service.
- **Use notification area (system tray)** (default on) shows the tray icon. Saving creates or removes it. If the icon cannot be created, the settings are still saved and the log says so.
- **Minimize to tray when closing the window** (default on): closing hides the window to the tray instead of exiting, but only while the tray icon is there to bring it back; without one, closing exits. Exit from the tray with **Exit Multi64**.
- **Start with the window hidden** (default off) opens Multi64 in the tray only. It needs the tray, and is turned off with it.
- **A second launch** does not start another Multi64: it shows, unminimizes and focuses the window of the one running.
- **Developer mode** (default off) adds the Developer card, with the last 400 lines of `multi64d`'s output, and the **Bridge logging** setting: **Default** (info), **Debug** (the cart link crates and `multi64d` at debug), **Serial trace** (`--serial-trace`: every cart read, with its bytes) or **Verbose** (`multi64d` at debug, plus Serial trace). Multi64 removes `RUST_LOG` and every `MULTI64D_*` variable, `MULTI64D_SERIAL_TRACE` among them, from the daemon's environment, so the preset alone decides.

### Tray

**Double-click** the tray icon to raise the window. **Right-click** opens the menu:

| Item | |
|------|---|
| `Bridge: …` | Status line, disabled. While running, names the port the daemon was actually started on (not what the settings would pick now), else the listen address, and adds `, no cart link` when the daemon last said it does not hold the cart's serial port, as the window's **No cart link**. Every 5 seconds Multi64 asks each daemon it started, and rebuilds the menu when the answer changes, because the menu itself cannot make a request that blocks each time it is rebuilt. `Bridge: starting…` while a start is under way. While stopped, `(listen address in use)` when something else already listens there, and `(no serial port)` when **Start** cannot work. Then the cart: while running, the cart the process was started for unless it is the SummerCart64 (`· EverDrive-64 X7 (beta)`); while stopped, the Cart setting unless it is SummerCart64, so the default reads `· Auto-detect` |
| **Start / Stop bridge** | One item, whichever applies. **Start** is disabled while a start is under way, and when it cannot work: two SummerCart64s with Serial port on Auto-detect, Cart on SummerCart64 with no port chosen and none found on USB, or a fixed EverDrive with no port chosen. **Stop** is always enabled, since the port can disappear while the daemon runs. **Start** stays enabled while the listen address is taken, as the window's Start button does: the menu is only rebuilt when the bridge changes, so a grayed item would stay grayed after the other process exits |
| **Restart bridge** | Disabled while stopped — that case is **Start** |
| **Show window**, **Exit Multi64** | |

The menu tracks state live: starting or stopping the bridge from the window updates the tray, and vice versa.

**Left-click does not open the menu.** It cannot — the first click of a double-click would pop it, making the double-click unusable. This matches Windows convention, where left-click activates and right-click menus.

### Restarting while Xfer64 is using the cart

Xfer64 pauses the bridge while it works with the cart ([daemon API §1.2](../../docs/spec/daemon-api-v1.md#12-serial-yield-xfer64)). That pause lives in the daemon process, so a bridge restarted meanwhile, by a Save that changes its settings or by **Restart bridge**, knows nothing of it. The new daemon cannot open the port Xfer64 holds, so it starts without the cart and tries the port again every second, and it can take the port between two of Xfer64's commands, failing the rest of what Xfer64 was doing. Do not restart the bridge while Xfer64 is copying or listing files on the cart.

## Appearance

Settings → **Appearance**. Changes apply immediately; there is no Save step for them. Xfer64,
AP64 and Multi64 Test carry the same sheet and the same three options.

| Option | Values |
|--------|--------|
| **Theme** | Dark · Light · **Match system** (default) · High contrast |
| **Text size** | 90% · 100% · 115% · 130% |
| **Motion** | **Match system** (default) · Reduce animation |

**Match system** follows the OS via `prefers-color-scheme`. **High contrast** is a darker, higher-contrast variant with solid borders; all four themes meet WCAG AA for text contrast.

**Text size** scales the whole UI, not just the glyphs — every dimension in these sheets is in `rem`, and the setting drives the root font size.

**The window fits the page.** It is 560 pixels wide and cannot be resized by hand; `tauri.conf.json` opens it 400 pixels tall, and the page then sets its height to what it shows (`fit_window_height`), within 240 to 1600 pixels. It grows with **Developer mode**, which adds the Developer card, and with a larger **Text size**, and shrinks back when they are turned off. While a dialog is open the window is at least 640 pixels tall, room for the dialog to scroll in, and it returns to the page's height when the dialog closes. The log keeps a fixed height and scrolls inside, however much is logged.

**Motion** honors the OS reduced-motion setting by default; **Reduce animation** forces it for systems that do not expose one. Animation collapses to 1ms rather than being removed, so `animationend` / `transitionend` still fire.

These preferences live in `localStorage`, *not* in the settings file — they must be readable synchronously before the first paint to avoid a flash of the wrong theme, and they are per-machine display choices rather than device configuration. Do not look for them in `gui-settings.json` or `xfer64-settings.json`.

## Building from source

Rust + Cargo, Node + npm, **WebView2** (current Windows 10/11). Build `multi64d` before the GUI so `multi64d.exe` is found next to the app (`cargo build -p multi64d`).

### Develop

```sh
cargo build -p multi64d
cd crates/multi64
npm install
npm run dev
```

### Release (Windows)

```sh
cargo build -p multi64d --release
cd crates/multi64 && npm install && npm run build
```

**Both installers stop `multi64d` before touching files**, on install and uninstall — NSIS via `NSIS_HOOK_PREINSTALL` / `PREUNINSTALL`, MSI via a custom action sequenced before `InstallValidate`. Without it a daemon still running holds `resources\multi64d.exe` open and the install fails on a locked file. Each daemon Multi64 starts is in a Windows job object that kills it when Multi64's process ends, however it ends, so when the installer terminates Multi64 the daemon goes too. One can still be left running by a Multi64 from before the job object, by a start whose job assignment failed (the log says so), or by a `multi64d` started by hand, so the installers still stop it. Both match by **image name**, so a `multi64d` you are running from `cargo run` is killed too.

Installer graphics come from `windows/*.bmp`, regenerated from the brand masters by [`branding/installer-images`](../../branding/installer-images/README.md).

Artifacts under `target/release/bundle/`: an NSIS `*-setup.exe` and an MSI. `src-tauri/build.rs` copies `multi64d.exe` into `src-tauri/resources/`, and [Tauri `bundle.resources`](https://v2.tauri.app/reference/config/#bundle) declares it; that is the only resource. Build `multi64d` with the same profile first — a declared resource that is missing is a hard error in `tauri-build`. The copy is made again whenever `target/<profile>/multi64d.exe` changes, a copy that fails fails the build, and when the daemon is missing but an earlier copy is still in `resources/`, the build warns that it may be stale.
