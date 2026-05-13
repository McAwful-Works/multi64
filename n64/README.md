# N64 firmware (libdragon)

> **Documentation:** [Documentation map](../docs/README.md) · [Flash carts (L2)](../docs/README.md#flash-carts-l2-backends) · **Contributing:** [CONTRIBUTING.md](../CONTRIBUTING.md)

This folder builds **libdragon** ROMs for on-cart testing. The in-tree **L2** reference is **SummerCart64**; other carts need matching L2 + the same L3 stream ([`l2-link-adapter.md`](../docs/spec/l2-link-adapter.md)).

## `test-rom/` — all-in-one hardware test ROM

**Single** build output: **`multi64_test.z64`** — the official **Multi64** e2e ROM for **SummerCart64** + L3 (and the same binary for **EverDrive X7** once `multi64-ed64-l2` is functional). Uses libdragon (`N64_INST`).

**Modes** (press **L** to cycle):

| Mode | Behavior |
|------|----------|
| **RAW_ECHO** (default) | Verbatim `MULTI64_L3` loopback — use with **`sc64-l3-framing-e2e`**, **`sc64-echo-test`** (serial L2, not WebSocket). |
| **M64T_PROTO** | L3 `DATA` / `APPLICATION` with magic **`M64T`** — `PING`→`PONG`, `ECHO`, `REQ_VERSION`, `REQ_CONTROLLER`, **`SESSION_*`**, EEPROM/SRAM test messages ([`docs/spec/test-l3-application-v0.md`](../docs/spec/test-l3-application-v0.md)). |
| **BENCH** | Same RX as M64T + **periodic** cart→host `BENCH_TICK` + **A** sends controller snapshot. |

**Controls** (modes **M64T_PROTO** / **BENCH**): **A** = `CONTROLLER` snapshot (active port). **B** = `STRESS_LARGE` (`0xE1`); hold **C-down** to force chunked USB writes. **C-left / C-right** = active port `0`–`3`. **C-up** = small L3 `DATA` on Log (non-`M64T`). **Start** = L3 `HEARTBEAT` on Control. **D-up / D-down** = bench interval ±15 frames (15–600, **BENCH** only). **Z** = rumble ~1 s on active port. **R** = reset RX buffer, M64T stats, diagnostics, and **session** state. **L** = cycle mode (also resets like **R**). The HUD shows **`ses`** (session id; `0` = none). **Save type** is set in the Makefile (`eeprom4k` by default; use `N64_ROM_SAVETYPE=sram256k` for SRAM M64T tests).

### Build

```sh
cd n64/test-rom
make
```

### PC checks — **RAW_ECHO** mode (default)

**SummerCart64** reference — serial L2 on the USB port (not `multi64d` WebSocket):

```sh
cargo run -p sc64-l3-framing-e2e --release -- --port COM3
cargo run -p sc64-echo-test -- --port COM3
```

**EverDrive X7:** same ROM; use **`ed64-l3-framing-e2e`** / **`ed64-echo-test`** when [`multi64-ed64-l2`](../crates/ed64-l2/README.md) implements **`Ed64L2Pipe`** (currently **`open`** returns unsupported).

### `multi64d` + M64T / BENCH

With the ROM in **M64T_PROTO** or **BENCH** and `multi64d` running, use **`multi64-test-connector`** ([`docs/connectors/test-rom.md`](../docs/connectors/test-rom.md)):

```sh
cargo run -p multi64-test-connector -- ping
cargo run -p multi64-test-connector -- listen
```

---

## Python WebSocket smoke

With **`multi64_test.z64`** in **RAW_ECHO**, see [`scripts/multi64_ws_test.py`](../scripts/multi64_ws_test.py) and the [root README](../README.md).
