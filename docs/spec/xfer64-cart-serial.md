# Xfer64: SD over USB serial (maintainer)

**Spec-Revision:** 2  

How **Xfer64** uses the **COM** port for **SD** (FAT/exFAT over each cart’s protocol), not the L3 WebSocket path. Complements [`l3-over-sc64.md`](./l3-over-sc64.md), the [daemon API](daemon-api-v1.md) (serial **release** / **resume**), and [`ed64-sd-usb-host.md`](./ed64-sd-usb-host.md) for EverDrive wire details.

---

## Session lifecycle

Each logical SD operation runs inside [`with_session`](../../crates/xfer64/src-tauri/src/cart_serial_sd.rs): open serial → identify cart → init SD / session → work → teardown + host flush → close. The port is not held between Tauri commands so other tools can open COM when idle.

**Releasing the SD lock is an invariant of the session, not of the caller.** While a PC-side SD session is open the SC64 holds the card away from the console, which then refuses to boot (`SD card is locked by the PC side`). [`Sc64SdSession`](../../crates/multi64-sc64-sd/src/partition.rs) and `Ed64SdSession` therefore release on `Drop`, covering early returns, `?`, and panics in **any** consumer of [`CartSession`](../../crates/multi64-sc64-sd/src/cart_session.rs) — Xfer64, `sc64-sd-e2e`, and anything added later. `close` stays the way to release **and see the error**; it is idempotent, so an explicit `close` followed by the drop deinits once.

Two things `Drop` cannot cover, so callers must still get them right:

