# L3 over EverDrive 64 X7 (draft mapping)

**Spec-Revision:** 1  
**Status:** **Draft** — §4 is now **derived from a working reference implementation** but has **not been validated against hardware in this repository**. **`multi64-ed64-l2`** (`Ed64L2Pipe`) implements §4 and has run on **one X7** (2026-09-18): L3 through `multi64d` worked, and the cart could not send while the host was sending (§4.5 item 6). One cart does not validate a mapping. In-tree EverDrive tooling: **`ed64-smoke`** (§8), **`ed64-echo-test`**, **`ed64-l3-framing-e2e`** — all runnable; the two e2e tools exercise §4 framing against a cart and are how §4.5 gets answered.

This document will define how **L3** octets ([l3-bridge-protocol-v1.md](./l3-bridge-protocol-v1.md)) are carried over the **EverDrive-64 X7** USB path. It does **not** redefine L3.

§4 specifies the host↔USB byte model. It is **not yet normative**: it is transcribed from **UNFLoader**, which has shipped this protocol for EverDrive 64 for years, but nobody has run it against an X7 *here*. Treat it as the implementation target for **`crates/ed64-l2`**, and see §4.0 for exactly what "unvalidated" means. §8 documents a **non-normative** USB64 **`cmd`/`t`** smoke handshake (Krikzz **`usb64`** style) and notes legacy **uppercase `CMD` + `T`** probes.

---

## 1. Scope

| Item | Detail |
|------|--------|
| **Hardware** | **EverDrive-64 X7** (USB models). See §1.1 for the full model matrix. |
| **Goal** | Same **ordered L3 byte stream** on the PC as [l2-link-adapter.md](./l2-link-adapter.md) — differences stay inside L2/L1. |
| **Reference backend** | [l3-over-sc64.md](./l3-over-sc64.md) shows the pattern: datatype tag + length + payload chunks. |

### 1.1 Which EverDrives this can apply to

Krikzz's N64 carts fall into **two unrelated USB families**. This document covers only the first: the X-series lineage, which speaks `usb64` (§8) plus the `DMA@` framing (§4).

| Model | USB | Supportable by this mapping | Notes |
|-------|-----|-----------------------------|-------|
| **EverDrive-64 X7** | yes | **target** | The model this mapping is written for. FTDI FT245R (`0403:6001`). Supported by Krikzz `usb64` and by UNFLoader (OS **3.04+**). |
| **EverDrive 64 3.0** | yes | probably | Discontinued. UNFLoader supports OS **3.04+** but documents OS **3.07+** as *incompatible*. Untested here; firmware version decides. |
| **EverDrive-64 X5** | **no** | **never** | No USB port (cart ID `0xED640014`). Nothing host-side is possible. |
| EverDrive 64 **2.5 and earlier** | **no** | **never** | No USB. libdragon's `usb.c` rejects 2.5 (`0xED640007`) explicitly. |
| **EverDrive-64 PRO** | yes | **no — different protocol** | Released August 2026. Speaks **edlink** (Gen3; protocol ID `0x07`, device ID `0x27`), not `usb64` + `DMA@`. Its N64-side registers at `0x1F800000` are a command FIFO and a mailbox, with none of the X7's USB registers, and neither libdragon's `usb.c` nor UNFLoader recognises it. Supporting it is a separate mapping: [`l3-over-everdrive-pro.md`](./l3-over-everdrive-pro.md). |
| Other EverDrive **PRO** / **CORE** carts | n/a | not applicable | Krikzz's other-console lines (Mega EverDrive PRO, EverDrive N8 PRO, …). |

**Do not use edlink for the X7 or 3.0.** edlink is the Gen3 protocol (921600 baud, `EPO`/`FCI` commands) of the PRO and CORE series. A previous attempt in this repository added an `EdlinkLink` backend with a `PROTOCOL_ID_ED64` handshake for X-series carts; because they do not speak Gen3, the handshake could never match, the code silently fell through to the `usb64` path it was written to fall back to, and the work was reverted. The X-series host protocol is **`usb64`** (§8) plus the data framing in **§4** — not edlink.

