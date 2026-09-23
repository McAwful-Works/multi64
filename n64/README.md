# N64 firmware (libdragon)

> **Documentation:** [Documentation map](../docs/README.md) · [Flash carts (L2)](../docs/README.md#flash-carts-l2-backends) · **Contributing:** [CONTRIBUTING.md](../CONTRIBUTING.md)

This folder builds **libdragon** ROMs for on-cart testing, and holds the **M64P agent** that goes into other ROMs. The in-tree **L2** reference is **SummerCart64**; other carts need matching L2 + the same L3 stream ([`l2-link-adapter.md`](../docs/spec/l2-link-adapter.md)).

## `agent/` — M64P agent for game ROMs

The console side of an RDRAM peek/poke integration: libdragon- and libultra-free code a game calls once per frame. It shares `test-rom/mem_proto.c` with the test ROM's **MEM_AGENT** mode. See [`agent/README.md`](agent/README.md) and the [ROM integration guides](../docs/integration/README.md).

## `test-rom/` — all-in-one hardware test ROM

**Single** build output: **`multi64_test.z64`** — the official **Multi64** e2e ROM for **SummerCart64** + L3, and the same binary for **EverDrive X7** and **EverDrive-64 PRO** (both experimental). Uses libdragon (`N64_INST`).

The committed binary is built from the current source and accepts SummerCart64, EverDrive X7 and EverDrive-64 PRO.

> **Built with the versions pinned in [`toolchain.lock`](toolchain.lock)** — libdragon `c4a7e11`, mips64-elf GCC **16.2.0**. Use `./setup-toolchain.sh` to install exactly those; see *Toolchain* below.

> Note the ROM is **compressed** (`N64_ROM_ELFCOMPRESS` defaults to 1 in `n64.mk`), so searching `multi64_test.z64` for strings will give misleading results — LZ back-references replace repeated substrings. Inspect `build/multi64_test.elf` from your own `make` instead; `build/` is not tracked.

**Modes** (press **L** to cycle):

| Mode | Behavior |
|------|----------|
| **RAW_ECHO** (default) | Verbatim `MULTI64_L3` loopback — use with **`sc64-l3-framing-e2e`**, **`sc64-echo-test`** (serial L2, not WebSocket). |
| **M64T_PROTO** | L3 `DATA` / `APPLICATION` with magic **`M64T`** — `PING`→`PONG`, `ECHO`, `REQ_VERSION`, `REQ_CONTROLLER`, **`SESSION_*`**, EEPROM/SRAM test messages ([`docs/spec/test-l3-application-v0.md`](../docs/spec/test-l3-application-v0.md)). |
| **BENCH** | Same RX as M64T + **periodic** cart→host `BENCH_TICK` + **A** sends controller snapshot. |
| **MEM_AGENT** | **M64P** RDRAM peek/poke ([`docs/spec/memory-l3-application-v0.md`](../docs/spec/memory-l3-application-v0.md)) — `HELLO`, `PEEKV`, `POKEV`. Host-driven; no buttons beyond **L** / **R**. |

Controls (modes `M64T_PROTO` / `BENCH`): **A** = `CONTROLLER` snapshot (active port). **B** = `STRESS_LARGE` (`0xE1`); hold **C-down** to force chunked USB writes. **C-left / C-right** = active port `0`–`3`. **C-up** = small L3 `DATA` on Log (non-`M64T`). **Start** = L3 `HEARTBEAT` on Control. **D-up / D-down** = bench interval ±15 frames (15–600, `BENCH` only). **Z** = rumble ~1 s on active port. **R** = reset RX buffer, M64T stats, diagnostics, and session state. **L** = cycle mode (also resets like **R**). The HUD shows `ses` (session id; `0` = none). Save type is set in the Makefile (`eeprom4k` by default; use `N64_ROM_SAVETYPE=sram256k` for SRAM M64T tests).

### Toolchain

Pinned in [`toolchain.lock`](toolchain.lock) so a rebuild reproduces the committed `multi64_test.z64`:

```sh
cd n64
./setup-toolchain.sh              # installs into ~/n64inst; no sudo needed
export N64_INST="$HOME/n64inst"
export PATH="$N64_INST/bin:$PATH"
```

Two things the lock works around, both of which would otherwise defeat the pin:

- **libdragon's toolchain release tag is rolling.** `toolchain-continuous-prerelease` dates from 2023, but its assets are replaced in place. Pinning the tag gives a different compiler over time, so the lock pins the immutable **asset id** and verifies a **SHA-256**; a mismatch aborts the install.
- **libdragon records no version of its own** once installed, so drift cannot be detected from the tree. `setup-toolchain.sh` writes a stamp, and `make check-toolchain` compares it against the lock:

```sh
cd n64/test-rom && make check-toolchain
```

It reports `OK` (both verified), `PARTIAL` (gcc matches but libdragon came from elsewhere, so it cannot be proven), or a non-zero `MISMATCH`. Nothing runs this automatically — CI does not build the ROM, so drift is only ever caught by a human.

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

**EverDrive X7:** same ROM — the committed binary boots on an EverDrive and shows an on-screen **UNVALIDATED** warning; use **`ed64-l3-framing-e2e`** / **`ed64-echo-test`**. [`multi64-ed64-l2`](../crates/ed64-l2/README.md) implements **`Ed64L2Pipe`**, but the mapping is **not yet validated**: it has run on one X7 (see the [hardware record](#hardware-record)), and these tools, or the test app, are how the rest of §4.5 gets answered. Start with **`ed64-smoke`** to confirm the port, then see [`l3-over-everdrive-x7.md`](../docs/spec/l3-over-everdrive-x7.md) §4.5.

**EverDrive-64 PRO:** same ROM. libdragon's `usb.h` does not know the PRO, so `test-rom/cart_link.c` detects one first and routes USB traffic through `test-rom/ed64pro.c`; the ROM then shows an on-screen **UNVALIDATED** warning. Host side: `ed64pro-echo-test` and `ed64pro-l3-framing-e2e` over the link alone, then `multi64d --cart ed64pro`. Never run on a cart — see [`l3-over-everdrive-pro.md`](../docs/spec/l3-over-everdrive-pro.md) §8 and §9.

### `multi64d` + M64T / BENCH

With the ROM in **M64T_PROTO** or **BENCH** and `multi64d` running, use **`multi64-test-connector`** ([`docs/connectors/test-rom.md`](../docs/connectors/test-rom.md)):

```sh
cargo run -p multi64-test-connector -- ping
cargo run -p multi64-test-connector -- listen
```

### Hardware record

Runs of the committed `multi64_test.z64` on real carts, newest first. Add one when the ROM changes or a cart is tried for the first time, with the cart's firmware and the host OS.

| Date | Cart | ROM | Host | Result |
|------|------|-----|------|--------|
| 2026-09-18 | EverDrive-64 X7 (OS version not recorded) | `multi64-test-rom 1.10`, SHA-256 `75cce6950c3667be8ef89392f90b9a359783543fd1ef5e86e40a67df9181fe4a`; then `1.11`, SHA-256 `c6e04dd596c89ad2ff1363c81abe4480b1b62ab928d00c8870a917cf2ae005bb`; then `1.12`, SHA-256 `de9f50ba96a2c40607d142983def2d45fe43bdab0fa5aab92c62e245b550952a` | Windows (version not recorded); `multi64d --cart ed64`; test app from `611c89d`, `06d55d2`, `e71631e`, `243ea4b`, then `9e120de` | **Pass** on 1.12 (33 passed, 2 skipped); 1.10 and 1.11 failed the burst framing check |
| 2026-09-17 | SummerCart64 (`SCv2`, firmware 2.20 rev 2) | `multi64-test-rom 1.10`; SHA-256 `75cce6950c3667be8ef89392f90b9a359783543fd1ef5e86e40a67df9181fe4a` | Windows 11 Pro 10.0.26200; host tools and `multi64d` from `a593195` | **Pass** (31/31) |
| 2026-09-13 | SummerCart64 (`SCv2`, firmware 2.20 rev 2) | built from `52098ce`; SHA-256 `68b0544013c1622abe03dedf5a13cb95a8288d1d59421e0b2dbd565c6a884ba3` | Windows 11 Pro 10.0.26200; host tools and `multi64d` from `c7b13bb` | **Pass** |

**2026-09-18, EverDrive-64 X7.** The first time the X7 mapping ran on a cart, run by someone else
with the three-file handover (Multi64 installer, ROM, test app).

- **Through `multi64d`:** every check passed — liveness, ROM version, all of M64T including the
  4 KiB echo across USB chunks, all of M64P, BENCH — with `DIAG` reporting the cart as
  "EverDrive X-series" and zero overflow, resync and bad-header drops. `Ed64L2Pipe` carried L3
  both ways.
- **Direct serial:** the 12-byte echo and the small framing cases passed. The 8,308-byte frame
  sent **in one burst** failed with `expected CMPH trailer, got [f0, f1, f2, f3]`; the same frame
  sent **one USB message at a time** passed. The cart cannot reliably send while the host is still
  sending; see [`l3-over-everdrive-x7.md`](../docs/spec/l3-over-everdrive-x7.md) §4.5 item 6.
- **ROM 1.11** added the cart's count of writes that gave up. It was 0 before the direct-serial
  checks and 6 after, with everything else as above: the cart's own writes are what fail, and
  only during the burst. 31 passed, 2 failed (the burst check and that count), 2 skipped.
- **ROM 1.12** reads everything the host has already sent before each RAW_ECHO write, and
  changes nothing else. **Every check passed**: 33 passed, 0 failed, 2 skipped (HUD text and
  rumble, which the host cannot see). The burst came back whole and the count of failed writes
  stayed at 0. So unread host data is what blocks an X7's write, not a slow USB unit.
- **Not recorded:** the EverDrive OS version and the host's Windows version.

The first run failed both direct-serial checks with timeouts, and that was the test app: it opened
the port with the SummerCart64 pipe whatever cart the daemon used, so its messages had no `DMA@`
framing and the X7 dropped them.

**2026-09-17, SummerCart64.** First unattended run: the
ROM was booted and the controller was not touched again. **31 checks, 0 failures.**

- **Host-set mode:** `REQ_SET_MODE` took the ROM out of **RAW_ECHO**, the mode it boots into and the
  one that parses nothing. This is what makes the run unattended, and it works on hardware.
- **M64T:** ping, version, echo (including 4 KiB across USB chunks), controller snapshot, and the
  session / EEPROM / SRAM sequence. `sram-info` still reports size 0, as built.
- **M64P:** `HELLO`, a full `PEEKV`/`POKEV` round trip through the ROM's scratch region, and an
  out-of-range read correctly refused with `E_RANGE`. **The first time M64P has run on hardware** —
  nothing in the tree spoke it before.
- **BENCH:** 3 unsolicited `BENCH_TICK` in 3 s.
- **RAW_ECHO over direct serial:** `sc64-echo-test` and `sc64-l3-framing-e2e --large` (8,308-byte
  frame), run in the same pass as the checks above by releasing and resuming the daemon's port —
  two cart states that previously only a person with a controller could switch between.
- **Stream health:** `DIAG` reported zero overflow, resync and bad-header drops across the run.
- **Not run:** CTRL_POLL, and an SRAM build.

The first attempt was **28/31**, and all three failures were real. Two were one bug in the new
`DIAG` field, which reported the scratch region as a KSEG0 pointer when M64P addresses are RDRAM
physical offsets: the cart refused the round trip, and the negative check passed `0x00000000`,
which is the *start* of RDRAM and therefore valid. The third was ordering in the script — it pinged
while the cart was still in RAW_ECHO, which echoes a ping rather than answering it.

**2026-09-13, SummerCart64.** The first run since the ROM's USB traffic moved behind `test-rom/cart_link.c`, which probes for an EverDrive-64 PRO before libdragon's `usb_initialize`. The ROM went onto the SD card with `sc64-sd-e2e --upload` and was read back byte for byte with `--verify`.

- **Boot:** normal, with no UNVALIDATED line, so the PRO probe does not misfire on an SC64.
- **RAW_ECHO:** `sc64-echo-test` (12 bytes) and `sc64-l3-framing-e2e --large` (a small `DATA` frame, a `HEARTBEAT`, and an 8,308-byte frame across USB chunks) both pass.
- **M64T_PROTO:** `scripts/test_rom_connector_e2e.sh` (since replaced by the suite in `multi64-test-connector`) passed all 13 steps. `rumble` answered status `0x01`, not supported on the port, with no Rumble Pak inserted; `sram-info` reports size 0, as built.
- **Cart-originated large sends:** **B** delivered repeated 8,192-byte `STRESS_LARGE` payloads to `multi64-test-connector listen`, with no errors.
- **Not run:** BENCH, CTRL_POLL, MEM_AGENT, and an SRAM build.

---

## Python WebSocket smoke

With **`multi64_test.z64`** in **RAW_ECHO**, see [`scripts/multi64_ws_test.py`](../scripts/multi64_ws_test.py) and the [root README](../README.md).