- **`std::process::exit` runs no destructors.** A tool must return an exit code up to `main` and let the session close first (`sc64-sd-e2e`'s `run`).
- **A failure inside `Sc64SdSession::open` after `SD_CARD_OP` init** has no `Self` to drop; `open` deinits before propagating.
- **Exiting the app kills a running copy thread without unwinding**, skipping both the release and the `multi64d` resume. [`close_guard.rs`](../../crates/xfer64/src-tauri/src/close_guard.rs) therefore holds a window close while any window reports work under way, and Xfer64 exits only once the last one reports idle, after the operation has ended and the bridge has been resumed.

**Panic safety (Xfer64):** `SdSessionCloseGuard` in `cart_serial_sd.rs` still wraps the work closure, so a close **error** is logged and surfaced to the UI rather than swallowed by a drop.

### Multi-file copy

Cart ↔ PC **multi-select** uses `cart_serial_export_copy_batch` / `cart_serial_import_copy_batch`: **one** SD session for the whole batch after overwrite prompts. Single-file copy is a batch of length 1; there are no per-file cart copy commands.

Each step's overwrite prompt is decided against the destination as it was before the batch, so two steps that write one destination would let the second replace the first unasked: two files of one name from different folders, as an Explorer search can turn up. The plan builders in [`copy_plan.rs`](../../crates/xfer64/src-tauri/src/copy_plan.rs) therefore refuse such a plan before anything is copied, comparing destinations as the card and Windows do, ignoring letter case, and both batch commands refuse such a batch themselves rather than trust the plan they are sent (`refuse_shared_destinations`). Two folders of one name merge; a file inside both is refused as a file.

An export writes each file through [`write_host_file`](../../crates/multi64-sc64-sd/src/host_file.rs): into a temporary file beside the destination, renamed over it only once flushed and synced, so a cancelled or failed **Replace** keeps the PC's original. Imports to an SC64 keep the cart's original the same way (#200); an EverDrive-64 PRO's link truncates the destination when it opens it, so there an interrupted Replace does lose the original.

An export from an SC64 or X-series card writes exactly the size the listing reports: an exFAT file's DataLength, with the bytes past its ValidDataLength written as zeros, not read. A FAT or exFAT cluster chain that ends before that size fails the export with `UnexpectedEof` rather than completing it short.

### multi64d coordination

Only one process can hold the cart serial device. `multi64d` exposes `POST /v1/serial/release` and `POST /v1/serial/resume` ([daemon API §1](daemon-api-v1.md)). Every cart read and write from a window goes through `withBridgePaused` ([`saved-cart-path.js`](../../crates/xfer64/src/saved-cart-path.js); the main window's `withCartDaemonYield` wraps it): when `multi64d` answers, **release** its COM, run the SD work, then **resume**. Only a copy (including a drag-out's staging), new folder or rename asks the user first (`withCartDaemonYield(fn, { confirm: true })`); listings, Properties, delete (which has its own confirmation) and the EverDrive SD base scan do not. Quick upload never asks: its listings go through `withBridgePaused` without a prompt, and its upload, like the headless `xfer64 upload`, pauses and resumes the bridge in the backend. No extra release inside each `with_session`.

### Listing vs copy

**Directory listing** (`cart_serial_list_dir_page`) uses `with_session` per **cache fill** (path change or refresh), not per UI page chunk. **Probe** (`probe_serial_cart`) identifies the cart only and does not start an SD session.

---

## Rust module map

### Application layer (`crates/xfer64`)

| Piece | Role |
|-------|------|
| [`cart_serial_sd.rs`](../../crates/xfer64/src-tauri/src/cart_serial_sd.rs) | COM port, cart mode (`auto` / `sc64` / `ed64_beta` / `ed64_pro`), `CartSdRole` (`Sc64` / `Ed64Linear` / `Ed64NoLinear` / `Ed64Pro`), `with_session` → [`CartSession`](../../crates/multi64-sc64-sd/src/cart_session.rs), Tauri IPC (`cart_serial_*`). |
| [`cart_probe.rs`](../../crates/xfer64/src-tauri/src/cart_probe.rs) | Auto-detect over [`multi64-cart-probe`](../../crates/cart-probe/README.md): an SC64 by its USB descriptors first (no writes, so it works while `multi64d` holds the port), then SC64 `IDENTIFIER_GET`, the EverDrive-64 PRO handshake at **921600**, and EverDrive `usb64` `cmd`/`t` at **115200** — except on an FTDI FT245R (`0403:6001`, the X7's USB bridge), where the `usb64` test goes first so the other probes' bytes are not already in its receive FIFO (#140). The `multi64d` yield check uses only a pinned port and never probes. Before any of that, Auto takes the port and cart a running `multi64d` reports (`GET /` `serial` and `cart`, [daemon API §1.1](./daemon-api-v1.md)) while that port is still plugged in. A port a probe identified is remembered across commands (`auto_detected_port` in `cart_serial_sd.rs`) while the set of serial ports stays the same; a port appearing or disappearing, a change of cart or port setting, or a session on it failing to open (which `with_session` then retries once) probes again. |
| [`copy_plan.rs`](../../crates/xfer64/src-tauri/src/copy_plan.rs) | Interactive copy plans; takes `&CartSession` for cart-side walks. |
| [`daemon.rs`](../../crates/xfer64/src-tauri/src/daemon.rs) | multi64d probe, release and resume (`explorer_daemon_*`) around cart work, and the cart hint Auto asks for (`daemon_cart_hint`). The port itself is chosen by `resolve_port` in `cart_serial_sd.rs`. |
| [`close_guard.rs`](../../crates/xfer64/src-tauri/src/close_guard.rs) | Holds a window close while any window reports work under way (`explorer_set_busy`), and exits once none does (`explorer_close_when_idle`). |
| [`dev_log.rs`](../../crates/xfer64/src-tauri/src/dev_log.rs) | `xfer64-settings.json`: `cartDevice`, `ed64RomLinearBase`, `preferredCom`, … |

### Library layer (`crates/multi64-sc64-sd`)

| Piece | Role |
|-------|------|
| [`link.rs`](../../crates/multi64-sc64-sd/src/link.rs) | [`SdCardTransport`](../../crates/multi64-sc64-sd/src/link.rs): sector read/write + flush. [`Sc64Link`](../../crates/multi64-sc64-sd/src/link.rs) (SC64) and, with `feature = "ed64"`, [`Ed64RomLinear`](../../crates/multi64-sc64-sd/src/ed64_linear.rs). |
| [`partition.rs`](../../crates/multi64-sc64-sd/src/partition.rs) | Partition discovery, FAT/exFAT sessions (`Sc64SdSession`, `Ed64SdSession`). |
| [`cart_session.rs`](../../crates/multi64-sc64-sd/src/cart_session.rs) | [`CartSession`](../../crates/multi64-sc64-sd/src/cart_session.rs): unified explorer API. |
| [`host_file.rs`](../../crates/multi64-sc64-sd/src/host_file.rs) | `write_host_file`: every PC-side file an export writes goes to a temporary file first and replaces the destination only when complete. |
| [`ed64_linear.rs`](../../crates/multi64-sc64-sd/src/ed64_linear.rs) | EverDrive: `RomRead` at `rom_linear_base + LBA×512` (optional feature). Reads cart ROM memory, not the SD card — see [`ed64-sd-usb-host.md`](./ed64-sd-usb-host.md#why-romread-is-not-sd-access). |
| [`ed64pro.rs`](../../crates/multi64-sc64-sd/src/ed64pro.rs) | EverDrive-64 PRO (optional `ed64pro` feature): a file-level session over edlink Gen3 — no sector reads, no host FAT mount. Rename copies, then deletes the original, since the link has no rename command. It deletes only once the original and the copy are both listed under their own exact names, compared without case folding, with the original's size, so a copy the cart wrote over the original under another letter case never costs the original; a change of letter case alone is refused. Experimental; see [`ed64-pro-usb-host.md`](./ed64-pro-usb-host.md). |

### Wire layer (`crates/multi64-ed64-link`)

- Legacy **X-series `usb64`** 16-byte `cmd`, `RomRead` / `RamRead` — [`Ed64Link`](../../crates/multi64-ed64-link/src/lib.rs) ([ed64-x-pub](https://github.com/krikzz/ed64-x-pub) / UNFLoader).

`multi64-sc64-sd` enables the `RomRead` SD experiment with its `ed64` feature.

### Settings (JSON)

- `cartDevice`: `auto` \| `sc64` \| `ed64_beta` \| `ed64_pro` (`ExplorerSettingsSnapshot` in `dev_log.rs`).
- `ed64RomLinearBase`: `u32` — base address for the EverDrive `RomRead` experiment, which reads cart ROM memory rather than the SD card ([why](./ed64-sd-usb-host.md#why-romread-is-not-sd-access)).

**EverDrive-64 PRO writes need consent.** Every write command — import, mkdir, rename, remove, and `xfer64 upload` — refuses a PRO session with an error starting `ED64PRO_WRITE_CONSENT_REQUIRED` until the user agrees. Explorer asks, then calls `cart_serial_allow_ed64pro_writes`, which lasts until the app exits. The upload picker asks with a native dialog, and the headless CLI requires `--experimental-ed64pro-writes`.
