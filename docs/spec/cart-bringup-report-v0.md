# Cart bring-up report (v0)

**Spec-Revision:** 1  
**Status:** **Draft.** The ROM and the host tool that implement it have run on one SummerCart64
(2026-10-08), the baseline every other cart's report is read against, and on no other cart.

The cart bring-up ROM, [`n64/bringup`](../../n64/bringup/README.md)'s `multi64_bringup.z64`, runs
the cart agent's own drivers on whatever cart it boots from and keeps what it measures in one
structure in RDRAM: the **report**. A host reads the report through the agent build the ROM is
running, with [M64P](./memory-l3-application-v0.md) `PEEKV`, and steers the ROM by writing the
report's control words with `POKEV`. `multi64-test-connector bringup`
([`docs/connectors/test-rom.md`](../connectors/test-rom.md)) is the host implementation.

Nothing here is an L3 or M64P message. The report is a layout in RDRAM that M64P reads like any
other memory, so L3 **Protocol-Major/Minor** and M64P's `proto` are unaffected.

---

## 1. Finding the report

The report starts at a **16-byte aligned** RDRAM physical address in `0x00000400`–`0x000FFFFF`. Its
first 8 bytes are ASCII `CARTBRUP`, and its `self` field (offset `0x010`) holds that same address.

A host finds it by reading the range with `PEEKV` and taking the first 16-byte boundary where both
hold. The ROM assembles the magic at run time, so its image holds no copy, but copies do appear
once a host has read the report: the agent's transmit buffer keeps the last response. Those carry
the address of the report, not their own, and the `self` check passes them over.

## 2. Reading it

All fields are 32-bit words, **big-endian**, stored as the CPU stores them. There is no padding.

Read offsets `0x000`–`0x387` (904 bytes) in **one** `PEEKV` region. The main loop updates the
report between calls to the agent, and the agent serves a request within one call, so one region
is consistent as far as the main loop goes. Two requests are not. Two kinds of field are written
at other times:

- the load fields (§3.6), from a timer interrupt every 2 ms, so each is valid on its own but may
  be a callback ahead of the rest;
- the link counters a request moves (§3.7), from inside the agent call that is serving it, before
  its response is built.

A host must check `format` (offset `0x008`) is `0` before reading further. `size` gives the whole
report, echo area included.

## 3. Layout

### 3.1 Header and state

| Offset | Field | Meaning |
|--------|-------|---------|
| `0x000` | `magic[2]` | ASCII `CARTBRUP` |
| `0x008` | `format` | `0` for this revision of the layout |
| `0x00C` | `size` | Bytes in the whole report: `0x1388` |
| `0x010` | `self` | Physical address of offset `0x000` |
| `0x014` | `rom_version` | The bring-up ROM's version: major `<< 16` \| minor |
| `0x018` | `frame` | Main-loop iterations since boot. The screen update paces the loop to the display |
| `0x01C` | `count_hz` | Rate of the CPU Count register, which every `*_ticks` field counts in: 46,875,000 |
| `0x020` | `ctl_load` | Control (§5): load level wanted (§4.6) |
| `0x024` | `ctl_variant` | Control: agent build wanted (§4.2); `0` keeps the current one |
| `0x028` | `ctl_rerun` | Control: the blocks and conditions stages run again when this changes |
| `0x02C` | `ctl_reserved` | Written as `0` |
| `0x030` | `cart` | Cart found (§4.1) |
| `0x034` | `variant` | Agent build driving the link now (§4.2) |
| `0x038` | `load` | Load level in effect |
| `0x03C` | `rerun_done` | The `ctl_rerun` value last acted on |
| `0x040` | `stages` | Stages completed (§4.4) |
| `0x044` | `mem_size` | RDRAM bytes, as libdragon reports them |
| `0x048` | reserved | 2 words |

### 3.2 Identify

Raw reads made before anything is unlocked, then each agent driver's own initialization, in the
order of §6.1. A read that did not complete reads `0` and leaves its `probe_ok` bit clear.

