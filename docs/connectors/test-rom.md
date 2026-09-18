# Test ROM connector (`multi64-test-connector`)

> [Doc map](../README.md) · [Flash carts](../README.md#flash-carts-l2-backends) · [Repo README](../../README.md)

Rust CLI: **`multi64_test.z64`** ([`n64/test-rom`](../../n64/README.md)) ↔ **`multi64d`** WebSocket. Payloads follow **[M64T](../spec/test-l3-application-v0.md)** inside L3 **`DATA` / `APPLICATION`**. Use ROM **M64T_PROTO** or **BENCH** (not **RAW_ECHO**).

**L2 today:** **`multi64d`** uses **SC64** ([`l3-over-sc64.md`](../spec/l3-over-sc64.md)) by default. **EverDrive X7:** `multi64d --cart ed64` selects the [`l3-over-everdrive-x7.md`](../spec/l3-over-everdrive-x7.md) §4 mapping through **`ed64-l2`**, which is **unvalidated on hardware**. **EverDrive-64 PRO:** `multi64d --cart ed64pro` selects [`l3-over-everdrive-pro.md`](../spec/l3-over-everdrive-pro.md) through **`ed64pro-l2`**, equally unvalidated; the ROM detects the PRO itself (`n64/test-rom/ed64pro.c`).

## Optional GUI

**`multi64-test-connector-gui`** — same commands over WebSocket; run **`multi64d`** (or Multi64) first. From **`crates/multi64-test-connector-gui/`**: `npm install`, `npm run dev`.

## Prerequisites

1. Cart + **`multi64d`** build you use (reference: **SC64** + USB) and **`multi64_test.z64`** on-console.  
2. PC: `cargo run -p multi64d --release -- --serial COM3`  
3. ROM mode **M64T_PROTO** or **BENCH** for the commands below. `set-mode` puts it there without touching the controller — it is the one command that also works in **RAW_ECHO**, so a host can drive the ROM from the mode it boots in.

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
| `set-mode` | `REQ_SET_MODE` (0x0F) + `--mode` | `SET_MODE_ACK` (0x8F); fails unless the cart reports the mode actually running |
| `diag` | `REQ_DIAG` (0x10) | `DIAG` (0x90); `--expect-clean` fails when a stream-health counter is non-zero |
| `mem-hello` | M64P `HELLO` (0x01) | `HELLO_ACK` (0x81) — protocol version, RDRAM size, writable flag |
| `mem-peek` | M64P `PEEKV` (0x02) + `--addr` / `--len` | `PEEKV_RESP` (0x82), or a named `ERR` |
| `mem-poke` | M64P `POKEV` (0x03) + `--addr` / `--hex` | `POKE_ACK` (0x83), or a named `ERR` |
| `mem-round-trip` | `REQ_DIAG`, then M64P `PEEKV`/`POKEV` | writes a pattern into the ROM's scratch region, reads it back and restores the original |
| `listen` | nothing | prints inbound L3 **APPLICATION** / **M64T** (**B** → **STRESS_LARGE**, **BENCH_TICK** in BENCH mode, **CONTROLLER** in reply to `req-controller`) |

Global options: `--url` (default `ws://127.0.0.1:38765/ws`), `--recv-timeout-secs` (default `5` for request/response commands).

`--addr` accepts `0x`-prefixed hex or decimal, and is an **RDRAM physical offset**, not a KSEG0 pointer: `0` is the start of RDRAM ([`memory-l3-application-v0.md`](../spec/memory-l3-application-v0.md) §4).

**`mem-*` talk to M64P**, a different profile on the same channel ([`memory-l3-application-v0.md`](../spec/memory-l3-application-v0.md)). The ROM dispatches on the payload magic, not on its mode, so they work in every mode except **RAW_ECHO** — **MEM_AGENT** is not required.

**Prefer `mem-round-trip` to `mem-poke`.** It takes its address from `DIAG`, which reports a region the ROM sets aside and never reads; every other address in RDRAM belongs to the ROM or to libdragon, so an arbitrary `mem-poke` can corrupt the running ROM. It is also the only M64P check that proves a write landed, rather than that a reply came back.

**A timeout does not mean the cart is silent.** `multi64d` accepts and discards writes while its link is released or faulted, with no error and no close on the WebSocket. When a command times out, check `GET /` — `serialActive: false` means the request never reached the cart.

`listen --duration-secs 0` runs until Ctrl+C; otherwise stops after N seconds.

## Unattended end-to-end run (hardware)

`scripts/l3_e2e.sh` runs the whole L3 bridge surface and reports **PASS**/**FAIL** per check. All it
needs is **`multi64d`** running against the cart and **`multi64_test.z64`** booted — **do not touch
the controller**. The ROM boots into **RAW_ECHO** and the script drives it out with `set-mode`,
which is what makes the run unattended.

| Host | Command |
|------|---------|
| Git Bash / Linux / macOS | `./scripts/l3_e2e.sh` |
| Windows PowerShell | `.\scripts\l3_e2e.ps1` |

The `.ps1` is a wrapper around the `.sh`, not a second copy: nothing in CI runs either, so a
drifting port would drift silently.

Options: `--port` (default `COM4`), `--url`, `--base`, `--skip-serial`. Environment:
`MULTI64_PORT`, `MULTI64_WS_URL`, `MULTI64_BASE_URL`, `MULTI64_EXPECT_ROM`, `MULTI64_SKIP_SERIAL`.

Exit codes: **0** all checks passed, **1** at least one failed, **2** the run could not start (no
daemon, or the connector does not build) — so a caller can tell a broken cart from a run that never
happened.

What it covers, in order:

1. **Preflight** — the connector builds once, the daemon answers `/health`, and it is holding its
   serial port.
2. **Liveness and identity** — `set-mode` out of RAW_ECHO, `ping`, and **the ROM version must match
   the tree**. That last one matters more than it looks: a stale ROM on the card makes every check
   below it a test of something else.
3. **M64T** — echo (including a 4 KiB body that crosses USB chunks), controller snapshot, and the
   session / EEPROM / SRAM sequence.
4. **M64P** — `mem-hello`, a full `mem-round-trip`, and a read outside RDRAM that **must** be
   refused.
5. **BENCH** — three seconds of unsolicited `BENCH_TICK`.
6. **Direct serial** — `set-mode` to RAW_ECHO, release the daemon's port, run `sc64-echo-test` and
   `sc64-l3-framing-e2e --large`, resume, and confirm the link came back. These need the cart in a
   state the WebSocket checks cannot use, which is why nothing chained them before `REQ_SET_MODE`
   existed.
7. **Stream health** — `diag --expect-clean`. Every check above proves its own round trip; only
   this proves the stream underneath them never desynchronised.

Two things it deliberately does **not** do. It does not stop at the first failure — every check
runs, because one broken check hiding the twenty after it is not a useful report. And it does not
count `rumble` or `display-text` as passes: their effects are on the console and the desk, and a
host that cannot observe them would only be asserting that the ROM sent an ack.

**If a check times out**, the line says whether the link was up. `multi64d` accepts and discards
writes while its link is released or faulted, so a silent cart and a dead link look identical from
the WebSocket; the script reads `GET /` at the moment of failure to tell them apart.

**If the script is killed** during the serial phase, an `EXIT` trap puts the daemon's port back. If
even that fails it says so loudly — `multi64d` would then be holding no port, and Multi64 needs a
restart.

### What it needs

On the PC: **`bash`** and **`curl`** — on Windows, Git for Windows (**not** WSL's bash, which cannot
reach the COM port) — and **`multi64d`** running against the cart, which the Multi64 installer
provides and starts.

On the desk: a **SummerCart64** in a powered console, and **`multi64_test.z64`** on its SD card,
booted, with the controller then left alone.

It does **not** need Node, Python, `jq`, or the N64 toolchain. It does not need a repo checkout or
a Rust toolchain either — see below.

### Running it without a checkout

The script drives three binaries. In a repo checkout with `cargo` on `PATH` it builds the connector
itself, as before. Outside one, put the binaries next to the script or point `--tools` at them:

| Binary | Covers |
|--------|--------|
| `multi64-test-connector` | every WebSocket check |
| `sc64-echo-test` | the direct-serial phase — **SKIP**ped, not failed, when absent |
| `sc64-l3-framing-e2e` | as above |

Build them with:

```sh
cargo build --release -p multi64-test-connector -p sc64-echo-test -p sc64-l3-framing-e2e
```

and copy them, plus `scripts/l3_e2e.sh` (and `l3_e2e.ps1` for Windows) and `multi64_test.z64`, into
one folder. With the Multi64 installer, that folder is everything a recipient needs — Multi64
carries the daemon, and the Xfer64 it installs can put the ROM on the card.

**Set `MULTI64_EXPECT_ROM`** when you hand it over, e.g. `MULTI64_EXPECT_ROM="multi64-test-rom
1.10"`. Outside a checkout there is no header to read the expected version from, so the version
guard **skips** rather than passes: a check that cannot fail is worse than no check, and this is the
one that stops the whole run being a test of some other ROM.

GitHub **CI** does not run this (no cart); it only builds and tests the Rust workspace. The latest
hardware run is recorded in [`n64/README.md`](../../n64/README.md#hardware-record).

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

Press **B** on the ROM for **STRESS_LARGE**, and in **BENCH** mode wait for **BENCH_TICK** lines. A **CONTROLLER** snapshot is host-driven — send `req-controller`; there is no button for it. Non-**APPLICATION** L3 (e.g. **C-up** / **Start** on the ROM) may appear as non-**M64T** payloads depending on host decoding.

Session, save, rumble, and display-text commands match **[`test-l3-application-v0.md`](../spec/test-l3-application-v0.md)** (**Spec-Revision** **1**). Default ROM build is **EEPROM**; rebuild the ROM with **`N64_ROM_SAVETYPE=sram256k`** (etc.) to get a non-zero SRAM window for **`sram-*`** commands.

Raw L3 loopback tests (**test ROM RAW_ECHO** mode — default at boot) use **serial L2** e2e tools on the USB port, not this WebSocket connector:

| Backend | Binaries |
|---------|----------|
| SummerCart64 (reference) | `sc64-l3-framing-e2e`, `sc64-echo-test` |
| EverDrive X7 (when `multi64-ed64-l2` works) | `ed64-l3-framing-e2e`, `ed64-echo-test` |
| EverDrive-64 PRO (when `multi64-ed64pro-l2` works) | `ed64pro-l3-framing-e2e`, `ed64pro-echo-test` |

The **`sc64-*`** crate names reflect the **reference** L2 implementation today.
