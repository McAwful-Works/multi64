# Xfer64: SD over USB serial (maintainer)

**Spec-Revision:** 2  

How **Xfer64** uses the **COM** port for **SD** (FAT/exFAT over each cart’s protocol), not the L3 WebSocket path. Complements [`l3-over-sc64.md`](./l3-over-sc64.md), the [daemon API](daemon-api-v1.md) (serial **release** / **resume**), and [`ed64-sd-usb-host.md`](./ed64-sd-usb-host.md) for EverDrive wire details.

---

## Session lifecycle

Each logical SD operation runs inside [`with_session`](../../crates/xfer64/src-tauri/src/cart_serial_sd.rs): open serial → identify cart → init SD / session → work → teardown + host flush → close. The port is not held between Tauri commands so other tools can open COM when idle.

**Releasing the SD lock is an invariant of the session, not of the caller.** While a PC-side SD session is open the SC64 holds the card away from the console, which then refuses to boot (`SD card is locked by the PC side`). [`Sc64SdSession`](../../crates/multi64-sc64-sd/src/partition.rs) and `Ed64SdSession` therefore release on **`Drop`**, covering early returns, `?`, and panics in **any** consumer of [`CartSession`](../../crates/multi64-sc64-sd/src/cart_session.rs) — Xfer64, `sc64-sd-e2e`, and anything added later. `close` stays the way to release **and see the error**; it is idempotent, so an explicit `close` followed by the drop deinits once.

Two things `Drop` cannot cover, so callers must still get them right:

- **`std::process::exit` runs no destructors.** A tool must return an exit code up to `main` and let the session close first (`sc64-sd-e2e`'s `run`).
- **A failure inside `Sc64SdSession::open` after `SD_CARD_OP` init** has no `Self` to drop; `open` deinits before propagating.

**Panic safety (Xfer64):** `SdSessionCloseGuard` in `cart_serial_sd.rs` still wraps the work closure, so a close **error** is logged and surfaced to the UI rather than swallowed by a drop.

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
| [`cart_serial_sd.rs`](../../crates/xfer64/src-tauri/src/cart_serial_sd.rs) | COM port, cart mode (`auto` / `sc64` / `ed64_beta` / `ed64_pro`), `CartSdRole` (`Sc64` / `Ed64Linear` / `Ed64NoLinear` / `Ed64Pro`), `with_session` → [`CartSession`](../../crates/multi64-sc64-sd/src/cart_session.rs), Tauri IPC (`cart_serial_*`). |
| [`cart_probe.rs`](../../crates/xfer64/src-tauri/src/cart_probe.rs) | Auto-detect: SC64 `IDENTIFIER_GET`, then the EverDrive-64 PRO handshake at **921600**, then EverDrive **`usb64`** `cmd`/`t` at **115200**. |
| [`copy_plan.rs`](../../crates/xfer64/src-tauri/src/copy_plan.rs) | Interactive copy plans; takes `&CartSession` for cart-side walks. |
| [`daemon.rs`](../../crates/xfer64/src-tauri/src/daemon.rs) | `resolve_com_port` / multi64d yield around cart work. |
| [`dev_log.rs`](../../crates/xfer64/src-tauri/src/dev_log.rs) | `xfer64-settings.json`: `cartDevice`, `ed64RomLinearBase`, `preferredCom`, … |

### Library layer (`crates/multi64-sc64-sd`)

| Piece | Role |
|-------|------|
| [`link.rs`](../../crates/multi64-sc64-sd/src/link.rs) | [`SdCardTransport`](../../crates/multi64-sc64-sd/src/link.rs): sector read/write + flush. [`Sc64Link`](../../crates/multi64-sc64-sd/src/link.rs) (SC64) and, with `feature = "ed64"`, [`Ed64RomLinear`](../../crates/multi64-sc64-sd/src/ed64_linear.rs). |
| [`partition.rs`](../../crates/multi64-sc64-sd/src/partition.rs) | Partition discovery, FAT/exFAT sessions (`Sc64SdSession`, `Ed64SdSession`). |
| [`cart_session.rs`](../../crates/multi64-sc64-sd/src/cart_session.rs) | [`CartSession`](../../crates/multi64-sc64-sd/src/cart_session.rs): unified explorer API. |
| [`ed64_linear.rs`](../../crates/multi64-sc64-sd/src/ed64_linear.rs) | EverDrive: `RomRead` at `rom_linear_base + LBA×512` (optional feature). Reads cart ROM memory, not the SD card — see [`ed64-sd-usb-host.md`](./ed64-sd-usb-host.md#why-romread-is-not-sd-access). |
| [`ed64pro.rs`](../../crates/multi64-sc64-sd/src/ed64pro.rs) | EverDrive-64 PRO (optional `ed64pro` feature): a file-level session over edlink Gen3 — no sector reads, no host FAT mount. Rename is unsupported. Experimental; see [`ed64-pro-usb-host.md`](./ed64-pro-usb-host.md). |

### Wire layer (`crates/multi64-ed64-link`)

- Legacy **X-series `usb64`** 16-byte **`cmd`**, **`RomRead`** / **`RamRead`** — [`Ed64Link`](../../crates/multi64-ed64-link/src/lib.rs) ([ed64-x-pub](https://github.com/krikzz/ed64-x-pub) / UNFLoader).

`multi64-sc64-sd` enables the **`RomRead`** SD experiment with its **`ed64`** feature.

### Settings (JSON)

- **`cartDevice`**: `auto` \| `sc64` \| `ed64_beta` \| `ed64_pro` (`ExplorerSettingsSnapshot` in `dev_log.rs`).
- **`ed64RomLinearBase`**: `u32` — base address for the EverDrive **`RomRead`** experiment, which reads cart ROM memory rather than the SD card ([why](./ed64-sd-usb-host.md#why-romread-is-not-sd-access)).

**EverDrive-64 PRO writes need consent.** Every write command — import, mkdir, rename, remove, and `xfer64 upload` — refuses a PRO session with an error starting `ED64PRO_WRITE_CONSENT_REQUIRED` until the user agrees. Explorer asks, then calls `cart_serial_allow_ed64pro_writes`, which lasts until the app exits. The upload picker asks with a native dialog, and the headless CLI requires `--experimental-ed64pro-writes`.
