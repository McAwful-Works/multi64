# L3 over EverDrive-64 PRO (draft mapping)

**Spec-Revision:** 1  
**Status:** **Draft.** **Never run against an EverDrive-64 PRO.** Nothing here is normative until §8 is answered on hardware. **`multi64-ed64pro-l2`** implements the host side and the test ROM's `ed64pro.c` the console side; both are equally unverified.

This document defines how **L3** octets ([l3-bridge-protocol-v1.md](./l3-bridge-protocol-v1.md)) travel between a PC and a ROM running on an **EverDrive-64 PRO**, as an L2 mapping under [l2-link-adapter.md](./l2-link-adapter.md). It builds on the host link in [ed64-pro-usb-host.md](./ed64-pro-usb-host.md), which it does not repeat. It does **not** redefine L3.

The PRO is not an X-series cart. Nothing in [l3-over-everdrive-x7.md](./l3-over-everdrive-x7.md) applies to it.

---

## 1. Provenance

The X7 mapping transcribes a working reference. **This one cannot**: no public host or ROM carries a byte stream over the PRO.

| Project | Revision checked | PRO support |
|---------|------------------|-------------|
| [libdragon](https://github.com/DragonMinded/libdragon) | `trunk` `c4a7e119`, `preview` `14eeee33` | None. `usb.h` knows `CART_64DRIVE`, `CART_EVERDRIVE` (X-series) and `CART_SC64` |
| [UNFLoader](https://github.com/buu342/N64-UNFLoader) | `master` `3821703b` | None, on either side |

So this mapping is **this repository's design**, built from the same Krikzz sources as the host link ([ed64-pro-usb-host.md §1](./ed64-pro-usb-host.md#1-sources)): edlink `eb2f5114` for the PC side, and ed64-pro-pub `5d7e9690` (`edio/everdrive.c`, `edio/appmain.c`) for the console side. Both are MIT-licensed.

---

## 2. Host → ROM

The host writes L3 octets into the cart FIFO, which the running ROM drains:

- **Mechanism:** edlink's `FifoWR` — a memory write (`CMD_EPO` from **LINK** `0x10` to **FCI** `0x13`) with destination address **`0x10010000`**, followed by the bytes ([host link §6, §8](./ed64-pro-usb-host.md#6-bulk-transfers-epo)). No status is read afterwards; edlink reads none.
- **Size:** each FIFO write MUST carry at most **2048** bytes, the FIFO's capacity. Hosts SHOULD use **1024**.
- **Spacing:** hosts SHOULD leave at least **34 ms** between FIFO writes (§5).
- **Quiet link:** while the stream is in use, the host MUST NOT send any command that returns data (status, file system, memory reads). Its reply would arrive among the ROM's bytes (§3), and nothing marks where it ends. The connection handshake runs once, before the stream starts, and the host discards any input that remains after it.

---

## 3. ROM → host

The ROM sends with a transfer command written into its own FIFO port, which the cart microcontroller forwards to USB:

1. Frame `2B D4 81 7E 10` (`CMD_EPO`, `EPO_SCMD_XFER`).
2. A 16-byte header, big-endian: source address `0`, destination address `0`, length, source endpoint **`0x10`** (LINK), destination endpoint **`0x18`** (USB), reserved `0`.
3. One start byte, `0`.
4. `length` data bytes, at most **1024** per command (`SIZE_ACK_BLOCK`).
5. Wait until `SYSSTAT` bit 0 (MCU busy) clears, then send the next block.

Endpoint `0x18` is the console library's number for USB; edlink's PC code numbers it differently ([host link §10](./ed64-pro-usb-host.md#10-where-the-sources-disagree)). The console numbering applies here, because the cart's MCU is interpreting a command from the console.

**Nothing comes back through the FIFO** for this command; the ROM only watches `SYSSTAT`. So host bytes that arrive while the ROM sends stay queued for it.

The host reads the bytes **raw**, as edlink's `usbrd` does: whatever is waiting on the serial port, with no header ([host link §9](./ed64-pro-usb-host.md#9-data-from-a-running-rom)).

---

## 4. No L2 framing

Neither direction adds anything. L3 frames carry their own boundaries (`MAGIC` + `PAYLOAD_LEN`) and its decoder resynchronises on `MAGIC`, and an L2 adapter must not interpret L3 ([l2-link-adapter.md §2](./l2-link-adapter.md#2-host-side-l2-interface-conceptual)). A datatype tag like SC64's or the X7's `DMA@` header would add nothing here: the PRO's link carries only this stream, provided §2's quiet-link rule holds.

---

## 5. Flow control

The FIFO holds **2048 bytes**, and the host is never told how much the ROM has drained. Krikzz's sample says only not to send more until the previous data has been read.

This mapping therefore paces by time. The host splits writes into **1024-byte** FIFO writes spaced **34 ms** apart: two frames at 60 Hz, for a ROM that drains the FIFO every frame. That caps host → ROM throughput at about 30 KiB/s, and an 8 KiB M64P request takes about a quarter of a second. The spacing applies across separate writes too, not only within one.

ed64-pro-pub's header also names `ADDR_FCI_FAVB` (`0x10020000`, "mcu fifo rd available"), which a host could read to see the fill level. Reading it is a command with a reply, which §2 forbids while the ROM may be sending, so this mapping does not use it.

**What to measure first**, before trusting or tuning the numbers above:

1. What the cart does with a FIFO write that would overflow: block the USB link, drop bytes, or corrupt the queue.
2. The largest chunk and smallest spacing that survive a sustained RAW_ECHO stream.
3. Whether `ADDR_FCI_FAVB` can be read safely while the ROM is known to be idle, which would allow real flow control.

---

## 6. ROM side (N64)

### 6.1 Registers

The cart's EDIO registers sit on the PI bus at **`0x1F800000`**, one 32-bit word each (ed64-pro-pub `everdrive.h`):

| Offset | Register | Access | Meaning |
|--------|----------|--------|---------|
| `+0x00` | `FIFODATA` | R/W | One byte per word. Reads drain bytes from the host (and command replies); writes go to the cart MCU |
| `+0x04` | `FIFOSTAT` | R | Low 16 bits: bytes waiting to be read |
| `+0x08` | `SYSSTAT` | R | Bit 0: MCU busy. Bit 3: inverts on every read. Bits 7..4: always `0xA` |
| `+0x10` | `MBX` | R/W | Mailbox shared with the PC; unused here |
| `+0x14` | `EDID` | R | Device ID, `0xED64xxxx` |

ed64-pro-pub's sample performs no initialisation before using the FIFO or sending to USB; neither does this mapping.

### 6.2 Detecting a PRO

libdragon's `usb_initialize` (`trunk` `c4a7e119`) looks for a 64drive, then writes the X-series register key `0xAA55` to `0x1F808004` and reads **`0x1F800014`**, accepting `0xED640013` (X7, X5) or `0xED640008` (3.0). On a PRO that address is `EDID`, whose low half is undocumented, and `0x1F808004` is not a register ed64-pro-pub describes. A PRO might therefore be rejected, or be driven as an X7.

A ROM that supports the PRO **MUST detect it before calling `usb_initialize`**, and SHOULD write nothing until the cart has identified itself:

1. Read `EDID`. Require `0xED64xxxx`, and reject the X-series and older IDs `0xED640007`, `0xED640008`, `0xED640013` and `0xED640014`.
2. Read `SYSSTAT` twice. Require bits 7..4 to be `0xA` in both, and bit 3 to differ. The strobe's timing is unverified, so a ROM MAY retry a few pairs before giving up; the test ROM tries up to four, and fails at once if the constant nibble is ever wrong.
3. Drain anything already in the FIFO; it would otherwise be read as the reply.
4. Write `CMD_STATUS` (`2B D4 10 EF`) to `FIFODATA` and read 4 bytes, giving up after a bounded wait. Require `5A 07 27`, as in [host link §5](./ed64-pro-usb-host.md#5-status-and-errors).

Only on success is the cart a PRO. Otherwise continue to `usb_initialize`.

### 6.3 Receiving

Read `FIFOSTAT`'s low 16 bits, then read that many words from `FIFODATA`, keeping the low byte of each. This never blocks. A ROM SHOULD drain the FIFO every frame (§5).

### 6.4 Sending

Write §3's sequence to `FIFODATA` and poll `SYSSTAT` bit 0 with a bounded wait. A cart that stays busy should cost a failed send, not a hung console.

---

## 7. Implementation in this repository

| Component | Role |
|-----------|------|
| [`crates/ed64pro-l2`](../../crates/ed64pro-l2/README.md) | **`multi64-ed64pro-l2`**: `Ed64ProL2Pipe`, the host side of §2–§5, over `multi64-ed64pro-link`. Tested against the in-memory `FakeEd64Pro` only. |
| `multi64d` | `--cart ed64pro` selects it ([daemon API §5.1](./daemon-api-v1.md#51-flags)). The PRO runs at its fixed 921600 baud; `--baud` does not apply. |
| [`n64/test-rom`](../../n64/README.md) | `ed64pro.c` implements §6; `cart_link.c` detects a PRO before libdragon's `usb_initialize` and routes the test ROM's USB traffic to it. The ROM shows an on-screen **UNVALIDATED** warning on a PRO. |
| [`n64/agent`](../../n64/agent/README.md) | `make CART=ed64pro` builds the in-game agent around its own `ed64pro.c`: §6 without libdragon, under the agent's PI rules, reassembling L3 frames across ticks. |
| [`crates/multi64`](../../crates/multi64/README.md) | Settings → **Cart** → *EverDrive-64 PRO (experimental)* starts the daemon with `--cart ed64pro`. Its default, *Auto-detect*, finds a PRO only by sending the edlink handshake to ports ([`multi64-cart-probe`](../../crates/cart-probe/README.md)). |
| [`crates/ed64pro-echo-test`](../../crates/ed64pro-echo-test) | **`ed64pro-echo-test`**: raw L3 bytes against the test ROM's **RAW_ECHO**, straight over `Ed64ProL2Pipe`, after printing what the handshake reported. Never run on a cart. |
| [`crates/ed64pro-l3-framing-e2e`](../../crates/ed64pro-l3-framing-e2e) | **`ed64pro-l3-framing-e2e`**: whole L3 frames against **RAW_ECHO**; `--large` spans several FIFO writes and so probes §5. Never run on a cart. |

---

## 8. Open questions — resolve on hardware before dropping Draft

1. **Detection (§6.2):** what `EDID` a PRO reports; whether `SYSSTAT` shows the constant nibble and strobe; whether a running ROM gets a status reply through the FIFO at all.
2. **What `FIFODATA` delivers:** only host bytes and replies to the ROM's own commands, or anything else the MCU generates.
3. **Flow control (§5):** overflow behaviour, usable chunk size and spacing.
4. **Sending (§3):** whether the MCU accepts endpoint `0x18` from a running game, how long `SYSSTAT` stays busy per 1024-byte block, and whether anything else must happen first.
5. **The host handshake:** whether its 66 zero bytes and status commands, sent while a ROM is running, reach only the MCU and leave the ROM's FIFO alone.
6. **Host polling:** the latency of polling for waiting bytes at 921600 baud on Windows and Linux VCP drivers.

---

## 9. Hardware test order

For someone **with** a PRO:

1. **The host link alone.** Xfer64's *EverDrive-64 PRO (experimental)* mode, or edlink itself, confirms the port, driver and handshake before any ROM is involved.
2. **Boot the test ROM.** "EverDrive PRO: UNVALIDATED host mapping" means §6.2 passed. "usb init failed" or the X7 warning means it did not; record `EDID` and `SYSSTAT`.
3. **RAW_ECHO over the link alone.** `ed64pro-echo-test`, then `ed64pro-l3-framing-e2e`. Then the same through `multi64d --cart ed64pro` from any L3 client.
4. **Large frames.** `ed64pro-l3-framing-e2e --large`, then frames up to the L3 payload cap, to probe §5.
5. **M64T and MEM_AGENT modes.**

Record the firmware version and OS for each result.

---

## 10. Revision

**Spec-Revision** counts edits to **this** document only. It is independent of L3 **Protocol-Major**/**Protocol-Minor** ([l3-bridge-protocol-v1.md](./l3-bridge-protocol-v1.md) §12), which this mapping does not change.

| Value | Meaning |
|-------|---------|
| **1** | Initial draft: designed from Krikzz's sources, never run on hardware. |