| Offset | Field | Meaning |
|--------|-------|---------|
| `0x050` | `probe_ok` | Raw reads that completed (§4.3) |
| `0x054` | `d64_magic` | `0x180002EC`: `UDEV` on a 64drive |
| `0x058` | `sc64_ident_locked` | `0x1FFF000C` before the SC64 unlock |
| `0x05C` | `ed_reg14_locked` | `0x1F800014` before any key: an X-series `VERSION`, a PRO's `EDID` |
| `0x060` | `ed_reg04_locked` | `0x1F800004`: an X-series `USBCFG`, a PRO's `FIFOSTAT` |
| `0x064` | `pro_sysstat[2]` | `0x1F800008` read twice: a PRO inverts bit 3 on every read |
| `0x06C` | `init_tried` | `1 << variant` for each driver initialization tried (§4.2 ids) |
| `0x070` | `init_ok` | `1 << variant` for each that succeeded |
| `0x074` | `ed_reg14_unlocked` | `0x1F800014` after the X7 driver's initialization, if tried |
| `0x078` | `ed_usbcfg_after` | `0x1F800004` after the X7 driver's initialization, if tried |
| `0x07C` | `sc64_ident_unlocked` | `0x1FFF000C` after the SC64 driver's initialization, if tried |
| `0x080` | `rom_header[16]` | The first 64 bytes of cartridge ROM |

### 3.3 Blocks

Two copies of each table: `[0]` measured with no load, `[1]` under the level in `cond_load`.

| Offset | Field | Meaning |
|--------|-------|---------|
| `0x0C0` | `timing[2][4]` | Register read costs (§3.4), 16 bytes each, slot-major |
| `0x140` | `buf_addr` | PI address the buffer tests used; `0` when the cart has none |
| `0x144` | `cond_load` | Load level `[1]` was measured under |
| `0x148` | `buffer[2][4]` | Buffer tests (§3.5), 20 bytes each, slot-major, in §4.5 order |

### 3.4 `timing` entry

| Offset | Field | Meaning |
|--------|-------|---------|
| `+0x0` | `id` | Register (§4.7); `0` for an unused entry |
| `+0x4` | `spins` | The driver's spin limit for a wait that polls this register |
| `+0x8` | `ticks_total` | Ticks for 1024 reads, each timed singly (the timing reads included) |
| `+0xC` | `ticks_max` | The slowest single read |

`ticks_total / 1024 * spins` is the longest that driver's wait can last at the measured rate: what a
game frame loses when the wait gives up.

### 3.5 `buffer` entry

| Offset | Field | Meaning |
|--------|-------|---------|
| `+0x00` | `result` | §4.5 |
| `+0x04` | `first_bad` | Byte offset of the first byte read back wrong; `0xFFFFFFFF` if none |
| `+0x08` | `bad_bytes` | Bytes read back wrong, of 512 |
| `+0x0C` | `ticks_write` | |
| `+0x10` | `ticks_read` | |

### 3.6 Load

Reset whenever the load level changes, by `ctl_load` or the controller, and each time the
conditions stage runs (at boot, on a `ctl_rerun` change, and on **A**), even when it ends on the
level it started at.

| Offset | Field | Meaning |
|--------|-------|---------|
| `0x1E8` | `load_bytes` | Bytes per background DMA |
| `0x1EC` | `load_period_ticks` | Between two timer callbacks |
| `0x1F0` | `load_started` | Background DMAs started |
| `0x1F4` | `load_skipped` | Callbacks that found the PI busy and started none |
| `0x1F8` | `irq_gap_max` | Longest gap between two timer callbacks. Beyond `load_period_ticks`, it is how long interrupts were held off |
| `0x1FC` | reserved | 3 words |

The timer runs at every level, `off` included, so `irq_gap_max` measures how long the code under test
masks interrupts even with no DMA running.

### 3.7 Link

`link[4]` at `0x208`, 96 bytes each, indexed by variant id − 1 (§4.2). Counters only grow; the three
`*_max` fields reset whenever the variant or the load level changes. A host compares two readings to
see what one stretch of traffic did. The conditions stage's own brief change of level does not
reset them.

