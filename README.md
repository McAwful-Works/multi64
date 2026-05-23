# Multi64

**PC ↔ Nintendo 64** over USB flash carts: cart-agnostic **L3** framing, per-cart **L2** on USB/serial, and a reference **`multi64d`** daemon (HTTP + WebSocket → raw L3 octets).

| Piece | Spec |
|-------|------|
| **L3** | [`docs/spec/l3-bridge-protocol-v1.md`](docs/spec/l3-bridge-protocol-v1.md) — `M64B`, sessions, channels |
| **L2** | [`docs/spec/l2-link-adapter.md`](docs/spec/l2-link-adapter.md) — how L3 maps to each cart |
| **Daemon** | [`docs/spec/daemon-api-v1.md`](docs/spec/daemon-api-v1.md) — `multi64d` |

**Status:** **`multi64d`** ships with **SummerCart64** L2 ([`l3-over-sc64.md`](docs/spec/l3-over-sc64.md)). **EverDrive-64 X7** L2 is still a **draft + stub** ([`l3-over-everdrive-x7.md`](docs/spec/l3-over-everdrive-x7.md), crate **`multi64-ed64-l2`**).

**Windows:** [**Multi64**](crates/multi64/README.md) (start/stop daemon, COM port, tray). [**Xfer64**](crates/xfer64/README.md) (SD card over USB; no drive letter).  
**Developers:** [`docs/README.md`](docs/README.md) (spec map, reading order). [`CONTRIBUTING.md`](CONTRIBUTING.md) (build, layout).

---

## Quick start

```sh
cd multi64
cargo test --workspace
cargo run -p multi64d --release -- --serial COM3   # or /dev/ttyACM0
```

- **HTTP:** `http://127.0.0.1:38765/` · **WebSocket:** `ws://127.0.0.1:38765/ws` — frames are **L3** bytes. Long-running installs: [`docs/run-as-service.md`](docs/run-as-service.md).

---

## Repository layout

| Path | Contents |
|------|----------|
| [`crates/`](crates/) | Rust workspace: L3, SC64/ED crates, `multi64d`, smoke/e2e, **Multi64** & **Xfer64** (Tauri) |
| [`docs/spec/`](docs/spec/) | Normative protocols |
| [`docs/connectors/`](docs/connectors/) | Host programs that use `multi64d` + L3 |
| [`n64/test-rom/`](n64/test-rom/) | Builds **`multi64_test.z64`** — see [`n64/README.md`](n64/README.md) |
| [`scripts/`](scripts/) | Optional Python WebSocket smoke + shell e2e helpers |

---

## Workspace crates

### Core & apps

| Crate | Role |
|-------|------|
| `multi64-l3` | L3 framing (`M64B`), `StreamDecoder`, session helpers |
| `multi64d` | Daemon on `127.0.0.1:38765` → L3 over **SC64 L2** today |
| `multi64-test-connector` | CLI ↔ **`multi64_test.z64`** ([`docs/connectors/test-rom.md`](docs/connectors/test-rom.md)) |
| `multi64-sc64-sd` | FAT/exFAT over SC64 USB; optional EverDrive linear `RomRead` — Xfer64 / tooling |
| `multi64-ed64-link` | EverDrive X-series **`usb64`** serial (`RomRead` / `RamRead`) |
| `multi64` (**Multi64**), `xfer64` (**Xfer64**) | Windows Tauri apps — READMEs under [`crates/multi64`](crates/multi64), [`crates/xfer64`](crates/xfer64) |
| `multi64-test-connector-gui` | Optional GUI; same WebSocket contract as the CLI ([`test-rom.md`](docs/connectors/test-rom.md)) |

### SummerCart64

| Crate | Role |
|-------|------|
| `multi64-sc64-link` | `CMD` / `CMP` / `PKT`, `USB_WRITE` helpers |
| `multi64-sc64-l2` | L3 stream over SC64 — [`l3-over-sc64.md`](docs/spec/l3-over-sc64.md) |
| `sc64-smoke` | Vendor `IDENTIFIER` / `VERSION` |
| `sc64-echo-test`, `sc64-l3-framing-e2e` | Serial e2e vs ROM **RAW_ECHO** |

### EverDrive (X7)

| Crate | Role |
|-------|------|
| `multi64-ed64-l2` | L3 over ED USB — **stub** |
| `ed64-smoke` | **`usb64`** `cmd`/`t` smoke ([spec §8](docs/spec/l3-over-everdrive-x7.md)); not L3 |
| `ed64-echo-test`, `ed64-l3-framing-e2e` | Same roles as SC64 e2e tools; need **`Ed64L2Pipe`** (blocked until `ed64-l2` is real) |

**Compare carts:** [`docs/README.md` — Flash carts (L2)](docs/README.md#flash-carts-l2-backends)

---

## Test ROM and checks

Build **[`n64/test-rom/`](n64/test-rom/)** → **`multi64_test.z64`** (needs **`N64_INST`**). Modes **RAW_ECHO** (default), **M64T_PROTO**, **BENCH** — [`test-l3-application-v0.md`](docs/spec/test-l3-application-v0.md).

**Serial (SC64, RAW_ECHO):** `cargo run -p sc64-l3-framing-e2e --release -- --port COM3` (add `--large` for multi-chunk USB). **EverDrive:** same ROM; ED e2e crates compile but **`Ed64L2Pipe::open`** is still unsupported.

**Via `multi64d`:** ROM in **M64T_PROTO** or **BENCH**, then e.g. `cargo run -p multi64-test-connector -- ping`. Optional: `pip install -r scripts/requirements.txt` and **`scripts/multi64_ws_test.py`** for HTTP/WebSocket smoke.

**Vendor smoke:** `cargo run -p sc64-smoke -- --port COM5` · `cargo run -p ed64-smoke -- --port COM5` (EverDrive: try `--baud 57600 --flush` if the probe stalls).

---

## License

**Multi64** is released under **SPDX** `MIT OR Apache-2.0` — same pattern as the Rust compiler and standard library: pick **either** [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE). Summary: [LICENSE](LICENSE). Apache **NOTICE**: [NOTICE](NOTICE). See [SECURITY.md](SECURITY.md) for sensitive issues.
