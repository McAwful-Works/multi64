# L3 over EverDrive 64 X7 (draft mapping)

**Spec-Revision:** 1  
**Status:** **Draft** — §4 is now **derived from a working reference implementation** but has **not been validated against hardware in this repository**. **`multi64-ed64-l2`** (`Ed64L2Pipe`) remains a **stub**. In-tree EverDrive tooling: **`ed64-smoke`** (§8), **`ed64-echo-test`**, **`ed64-l3-framing-e2e`** (blocked on L2).

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

Krikzz's **Nintendo 64** line is **X5 and X7 only**. There is no N64 cartridge in the PRO or CORE series.

| Model | USB | Supportable | Notes |
|-------|-----|-------------|-------|
| **EverDrive-64 X7** | yes | **target** | The model this mapping is written for. Supported by Krikzz `usb64` and by UNFLoader (OS **3.04+**). |
| **EverDrive 64 3.0** | yes | probably | UNFLoader supports OS **3.04+** but documents OS **3.07+** as *incompatible*. Untested here; firmware version decides. |
| **EverDrive-64 X5** | **no** | **never** | No USB port. Nothing host-side is possible. |
| EverDrive **PRO** / **CORE** series | n/a | **not applicable** | These are Krikzz's *other-console* lines (Mega EverDrive PRO, EverDrive N8 PRO, …). **No N64 cartridge is in them.** |

