# Test ROM connector (`multi64-test-connector`)

> [Doc map](../README.md) · [Flash carts](../README.md#flash-carts-l2-backends) · [Repo README](../../README.md)

Rust CLI: **`multi64_test.z64`** ([`n64/test-rom`](../../n64/README.md)) ↔ **`multi64d`** WebSocket. Payloads follow **[M64T](../spec/test-l3-application-v0.md)** inside L3 **`DATA` / `APPLICATION`**. Use ROM **M64T_PROTO** or **BENCH** (not **RAW_ECHO**).

**L2 today:** **`multi64d`** uses **SC64** ([`l3-over-sc64.md`](../spec/l3-over-sc64.md)). **EverDrive X7:** [`l3-over-everdrive-x7.md`](../spec/l3-over-everdrive-x7.md) / **`ed64-l2`** implements §4 but is **unvalidated on hardware**.

## Optional GUI

**`multi64-test-connector-gui`** — same commands over WebSocket; run **`multi64d`** (or Multi64) first. From **`crates/multi64-test-connector-gui/`**: `npm install`, `npm run dev`.

## Prerequisites

1. Cart + **`multi64d`** build you use (reference: **SC64** + USB) and **`multi64_test.z64`** on-console.  
2. PC: `cargo run -p multi64d --release -- --serial COM3`  
3. ROM mode **M64T_PROTO** or **BENCH** for the commands below.

## Commands

| Subcommand | Sends | Expects |
|------------|--------|---------|
| `ping` | `PING` (0x01) | `PONG` (0x81) |
| `echo` | `ECHO` (0x02) + body | `ECHO_REPLY` (0x82) |
| `version` | `REQ_VERSION` (0x03) | `VERSION` (0x83) |
| `req-controller` | `REQ_CONTROLLER` (0x04) | `CONTROLLER` (0x84) |
| `session-open` | `SESSION_OPEN` (0x05) + 8-byte `--hex-challenge` | `SESSION_ACK` (0x85) |
| `session-close` | `SESSION_CLOSE` (0x06) | `SESSION_END` (0x86) |
| `eeprom-info` | `REQ_EEPROM_INFO` (0x07) | `EEPROM_INFO` (0x87) |
| `eeprom-read` | `REQ_EEPROM_READ` (0x08) | `EEPROM_DATA` (0x88) or status |
| `eeprom-write` | `REQ_EEPROM_WRITE` (0x09) + `--hex` | `EEPROM_STATUS` (0x89); **requires session** |
| `sram-info` | `REQ_SRAM_INFO` (0x0A) | `SRAM_INFO` (0x8A) |
| `sram-read` | `REQ_SRAM_READ` (0x0B) | `SRAM_DATA` (0x8B) or status |
| `sram-write` | `REQ_SRAM_WRITE` (0x0C) + `--hex` | `SRAM_STATUS` (0x8C); **requires session** |
| `rumble` | `REQ_RUMBLE` (0x0D) + `--port` / `--frames` | `RUMBLE_ACK` (0x8D) |
| `display-text` | `REQ_DISPLAY_TEXT` (0x0E) + `--text` (UTF-8, max **120** bytes; empty clears) | `DISPLAY_TEXT_ACK` (0x8E) |
| `listen` | nothing | prints inbound L3 **APPLICATION** / **M64T** (e.g. **A** → **CONTROLLER**, **B** → **STRESS_LARGE**, **BENCH_TICK**) |

Global options: `--url` (default `ws://127.0.0.1:38765/ws`), `--recv-timeout-secs` (default `5` for request/response commands).

`listen --duration-secs 0` runs until Ctrl+C; otherwise stops after N seconds.

## Regression script (hardware)

With **`multi64d`** running and the test ROM in **M64T_PROTO** or **BENCH**, run the full connector smoke from the **repo root**:

| Host | Command |
|------|---------|
| Git Bash / Linux / macOS | `./scripts/test_rom_connector_e2e.sh` |
| Windows PowerShell | `.\scripts\test_rom_connector_e2e.ps1` |

Environment (optional): **`MULTI64_WS_URL`** (default `ws://127.0.0.1:38765/ws`), **`MULTI64_RECV_TIMEOUT_SECS`** (passed through to **`--recv-timeout-secs`**).

The script runs **`ping`**, **`version`**, **`echo`**, **`req-controller`**, **`rumble`** (port **0**, **60** frames), **`display-text`**, **`session-open`**, **`eeprom-info`**, **`eeprom-read`**, **`eeprom-write`**, **`sram-info`**, **`session-close`**, then **`listen`** for two seconds. **`sram-info`** still succeeds when the ROM was built for EEPROM only (size **0**). GitHub **CI** does not run this (no cart); it only builds and tests the Rust workspace.

## Examples

```sh
cargo run -p multi64-test-connector -- ping
cargo run -p multi64-test-connector -- echo --text hello
cargo run -p multi64-test-connector -- version
cargo run -p multi64-test-connector -- req-controller
cargo run -p multi64-test-connector -- session-open
cargo run -p multi64-test-connector -- eeprom-info
cargo run -p multi64-test-connector -- eeprom-read --offset 0 --len 16
cargo run -p multi64-test-connector -- sram-info
cargo run -p multi64-test-connector -- rumble --port 0 --frames 60
cargo run -p multi64-test-connector -- display-text --text "hello from host"
cargo run -p multi64-test-connector -- listen
```

Press **A** for **CONTROLLER** (9-byte body including port), **B** for **STRESS_LARGE**, and in **BENCH** mode wait for **BENCH_TICK** lines. Non-**APPLICATION** L3 (e.g. **C-up** / **Start** on the ROM) may appear as non-**M64T** payloads depending on host decoding.

Session, save, rumble, and display-text commands match **[`test-l3-application-v0.md`](../spec/test-l3-application-v0.md)** (**Spec-Revision** **1**). Default ROM build is **EEPROM**; rebuild the ROM with **`N64_ROM_SAVETYPE=sram256k`** (etc.) to get a non-zero SRAM window for **`sram-*`** commands.

Raw L3 loopback tests (**test ROM RAW_ECHO** mode — default at boot) use **serial L2** e2e tools on the USB port, not this WebSocket connector:

| Backend | Binaries |
|---------|----------|
| SummerCart64 (reference) | `sc64-l3-framing-e2e`, `sc64-echo-test` |
| EverDrive X7 (when `multi64-ed64-l2` works) | `ed64-l3-framing-e2e`, `ed64-echo-test` |

The **`sc64-*`** crate names reflect the **reference** L2 implementation today.