| Offset | Field | Meaning |
|--------|-------|---------|
| `+0x00` | `init_calls` | The agent's calls to its driver's initialization |
| `+0x04` | `init_ok` | ... that succeeded |
| `+0x08` | `recv_calls` | Calls to the driver's receive (`sc64_poll` on an SC64) |
| `+0x0C` | `recv_data` | ... that delivered bytes |
| `+0x10` | `recv_bytes` | Bytes delivered |
| `+0x14` | `recv_lost` | ... that reported part of the stream lost (EverDrives) |
| `+0x18` | `read_calls` | `sc64_read` calls (SC64 only) |
| `+0x1C` | `read_failed` | ... that failed |
| `+0x20` | `send_calls` | |
| `+0x24` | `send_ok` | |
| `+0x28` | `send_bytes` | Bytes in sends that succeeded |
| `+0x2C` | `pio_calls` | `pi_io` calls from the driver and from `PEEKROM` (not SC64's own copy loop) |
| `+0x30` | `pio_failed` | ... that reported the PI busy |
| `+0x34` | `recv_ticks_max` | Longest receive |
| `+0x38` | `send_ticks_max` | Longest send |
| `+0x3C` | `tick_ticks_max` | Longest whole `agent_tick` |
| `+0x40` | `tick_ticks_total` | All `agent_tick` time, wrapping |
| `+0x44` | `agent_ticks` | `agent_get_ticks()` |
| `+0x48` | `agent_frames` | `agent_get_frames_handled()`: ticks that carried an M64P request |
| `+0x4C` | `agent_ready` | `agent_is_ready()` |
| `+0x50` | `m64p_requests` | Requests the agent served |
| `+0x54` | `m64p_errors` | Requests it answered with `ERR` |
| `+0x58` | `m64p_last_error` | Last `ERR` code sent |
| `+0x5C` | reserved | |

The agent counters (`agent_*`, `m64p_*`) are updated only while that variant drives the link.

### 3.8 Echo area

`echo[4096]` at `0x388`, to the end of the report. The ROM never reads or writes it: it is the
host's, for writing a pattern with `POKEV` and reading it back.

## 4. Values

### 4.1 `cart`

| Value | Cart |
|-------|------|
| `0` | None found |
| `1` | SummerCart64 |
| `2` | EverDrive X-series (X7 or 3.0). Also set when the version register names an X-series but its USB unit reads as off (an X5, or no cable); `init_ok` then says the driver did not answer |
| `3` | EverDrive-64 PRO |
| `4` | 64drive: it answers, but no agent driver exists for it |

### 4.2 Variants

Each is a complete agent build, all linked into the one ROM.

| Id | Build | Drives |
|----|-------|--------|
| `1` | `sc64.c` | SummerCart64 |
| `2` | `ed64.c`, `pi_io.c` moving words by CPU load and store, as the agent ships | X-series (default) |
| `3` | `ed64.c`, `pi_io.c` built with `PI_IO_DMA`, moving words by PI DMA | X-series |
| `4` | `ed64pro.c` | PRO |

### 4.3 `probe_ok` bits

| Bit | Read |
|-----|------|
| 0 | ROM header |
| 1 | `d64_magic` |
| 2 | `sc64_ident_locked` |
| 3 | `ed_reg14_locked` |
| 4 | `ed_reg04_locked` |
| 5 | `pro_sysstat` |

### 4.4 `stages` bits

| Bit | Stage |
|-----|-------|
| 0 | Identify done |
| 1 | Blocks done (slot `[0]`) |
| 2 | Conditions done (slot `[1]`) |
| 3 | Link running: a variant was selected |

### 4.5 Buffer tests

Entry order: written by CPU words then read by CPU words (`IO>IO`), `IO>DMA`, `DMA>IO`,
`DMA>DMA`.

| `result` | Meaning |
|----------|---------|
| `0` | Not run |
| `1` | Read back what was written |
| `2` | Read back something else (`first_bad`, `bad_bytes`) |
| `3` | The write reported the PI busy; nothing to compare |
| `4` | The read reported the PI busy |
| `5` | Not applicable: this cart has no buffer the test can use |

### 4.6 Load levels

| Value | Name | Background DMA |
|-------|------|----------------|
| `0` | off | None (the timer still runs, for `irq_gap_max`) |
| `1` | moderate | 4 KiB every 2 ms |
| `2` | heavy | 16 KiB every 2 ms: more than the PI finishes in the period, so it is rarely idle |

### 4.7 `timing` ids

| Id | Register | Spin limit it multiplies |
|----|----------|--------------------------|
| `1` | `PI_STATUS`, read directly | `pi_io.c` `PI_WAIT_SPINS` |
| `2` | X-series `USBCFG` `0x1F800004`, through `pi_io_read` | `ed64.c` `ED_BUSY_SPINS` |
| `3` | SC64 `SR_CMD` `0x1FFF0000`, through `pi_io_read` | `sc64.c` `SC64_CMD_SPINS` |
| `4` | PRO `SYSSTAT` `0x1F800008`, through `pi_io_read` | `ed64pro.c` `MCU_SPINS` |
| `5` | PRO `FIFOSTAT` `0x1F800004`, through `pi_io_read` | `ed64pro.c` `STATUS_SPINS` |

The spin limits are read from the driver sources when the ROM is built, so they are the ones the
agent ships with.

## 5. Control

A host writes `ctl_load`, `ctl_variant` and `ctl_rerun` with `POKEV`. The ROM looks at them once a
frame and acts on a **change**, not on the value: a value written once is acted on once, and a
controller button pressed afterward is not undone by the host's older value.

So a host that writes the value a word already holds changes nothing, even when the ROM's state
differs from it because of a button since. To be sure of a setting, a host first writes a value the
ROM ignores, such as `0xFFFFFFFF`, then the one it wants, in two requests: the ROM reads its
controls between frames and the agent serves one request per frame. It then confirms `variant` and
`load` before relying on them.

- `ctl_variant` naming a build that cannot drive the cart found (§4.2), or `ctl_load` naming no
  level (§4.6), is ignored, but still counts as the value last seen.
- Switching variant hands the link to a different agent with its own state. Its first ticks run its
  own driver initialization, and whatever the previous agent had read but not answered is lost.
- A variant that never answers cannot be switched away from over the link. The controller can.
- `ctl_rerun` runs the blocks stage into `[0]` and the conditions stage into `[1]` again, then
  copies `ctl_rerun` to `rerun_done`.

## 6. What the stages do

### 6.1 Identify

The raw reads of §3.2 come first, in table order, with nothing written. Then each driver's own
initialization runs until one answers, in the order libdragon and the test ROM probe, so that no
cart is sent another cart's unlock once it has been identified:

1. A 64drive (`d64_magic` is `UDEV`): `cart` is `4` and no initialization runs.
2. The PRO driver's, which only reads until the cart has identified as a PRO.
3. The X7 driver's, which writes the X-series key. If it fails but the version register names an
   X-series, the search stops there, as libdragon's does.
4. The SC64 driver's.

### 6.2 Blocks

`PI_STATUS`, then the found cart's status registers (§4.7), each read 1024 times.

Then a 512-byte pattern is written to the cart's buffer and read back, four ways (§4.5): the X-series
USB window at `0x1F800400`, with `USBCFG` set to its idle write mode before writing and its idle
read mode before reading; or the SummerCart64 buffer at `0x1FFE0000`, with ROM write enabled around
the test, as `sc64_write` does around staging, and put back afterward. The PRO has no buffer. CPU
words go through `pi_io.c` as built for the agent, and PI DMA through `pi_io.c` built with
`PI_IO_DMA`. Every word differs in every test and every run, so stale data cannot pass.

The X-series window is not documented as memory that reads back what was written. A mismatch there
in every combination is a fact about the window; a mismatch in some combinations and not others is
a fact about the transfer method.

### 6.3 Conditions

Blocks again, under moderate load, into `[1]`. The background DMAs are started from a timer
interrupt, where a game's PI manager starts its own, and they read the ROM's own image.

### 6.4 Link

The selected variant's `agent_tick()` is called once per main-loop iteration, from the main thread,
as a game's per-frame hook would call it. The loop waits on the screen update, which paces it to
the display.

## 7. Revision

**Spec-Revision** counts edits to this document only.

| Value | Meaning |
|-------|---------|
| **1** | Initial draft, `format` `0`: written with the ROM and host tool, before either ran on a cart. |