That conclusion is specific to the X-series. The **EverDrive-64 PRO does speak edlink**: [krikzz/edlink](https://github.com/krikzz/edlink) has an `ED64` device module, and [krikzz/ed64-pro-pub](https://github.com/krikzz/ed64-pro-pub) has N64-side sources for its FIFO, USB and SD file commands, both MIT-licensed. A PRO backend reuses neither §4 nor `ed64-l2`: it is `multi64-ed64pro-l2`, specified in [`l3-over-everdrive-pro.md`](./l3-over-everdrive-pro.md), and it is no more proven than this mapping. Everything stated about the PRO here comes from those sources, not from hardware.

---

## 2. References (external)

Implementors should start from vendor and community sources:

| Resource | Notes |
|----------|--------|
| [Krikzz — EverDrive-64 X-series dev files](https://krikzz.com/pub/support/everdrive-64/x-series/dev/) | `usb64` sources, `usbio-sample.zip`, etc. |
| [krikzz/ed64-x-pub](https://github.com/krikzz/ed64-x-pub) (GitHub) | Reference code: **`usb64/usb64/`** (Windows `usb64` tool, C# `CommandProcessor`), **`ED64-XIO`** sample, **`docs/`** (TOC, hardware ID; some USB wire notes may be **WIP**) |
| [N64brew — EverDrive-64 X7](https://n64brew.dev/wiki/EverDrive-64_X7) | **N64-side** registers (`REG_USB_CFG`, `REG_USB_DATA` 512-byte buffer), `bi_usb_rd` / `bi_usb_wr` behavior |
| [jsdf/webserial-ed64log](https://github.com/jsdf/webserial-ed64log) | Example of host serial/WebUSB usage |
| [krikzz/edlink](https://github.com/krikzz/edlink) | Gen3 USB utility for PRO/CORE carts, including `edlink/DEV_ED64` for the **EverDrive-64 PRO** (MIT). **Not** applicable to the X-series (§1.1). |
| [krikzz/ed64-pro-pub](https://github.com/krikzz/ed64-pro-pub) | **EverDrive-64 PRO** dev sources: the `edio` sample ROM (FIFO, USB, SD file access) and host scripts (MIT). |

**Normative for Multi64:** once this spec defines the **PC-side** framing, the **`ed64-l2`** crate MUST match it; the N64 ROM MUST use compatible `bi_usb_*` (or equivalent) so L3 bytes round-trip.

---

## 3. L2 constraints (from [l2-link-adapter.md](./l2-link-adapter.md) §5)

EverDrive USB paths often use **fixed 512-byte** data areas on the cart. The host adapter MAY buffer, pad, or split so that the **L3 codec** still sees a single continuous stream (padding MUST NOT appear inside L3 payloads).

Public documentation (N64brew) notes practical limits on the **N64** side, including:

- Data moved through **`REG_USB_DATA`** in up to **512-byte** chunks.
- Reads may require **at least 16 bytes** in some flows; hosts should pad outbound data accordingly when the final mapping requires it.

The host packet layout that satisfies these constraints is given in **§4**.

---

## 4. Host-side mapping

### 4.0 Provenance and validation status

Everything in §4 is transcribed from **[UNFLoader](https://github.com/buu342/N64-UNFLoader)** — `UNFLoader/device_everdrive.cpp`, the host half — cross-checked against the N64-side library this repository's test ROM already links: `<usb.h>`, libdragon's port of UNFLoader's `usb.c`. UNFLoader has shipped this protocol for EverDrive 64 for years, so it is a **working reference**, not a guess.

It has been executed against **one X7** in this repository (2026-09-18, [`n64/README.md`](../../n64/README.md#hardware-record)), which answered some of §4.5 and raised a new question there. Until §4.5 is closed:

- this document stays **Draft** and §4 is **not normative**;
- `ed64-l2` built to it MUST be described as unproven, not as EverDrive support;
- the open questions in **§4.5** MUST be resolved by observation, not assumption.

The distinction matters here specifically: the previous EverDrive attempt in this repo failed by implementing a protocol from a plausible-looking vendor source that did not apply to the hardware (§1.1). Deriving from a reference that demonstrably drives an X7 avoids that class of error — it does not substitute for running it.

### 4.1 Device discovery

The X7 presents an **FTDI** USB interface. UNFLoader drives it through the FTDI **D2XX** API; this repository instead uses `serialport`, i.e. the **VCP** (virtual COM port) driver, which is what `multi64-ed64-link` and `ed64-smoke` already do.

| Platform | Expectation |
|----------|-------------|
| **Windows** | A **COM** port once Krikzz's `usb64` FTDI driver is installed. Replacing it with WinUSB/libusb (Zadig) **breaks** COM access. |
| **Linux** | `/dev/ttyUSB*` via the in-kernel `ftdi_sio` driver. |
| **macOS** | `/dev/tty.usbserial-*`. Untested. |

Selection is by port name, as elsewhere in this repository; there is no identity handshake in the data path. `ed64-smoke` (§8) is the way to confirm a port is an EverDrive before opening an L2 pipe on it.

### 4.2 Wire format

**Data to and from a running ROM does not use the 16-byte `cmd` packet.** That packet (§8, and `Ed64Link::command_packet`) is a *cartridge firmware* command — ROM upload, test connection — handled by the EverDrive OS. Once a ROM is running, the host writes into the cart's USB FIFO and the N64 drains it via `REG_USB_DATA`; the framing below is the application-level agreement between the host and the ROM's USB library.

Both directions send the same four fields:

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | ASCII **`DMA@`** (`0x44 0x4D 0x41 0x40`) |
| 4 | 4 | **Big-endian `u32`**: `(datatype << 24) | (size & 0x00FF_FFFF)` |
| 8 | *size* | Payload |
| — | 4 | ASCII **`CMPH`** (`0x43 0x4D 0x50 0x48`) |

The framing is **not symmetric**: the two directions put the 2-byte alignment padding (§4.3) in
different places, and only that.

| Direction | Layout | Reference |
|-----------|--------|-----------|
| **Host → cart** | header, payload **padded** to 2, then `CMPH` | UNFLoader `device_senddata_everdrive` sends `ALIGN(size, 2)` payload bytes from a zero-filled copy, then the 4-byte trailer; libdragon's `usb_everdrive_poll` reads `ALIGN(usb_datasize, 2)` bytes and *then* the trailer |
| **Cart → host** | header, **unpadded** payload, `CMPH`, then padding to a 2-byte **whole message** | libdragon's `usb_everdrive_write` restarts its copy loop to append `CMPH` straight after the data and sends `ALIGN(block+offset, 2)` bytes; UNFLoader's `device_receivedata_everdrive` checks the trailer at `size` and then consumes `alignment - (totalread % alignment)` bytes |

Both layouts are the same total length, and both carry the unpadded length in the header, so they
differ only for an **odd-length** payload — which is why the earlier "symmetric" reading of this
section survived unit tests and was only caught by reading the two references (issue #134).

A receiver MUST NOT assume the padding byte is zero. libdragon sends whatever its transmit buffer
last held, so on the cart → host direction it is arbitrary; it is consumed with its own message and
never inspected.

`datatype` is the same enumeration the N64 side passes to `usb_write`. This repository's test ROM already emits `usb_write(MULTI64_L3, …)`, so **`MULTI64_L3` arrives in the top byte of the header** exactly as the SC64 path tags its `PKT` `U` payloads — the two backends agree at this level, which is what lets one L3 codec sit above both.

`size` is the **unpadded** payload length. A receiver MUST use it, and MUST discard the padding, so that padding never reaches the L3 codec — the requirement in §3 and in [l2-link-adapter.md](./l2-link-adapter.md) §5.

### 4.3 Fragmentation and padding

- The alignment depends on the library's protocol version: **2 bytes** for `PROTOCOL_VERSION2`, **512 bytes** for version 1. What is aligned differs by direction — see the second table in §4.2.
- Payload bytes move in **512-byte** chunks, matching the cart's `REG_USB_DATA` window (§3).
- The **header carries the true length**, so reassembly is driven by `size`, not by transfer boundaries. An L3 frame larger than one transfer is simply a longer payload; the L2 adapter concatenates decoded payloads into one continuous octet stream.

### 4.4 Errors

§4 is not normative while this document is Draft, so the last column records what `ed64-l2`
(`crates/ed64-l2/src/lib.rs`) **does**, rather than what an implementation must do. It recovers
further than the reference does: only a bad trailer is an error at all.

| Condition | Reference behavior | `ed64-l2` as implemented |
|-----------|--------------------|------------------------|
| No data pending | Queue status reports 0 | `read_l3_bytes` returns `Ok(0)` — same contract as `Sc64L2Pipe` |
| Bytes before a header | `DEVICEERR_64D_BADDMA` | **Not an error.** The parser skips forward to the next `DMA@` and carries on, logging how many bytes it discarded at `debug` on target `multi64_ed64_l2`. With no `DMA@` anywhere in the buffer it keeps the last 3 bytes, in case a magic is split across two reads |
| Trailer mismatch | `DEVICEERR_64D_BADCMP` | `io::ErrorKind::InvalidData`. Parse state is **not** reset: only the 4 `DMA@` magic bytes are dropped, so a resync cannot latch onto the same bad message again, and every byte after them stays buffered, to be parsed on the next read that brings more in. L3 octets decoded before the error are handed out first, so a caller that drops the pipe on error does not lose them; the error then surfaces once, when the queue is empty |
| Datatype other than `MULTI64_L3` | — | Not an error: the message is consumed whole and its payload **discarded**, with a `debug` log. Only `0x01` payloads reach the L3 codec |
| Stalled transfer | 500 ms read/write timeouts | A read timeout is `Ok(0)`, not an error — the timeout is whatever `set_timeout` last applied (`multi64d` uses 50 ms for reads, 1 s for a write). Any other read error is returned unchanged, which faults `multi64d`'s link |
| Resynchronisation | Purge RX **and** TX | `clear_serial_buffers` clears the port in both directions, the wire buffer, the decoded L3 queue, and any error held back |

A `size` field that is wrong in a way the trailer check cannot yet see costs time rather than data:
the parser must buffer `align(8 + size + 4)` bytes before it can check the trailer at all, and `size`
is 24 bits, so a `DMA@` invented by noise can hold the decoder until as much as 16 MiB has arrived.
Nothing bounds that below the header's own limit.

The trailer check reads the 4 bytes at `8 + size`, per the cart → host row of §4.2. It previously
read them at `8 + align(size, 2)`, the host → cart layout, which put every odd-length message from
a cart on the trailer-mismatch row above and faulted `multi64d`'s link (issue #134). That is fixed
from the references; it has still never been exercised against a cart.

### 4.5 Open questions — resolve on hardware before dropping Draft

1. ~~**Protocol version.**~~ **Resolved from source.** libdragon's `usb.c` declares `USBPROTOCOL_VERSION 2` and aligns payloads to **2 bytes**; `ed64-l2` matches. Still worth confirming on hardware that the cart's firmware agrees, but this is no longer an open guess.
2. ~~**Padding direction.**~~ **Resolved from source (#134).** Read from all four reference functions — libdragon's `usb_everdrive_write` and `usb_everdrive_poll`, UNFLoader's `device_senddata_everdrive` and `device_receivedata_everdrive` — which agree with each other and disagree with this document's earlier "symmetric" claim. The layouts are in §4.2; `ed64-l2` and `n64/agent/ed64.c` both match them. Confirm on a cart along with the rest of §4, but this is no longer a guess.
3. **VCP vs D2XX.** UNFLoader uses D2XX and purges the FTDI queues directly. Whether a `serialport` VCP handle gives equivalent behaviour under load — particularly for the purge in §4.4 — is unverified.
4. **Baud.** `usb64` framing uses 115200 for the `cmd` path; whether the FIFO data path is baud-sensitive at all over VCP is unconfirmed.
5. **EverDrive 3.0.** Whether the framing is identical on 3.0, and where the OS 3.07 incompatibility bites (§1.1).
6. **Sending while receiving. Observed on one X7, cause not yet confirmed.** With the test ROM echoing each host message as it arrives, a burst of 17 messages came back with one cut off after its first 512-byte block; the host found the next message's payload where `CMPH` belonged. The same 17 messages sent one at a time, each echo read before the next, came back intact. libdragon's `usb_everdrive_write` gives up when the USB unit stays busy for 100 ms and returns part-way through the message, reporting it only through `usb_timedout()`; `n64/agent/ed64.c`'s `ed64_send` has the same shape. Test ROM 1.11 counts such writes in `DIAG` (`tx_failures`, `test-l3-application-v0.md` §11) to confirm the cause. The host already survives it: §4.4's resynchronisation drops the broken message and the stream continues, so the cost is a lost message, not a lost link.

The same run bears on items 1–3 without closing them. Every check through `multi64d` passed with zero overflow, resync or bad-header drops, which is consistent with the 2-byte alignment and the padding layouts in §4.2, though no check targeted an odd-length payload. A `serialport` VCP handle carried the whole run; the §4.4 purge under load was not specifically tested.

---

## 5. N64-side expectations

Homebrew should follow Krikzz-style initialization (`REG_KEY`, `REG_SYS_CFG`, USB cfg) and use **`bi_usb_rd` / `bi_usb_wr`** (or equivalent) so that the byte stream seen by the game matches what the **host adapter** emits.

**In practice this is already handled.** `n64/test-rom` links libdragon's `<usb.h>` — the same UNFLoader-derived library that abstracts SC64 and EverDrive behind one API (`usb_initialize`, `usb_write`, `usb_read`, `usb_poll`, `usb_getcart`). It already emits `usb_write(MULTI64_L3, …)`, and the library applies the per-cart framing, which on EverDrive is §4.2.

The ROM therefore **already boots on an X7**. `n64/test-rom/main.c` halts only on an unknown cart, and on an EverDrive it warns rather than implying support:

```c
const char cart = usb_getcart();
if (cart != CART_SC64 && cart != CART_EVERDRIVE) {
    printf("Need SummerCart64 or EverDrive 64\n");
    while (1) { }
}
if (cart == CART_EVERDRIVE) {
    printf("EverDrive: UNVALIDATED host mapping\n");
    printf("  expect failures; see l3-over-everdrive-x7.md\n");
}
```

This was relaxed **ahead of** hardware validation, deliberately: a host mapping cannot be validated without a ROM that boots on the cart, so keeping the `CART_SC64` gate would have left §4 untested indefinitely. The risk in doing it early — a ROM that implies support it has never demonstrated — is met by the on-screen warning instead of a silent boot. Keep that warning until §4.5 is answered. The committed `multi64_test.z64` includes this change.

No other N64-side change is expected. If validation turns one up, record it here.

---

## 6. Implementation in this repository

| Component | Role |
|-----------|------|
| [`crates/multi64-ed64-link`](../../crates/multi64-ed64-link) | Rust **`multi64-ed64-link`**: X7 **`usb64`** **`cmd`** framing, `RomRead` — **not** the L3 stream adapter (**`ed64-l2`**). |
| [`crates/ed64-l2`](../../crates/ed64-l2/README.md) | `Ed64L2Pipe` — implements §4 framing, mirroring `multi64-sc64-l2::Sc64L2Pipe`. Unit-tested for framing, and run on one X7 (§4.5 item 6); **not validated** until §4.5 is closed. |
| [`crates/ed64-smoke`](../../crates/ed64-smoke) | **`ed64-smoke`** binary: host **`cmd`/`t`** smoke test (§8, `usb64`-style), not L3. |
| [`crates/ed64-echo-test`](../../crates/ed64-echo-test) | **`ed64-echo-test`**: same role as `sc64-echo-test` over **`Ed64L2Pipe`**; runs, exercising §4 framing that is still unvalidated. |
| [`crates/ed64-l3-framing-e2e`](../../crates/ed64-l3-framing-e2e) | **`ed64-l3-framing-e2e`**: same role as `sc64-l3-framing-e2e` over **`Ed64L2Pipe`**; runs, exercising §4 framing that is still unvalidated. |
| [`n64/test-rom`](../../n64/README.md) | Already uses libdragon `<usb.h>`, which supports both carts. Boots on `CART_SC64` and `CART_EVERDRIVE`, with an on-screen **UNVALIDATED** warning on the latter (§5). |
| `multi64d` | `--cart ed64` selects `Ed64L2Pipe` ([daemon API §5.1](./daemon-api-v1.md)). Experimental: has carried L3 both ways to one X7, with zero stream errors across the test app's checks (2026-09-18). |
| [`n64/agent`](../../n64/agent/README.md) | `make CART=ed64` builds the in-game agent around `ed64.c`: the console side of §4 without libdragon, under the agent's PI rules. Never run on a cart. |
| [`crates/multi64`](../../crates/multi64/README.md) | Settings → **Cart** → *EverDrive-64 X7 (beta)* starts the daemon with `--cart ed64`. Its default, *Auto-detect*, finds an X7 only by sending the `usb64` test (§8) to ports, since its FT245R has no cart-specific USB descriptor ([`multi64-cart-probe`](../../crates/cart-probe/README.md)). |

---

## 7. Contributor testing checklist (hardware)

For developers **with** an X7:

1. Flash **`multi64_test.z64`** and leave it in **RAW_ECHO**. The committed binary accepts an EverDrive and shows `EverDrive: UNVALIDATED host mapping` at boot (§5). libdragon's `<usb.h>` is expected to handle cart bring-up; if it does not, that is a §4.5 finding.
2. Confirm **serial device** appears on the host when the ROM uses USB.
3. Capture **host↔device** traces (optional) to help finalize §4.
4. Open a PR updating this spec + **`ed64-l2`** with measured behavior.
5. Run **`ed64-smoke`** (`crates/ed64-smoke`) if the cart exposes EverDrive-style USB serial — see §8.

---

## 8. Host USB64 `cmd`/`t` smoke handshake (non-normative)

This is **not** the future normative L3 byte pipe from §4. It documents the host **test connection** probe used by Krikzz’s reference **`usb64`** (`CommandProcessor` in **`ed64-x-pub`**) and implemented as **`ed64-smoke`**. Some **older community** host code sent only a **short uppercase** form (`CMD` + `T`); **firmware** may or may not accept that.

| Field | Value |
|-------|--------|
| PC → device (reference) | **16 bytes:** ASCII **`cmd`** (lowercase) + single-byte command **`t`** (`0x74`, test connection), then three **big-endian `uint32`** fields: **address**, **length** (in 512-byte blocks in the vendor tool), **argument** — all **zero** for this probe. Matches **`CommandPacketTransmit(TransmitCommand.TestConnection)`** in **`usb64/usb64/CommandProcessor.cs`**. |
| PC → device (legacy) | **4 bytes:** ASCII **`CMD`** + **`T`** — seen in some community USB loaders; **not** interchangeable with the reference layout on all builds. |
| Success pattern | Response buffer has **`k`** (legacy reply) or **`r`** (reply) at **byte index 3** (0-based), per vendor receive parsing. Some firmware builds also send **`3`** at index 4 and a region hint at index 5 (`p` / `n` / `m`). |

**X7 OS builds and USB drivers differ.** If `ed64-smoke` fails, try another baud rate, **`--flush`**, or confirm the EverDrive menu / OS has USB serial active. Capture traces to help finalize §4.

---

## 9. Revision

**Spec-Revision** counts edits to **this** document only. It is **independent** of L3 **Protocol-Major/Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).

| Value | Meaning |
|-------|---------|
| **1** | EverDrive X7 draft mapping: **`usb64`** smoke (**§8**), **`ed64-x-pub`** references, **`multi64_test.z64`** checklist; pre-release tree. |

**Normative compatibility (preserved):** The goal remains **one L3 octet stream** at the codec boundary (see [l2-link-adapter.md](./l2-link-adapter.md)); ED64 host details in §4–§5 must not break that once normative.

**Spec-Revision policy:** All normative docs use **Spec-Revision** **1** for now; edits **accumulate** under **1** until maintainers announce a bump. This is independent of L3 **Protocol-Major** / **Protocol-Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).
