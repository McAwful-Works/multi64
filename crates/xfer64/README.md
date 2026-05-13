# Xfer64 (Windows)

Dual-pane **SD card** access over **USB serial** (FAT/exFAT via [`multi64-sc64-sd`](../multi64-sc64-sd)) — no Windows drive letter; one COM port at a time.

- **SummerCart64:** vendor SD / memory ops (see SC64 USB documentation).
- **EverDrive:** **edlink** on PRO/CORE (921600); **X-series** uses legacy **`usb64`** **`RomRead`** at `ed64RomLinearBase + LBA×512`. Use **Scan for SD base** if auto-open fails — [ed64-sd-usb-host.md](../../docs/spec/ed64-sd-usb-host.md).

**Auto-detect:** SC64 identify first, then EverDrive (**edlink** @ 921600 or **`usb64`** `cmd`/`t` @ 115200). Override in Settings if needed.

**Maintainer doc:** [xfer64-cart-serial.md](../../docs/spec/xfer64-cart-serial.md).

## Drivers (Windows)

Expect a **COM** port. **SC64:** usually inbox CDC (`usbser.sys`). **EverDrive X-series:** if no COM appears, install Krikzz’s **`usb64`** driver from [X-series dev/support](https://krikzz.com/pub/support/everdrive-64/x-series/dev/). Optional WinUSB/Zadig can break normal COM access — only if you know you need it. Installers here do **not** redistribute vendor drivers.

## Build

```sh
cargo build -p xfer64 --release
cd crates/xfer64
npm install
npm run build
```

Output: `target/release/` and `target/release/bundle/`. **Multi64** can launch Xfer64 when the exe is next to `multi64.exe` or under `%LOCALAPPDATA%\multi64\` from a bundle.
