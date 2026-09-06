# Xfer64: SD over USB serial (maintainer)

**Spec-Revision:** 2  

How **Xfer64** uses the **COM** port for **SD** (FAT/exFAT over each cart’s protocol), not the L3 WebSocket path. Complements [`l3-over-sc64.md`](./l3-over-sc64.md), the [daemon API](daemon-api-v1.md) (serial **release** / **resume**), and [`ed64-sd-usb-host.md`](./ed64-sd-usb-host.md) for EverDrive wire details.

---

## Session lifecycle

Each logical SD operation runs inside [`with_session`](../../crates/xfer64/src-tauri/src/cart_serial_sd.rs): open serial → identify cart → init SD / session → work → teardown + host flush → close. The port is not held between Tauri commands so other tools can open COM when idle.

**Panic safety:** `SdSessionCloseGuard` ensures [`CartSession`](../../crates/multi64-sc64-sd/src/cart_session.rs)::`close` runs if the work closure unwinds, so the cart is less likely to stay SD-locked without host cleanup.

### Multi-file copy

Cart ↔ PC **multi-select** uses **`cart_serial_export_copy_batch`** / **`cart_serial_import_copy_batch`**: **one** SD session for the whole batch after overwrite prompts. Single-file copy is a batch of length 1. Per-file `cart_serial_*_copy_one` commands remain for compatibility.

### multi64d coordination

Only one process can hold the cart serial device. **`multi64d`** exposes `POST /v1/serial/release` and `POST /v1/serial/resume` ([daemon API §1](daemon-api-v1.md)). Xfer64 wraps cart work in **`withCartDaemonYield`**: prompt the user, **release** the daemon’s COM, run SD work, then **resume**. No extra release inside each `with_session`.

### Listing vs copy

**Directory listing** (`cart_serial_list_dir_page`) uses `with_session` per **cache fill** (path change or refresh), not per UI page chunk. **Probe** (`probe_serial_cart`) identifies the cart only and does not start an SD session.

---

## Rust module map

### Application layer (`crates/xfer64`)

| Piece | Role |
|-------|------|
| [`cart_serial_sd.rs`](../../crates/xfer64/src-tauri/src/cart_serial_sd.rs) | COM port, cart mode (`auto` / `sc64` / `ed64_beta`), `CartSdRole` (`Sc64` / `Ed64Linear` / `Ed64NoLinear`), `with_session` → [`CartSession`](../../crates/multi64-sc64-sd/src/cart_session.rs), Tauri IPC (`cart_serial_*`). |
| [`cart_probe.rs`](../../crates/xfer64/src-tauri/src/cart_probe.rs) | Auto-detect: SC64 `IDENTIFIER_GET`, then EverDrive **`usb64`** `cmd`/`t` at **115200**. |
| [`copy_plan.rs`](../../crates/xfer64/src-tauri/src/copy_plan.rs) | Interactive copy plans; takes `&CartSession` for cart-side walks. |
| [`daemon.rs`](../../crates/xfer64/src-tauri/src/daemon.rs) | `resolve_com_port` / multi64d yield around cart work. |
| [`dev_log.rs`](../../crates/xfer64/src-tauri/src/dev_log.rs) | `xfer64-settings.json`: `cartDevice`, `ed64RomLinearBase`, `preferredCom`, … |

### Library layer (`crates/multi64-sc64-sd`)

| Piece | Role |
|-------|------|
| [`link.rs`](../../crates/multi64-sc64-sd/src/link.rs) | [`SdCardTransport`](../../crates/multi64-sc64-sd/src/link.rs): sector read/write + flush. [`Sc64Link`](../../crates/multi64-sc64-sd/src/link.rs) (SC64) and, with `feature = "ed64"`, [`Ed64RomLinear`](../../crates/multi64-sc64-sd/src/ed64_linear.rs). |
| [`partition.rs`](../../crates/multi64-sc64-sd/src/partition.rs) | Partition discovery, FAT/exFAT sessions (`Sc64SdSession`, `Ed64SdSession`). |
| [`cart_session.rs`](../../crates/multi64-sc64-sd/src/cart_session.rs) | [`CartSession`](../../crates/multi64-sc64-sd/src/cart_session.rs): unified explorer API. |
| [`ed64_linear.rs`](../../crates/multi64-sc64-sd/src/ed64_linear.rs) | EverDrive: `RomRead` at `rom_linear_base + LBA×512` (optional feature). |

### Wire layer (`crates/multi64-ed64-link`)

- Legacy **X-series `usb64`** 16-byte **`cmd`**, **`RomRead`** / **`RamRead`** — [`Ed64Link`](../../crates/multi64-ed64-link/src/lib.rs) ([ed64-x-pub](https://github.com/krikzz/ed64-x-pub) / UNFLoader).

`multi64-sc64-sd` enables linear **`RomRead`** SD with its **`ed64`** feature.

### Settings (JSON)

- **`cartDevice`**: `auto` \| `sc64` \| `ed64_beta` (`ExplorerSettingsSnapshot` in `dev_log.rs`).
- **`ed64RomLinearBase`**: `u32` — required for EverDrive SD via experimental linear **`RomRead`**.
