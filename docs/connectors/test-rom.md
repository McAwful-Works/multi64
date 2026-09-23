# Test ROM connector (`multi64-test-connector`)

> [Doc map](../README.md) · [Flash carts](../README.md#flash-carts-l2-backends) · [Repo README](../../README.md)

Rust CLI: `multi64_test.z64` ([`n64/test-rom`](../../n64/README.md)) ↔ `multi64d` WebSocket. Payloads follow **[M64T](../spec/test-l3-application-v0.md)** inside L3 **`DATA` / `APPLICATION`**. Use ROM **M64T_PROTO** or **BENCH** (not **RAW_ECHO**).

**L2 today:** `multi64d` uses **SC64** ([`l3-over-sc64.md`](../spec/l3-over-sc64.md)) by default. **EverDrive X7:** `multi64d --cart ed64` selects the [`l3-over-everdrive-x7.md`](../spec/l3-over-everdrive-x7.md) §4 mapping through `ed64-l2`, which is **unvalidated on hardware**. **EverDrive-64 PRO:** `multi64d --cart ed64pro` selects [`l3-over-everdrive-pro.md`](../spec/l3-over-everdrive-pro.md) through `ed64pro-l2`, equally unvalidated; the ROM detects the PRO itself (`n64/test-rom/ed64pro.c`).

## Optional GUI

`multi64-test-connector-gui` — same commands over WebSocket; run `multi64d` (or Multi64) first. From `crates/multi64-test-connector-gui/`: `npm install`, `npm run dev`.

## Prerequisites

1. Cart + `multi64d` build you use (reference: **SC64** + USB) and `multi64_test.z64` on-console.  
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

One run covers the whole L3 bridge and reports **PASS**/**FAIL** per check. All it needs is
`multi64d` running against the cart and `multi64_test.z64` booted — **do not touch the
controller**. The ROM boots into **RAW_ECHO**, which parses nothing; the suite drives it out with
`REQ_SET_MODE`, and that is what makes the run unattended.

There are two ways in, and they are the *same* checks: both call
`multi64_test_connector::suite::run_suite`. There is deliberately no second implementation to
drift, and no shell script — an earlier one existed and was replaced by this.

**The app** — `crates/multi64-test-app`, a window with a Run button and live results:

```sh
cargo build --release -p multi64-test-app     # target/release/multi64-test-app.exe
```

It is a single portable executable and needs nothing else installed but Multi64 itself. The ROM
version it expects is baked in at build time from `n64/test-rom/test_proto.h` (see its `build.rs`),
so a tester never has to know one. **Copy report** puts the whole run on the clipboard.

**The command line** — same checks, for a terminal or a script:

```sh
cargo run -p multi64-test-connector --release -- suite --expect-rom "multi64-test-rom 1.13"
```

Options: `--port` (default `COM4`), `--base`, `--url`, `--expect-rom`, `--skip-serial`.

Exit codes: **0** all checks passed, **1** at least one failed, **2** the run could not start (no
daemon) — so a caller can tell a broken cart from a run that never happened.

### Handing it to someone else

Four things, and nothing else:

1. the **Multi64 installer** — it carries `multi64d`;
2. the **Xfer64 installer** — Xfer64 can put the ROM on the card;
3. `multi64_test.z64`;
4. `multi64-test-app.exe`.

No Rust, no Node, no Python, no `bash`, no loose helper binaries. The direct-serial checks are
linked into the app rather than shelling out to `sc64-echo-test` and `sc64-l3-framing-e2e`, which is
what collapses the handover to one file.

### What it covers, in order

1. **Preflight** — the daemon answers `/health` and is holding its serial port.
2. **Liveness and identity** — out of RAW_ECHO, `ping`, and **the ROM version must match**. That one
   matters more than it looks: a stale ROM on the card makes every check below it a test of
   something else. With no expected version it **skips** rather than passes.
3. **M64T** — echo (including a 4 KiB body across USB chunks), controller snapshot, and the session
   / EEPROM / SRAM sequence.
4. **M64P** — `HELLO`, a full round trip through the ROM's scratch region, and a read outside RDRAM
   that **must** be refused.
5. **BENCH** — three seconds of unsolicited `BENCH_TICK`.
6. **Direct serial** — RAW_ECHO, release the daemon's port, echo and framing (including an
   8,308-byte frame) over the same cart pipe the daemon uses, resume, and confirm the link came
   back. These need a cart state the WebSocket checks cannot use, which is why nothing chained them
   before `REQ_SET_MODE` existed.

   The 8,308-byte frame is sent twice. First it goes **in one burst**, so the cart is echoing early
   messages while later ones are still arriving. Then it goes **one USB message at a time**, each
   echo read before the next message is sent. A cart that passes the second and fails the first
   cannot send while the host is sending: large frames work, full-duplex traffic does not. An
   EverDrive X7 does exactly that ([hardware record](../../n64/README.md#hardware-record)). The
   suspected cause is libdragon's EverDrive write, which gives up after 100 ms and leaves the
   message half sent.

   Last, the cart's own count of writes that gave up (`DIAG` `tx_failures`), read before RAW_ECHO
   and after, must not have moved. It is the only direct evidence of a failed write — the host
   otherwise sees just a malformed message — and it is **skipped** on a ROM older than 1.11.
7. **Stream health** — the `DIAG` counters. Every check above proves its own round trip; only this
   proves the stream underneath them never desynchronized.

Two things it deliberately does **not** do. It does not stop at the first failure — every check
runs, because one broken check hiding the twenty after it is not a useful report. And it does not
count `rumble` or `display-text` as passes: their effects are on the console and the desk, so a host
that cannot observe them would only be asserting that the ROM sent an ack. They run, and are
reported as **SKIP** with that reason.

**If a check times out**, the result says whether the link was up. `multi64d` accepts and discards
writes while its link is released or faulted, so a silent cart and a dead link look identical from
the WebSocket; the suite reads `GET /` at the moment of failure to tell them apart.

**If a run is interrupted** during the serial phase, a `Drop` guard puts the daemon's port back.
Without it `multi64d` would be left holding no port, and Multi64 dead until restarted.

GitHub **CI** does not run any of this (no cart); it only builds and tests the Rust workspace. The
latest hardware run is recorded in [`n64/README.md`](../../n64/README.md#hardware-record).

## Examples

```sh
cargo run -p multi64-test-connector -- ping                                    # one round trip
cargo run -p multi64-test-connector -- echo --text hello                       # echoed back
cargo run -p multi64-test-connector -- version                                 # the ROM's version string
cargo run -p multi64-test-connector -- req-controller                          # one controller snapshot
cargo run -p multi64-test-connector -- session-open                            # open a session; save writes need one
cargo run -p multi64-test-connector -- eeprom-info                             # what EEPROM the ROM has
cargo run -p multi64-test-connector -- eeprom-read --offset 0 --len 16         # first 16 bytes of EEPROM
cargo run -p multi64-test-connector -- sram-info                               # what SRAM the ROM has
cargo run -p multi64-test-connector -- rumble --port 0 --frames 60             # rumble port 0 for 60 frames
cargo run -p multi64-test-connector -- display-text --text "hello from host"   # text on the ROM's screen
cargo run -p multi64-test-connector -- listen                                  # print what the ROM sends
```

Press **B** on the ROM for **STRESS_LARGE**, and in **BENCH** mode wait for **BENCH_TICK** lines. A **CONTROLLER** snapshot is host-driven — send `req-controller`; there is no button for it. Non-**APPLICATION** L3 (e.g. **C-up** / **Start** on the ROM) may appear as non-**M64T** payloads depending on host decoding.

Session, save, rumble, and display-text commands match **[`test-l3-application-v0.md`](../spec/test-l3-application-v0.md)** (**Spec-Revision** **1**). Default ROM build is **EEPROM**; rebuild the ROM with `N64_ROM_SAVETYPE=sram256k` (etc.) to get a non-zero SRAM window for `sram-*` commands.

Raw L3 loopback tests (**test ROM RAW_ECHO** mode — default at boot) use **serial L2** e2e tools on the USB port, not this WebSocket connector:

| Backend | Binaries |
|---------|----------|
| SummerCart64 (reference) | `sc64-l3-framing-e2e`, `sc64-echo-test` |
| EverDrive X7 (when `multi64-ed64-l2` works) | `ed64-l3-framing-e2e`, `ed64-echo-test` |
| EverDrive-64 PRO (when `multi64-ed64pro-l2` works) | `ed64pro-l3-framing-e2e`, `ed64pro-echo-test` |

The `sc64-*` crate names reflect the **reference** L2 implementation today.