**Do not implement Krikzz [edlink](https://github.com/krikzz/edlink) for N64.** edlink is the Gen3 `++`-framed protocol (921600 baud, EPO/FCI reads) for the **PRO and CORE** series, per its own README. A previous attempt in this repository added an `EdlinkLink` backend with a `PROTOCOL_ID_ED64` handshake; because no N64 cartridge speaks Gen3, the handshake could never match, the code silently fell through to the `usb64` path it was written to fall back to, and the work was reverted. The N64 host protocol is **`usb64`** (§8) plus the data framing in **§4** — not edlink.

---

## 2. References (external)

Implementors should start from vendor and community sources:

| Resource | Notes |
|----------|--------|
| [Krikzz — EverDrive-64 X-series dev files](https://krikzz.com/pub/support/everdrive-64/x-series/dev/) | `usb64` sources, `usbio-sample.zip`, etc. |
| [krikzz/ed64-x-pub](https://github.com/krikzz/ed64-x-pub) (GitHub) | Reference code: **`usb64/usb64/`** (Windows `usb64` tool, C# `CommandProcessor`), **`ED64-XIO`** sample, **`docs/`** (TOC, hardware ID; some USB wire notes may be **WIP**) |
| [N64brew — EverDrive-64 X7](https://n64brew.dev/wiki/EverDrive-64_X7) | **N64-side** registers (`REG_USB_CFG`, `REG_USB_DATA` 512-byte buffer), `bi_usb_rd` / `bi_usb_wr` behavior |
| [jsdf/webserial-ed64log](https://github.com/jsdf/webserial-ed64log) | Example of host serial/WebUSB usage |

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

It has nonetheless **never been executed against an X7 in this repository.** Until it has:

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

The framing is **symmetric**. Both directions send:

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | ASCII **`DMA@`** (`0x44 0x4D 0x41 0x40`) |
| 4 | 4 | **Big-endian `u32`**: `(datatype << 24) | (size & 0x00FF_FFFF)` |
| 8 | *size*, padded | Payload (see §4.3) |
| — | 4 | ASCII **`CMPH`** (`0x43 0x4D 0x50 0x48`) |

`datatype` is the same enumeration the N64 side passes to `usb_write`. This repository's test ROM already emits `usb_write(MULTI64_L3, …)`, so **`MULTI64_L3` arrives in the top byte of the header** exactly as the SC64 path tags its `PKT` `U` payloads — the two backends agree at this level, which is what lets one L3 codec sit above both.

`size` is the **unpadded** payload length. A receiver MUST use it, and MUST discard the padding, so that padding never reaches the L3 codec — the requirement in §3 and in [l2-link-adapter.md](./l2-link-adapter.md) §5.

### 4.3 Fragmentation and padding

- The sender pads the payload to an alignment that depends on the library's protocol version: **2 bytes** for `PROTOCOL_VERSION2`, **512 bytes** for version 1.
- Payload bytes move in **512-byte** chunks, matching the cart's `REG_USB_DATA` window (§3).
- The **header carries the true length**, so reassembly is driven by `size`, not by transfer boundaries. An L3 frame larger than one transfer is simply a longer payload; the L2 adapter concatenates decoded payloads into one continuous octet stream.

### 4.4 Errors

| Condition | Reference behavior | Mapping for `ed64-l2` |
|-----------|--------------------|------------------------|
| No data pending | Queue status reports 0 | `read_l3_bytes` returns `Ok(0)` — same contract as `Sc64L2Pipe` |
| Header mismatch | `DEVICEERR_64D_BADDMA` | `io::ErrorKind::InvalidData`, and reset parse state |
| Trailer mismatch | `DEVICEERR_64D_BADCMP` | `io::ErrorKind::InvalidData`, and reset parse state |
| Stalled transfer | 500 ms read/write timeouts | `io::ErrorKind::TimedOut` |
| Resynchronisation | Purge RX **and** TX | `clear_serial_buffers` MUST clear the port *and* the internal wire buffer |

### 4.5 Open questions — resolve on hardware before dropping Draft

1. **Protocol version.** Which alignment does libdragon's `usb.c` use on EverDrive, 2-byte or 512-byte? UNFLoader negotiates this; a host that assumes the wrong one mis-parses every message. **This is the single most likely thing to be wrong below.**
2. **VCP vs D2XX.** UNFLoader uses D2XX and purges the FTDI queues directly. Whether a `serialport` VCP handle gives equivalent behaviour under load — particularly for the purge in §4.4 — is unverified.
3. **Baud.** `usb64` framing uses 115200 for the `cmd` path; whether the FIFO data path is baud-sensitive at all over VCP is unconfirmed.
4. **EverDrive 3.0.** Whether the framing is identical on 3.0, and where the OS 3.07 incompatibility bites (§1.1).

---

## 5. N64-side expectations

Homebrew should follow Krikzz-style initialization (`REG_KEY`, `REG_SYS_CFG`, USB cfg) and use **`bi_usb_rd` / `bi_usb_wr`** (or equivalent) so that the byte stream seen by the game matches what the **host adapter** emits.

**In practice this is already handled.** `n64/test-rom` links libdragon's `<usb.h>` — the same UNFLoader-derived library that abstracts SC64 and EverDrive behind one API (`usb_initialize`, `usb_write`, `usb_read`, `usb_poll`, `usb_getcart`). It already emits `usb_write(MULTI64_L3, …)`, and the library applies the per-cart framing, which on EverDrive is §4.2.

The one thing stopping the existing ROM from running on an X7 is an explicit gate in `n64/test-rom/main.c`:

```c
if (usb_getcart() != CART_SC64) {
    printf("Need SummerCart64
");
    while (1) { }
}
```

Relaxing that to accept `CART_EVERDRIVE` is expected to be the whole N64-side change. It should be made **together with** hardware validation, not before — a ROM that advertises EverDrive support it has never demonstrated is worse than one that refuses to boot.

---

## 6. Implementation in this repository

| Component | Role |
|-----------|------|
| [`crates/multi64-ed64-link`](../../crates/multi64-ed64-link) | Rust **`multi64-ed64-link`**: X7 **`usb64`** **`cmd`** framing, `RomRead` — **not** the L3 stream adapter (**`ed64-l2`**). |
| [`crates/ed64-l2`](../../crates/ed64-l2/README.md) | Stub crate; will provide an L2 handle similar to `multi64-sc64-l2::Sc64L2Pipe` when implemented. |
| [`crates/ed64-smoke`](../../crates/ed64-smoke) | **`ed64-smoke`** binary: host **`cmd`/`t`** smoke test (§8, `usb64`-style), not L3. |
| [`crates/ed64-echo-test`](../../crates/ed64-echo-test) | **`ed64-echo-test`**: same role as `sc64-echo-test` over **`Ed64L2Pipe`** (blocked until L2). |
| [`crates/ed64-l3-framing-e2e`](../../crates/ed64-l3-framing-e2e) | **`ed64-l3-framing-e2e`**: same role as `sc64-l3-framing-e2e` over **`Ed64L2Pipe`** (blocked until L2). |
| [`n64/test-rom`](../../n64/README.md) | Already uses libdragon `<usb.h>`, which supports both carts; gated to `CART_SC64` today (§5). |
| `multi64d` | Future: optional backend selection (`--link ed64` or similar) once `ed64-l2` is functional. |

---

## 7. Contributor testing checklist (hardware)

For developers **with** an X7:

1. Build and run **`n64/test-rom`** → **`multi64_test.z64`** with ED64-specific USB bring-up (may require a small ED64 init layer — to be shared in-repo when available).
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
