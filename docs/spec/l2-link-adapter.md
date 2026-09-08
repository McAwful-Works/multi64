# L2 — Link adapter contract

**Spec-Revision:** 1  

This document defines what a **link adapter** provides on the **host** (PC) so that **L3** ([l3-bridge-protocol-v1.md](./l3-bridge-protocol-v1.md)) can run over different hardware (SummerCart64, EverDrive 64 X7, future transports). It is **not** the USB vendor protocol of any cart; those live in vendor docs and thin driver code.

---

## 1. Responsibilities

| Layer | Responsibility |
|-------|----------------|
| **L3** | Framing, handshake, channels, opaque payloads, error codes. |
| **L2** | Expose a **single ordered bidirectional byte stream** between host process and the running N64 software path that terminates L3. Hide USB packetization, chunk sizes, and OS-specific serial APIs. |
| **L1** | Vendor/USB/serial details (SC64 serial protocol, ED64 register streaming, etc.). |

The **daemon** composes: outward client API ↔ L3 codec ↔ L2 adapter ↔ L1 driver.

---

## 2. Host-side L2 interface (conceptual)

An L2 implementation MUST provide the following **semantic** operations (names are illustrative):

| Operation | Behavior |
|-----------|----------|
| `open(device_hint)` | Acquire the connection to a specific cart or default device. MAY block until the device is ready. |
| `close()` | Release resources; N64 side may see logical disconnect (implementation-defined). |
| `read(dst, max_len, timeout)` | Copy **up to** `max_len` bytes received in order from the device. Returns count `0..max_len`. Timeout for blocking semantics is implementation-defined. |
| `write(src, len)` | Send `len` bytes in order. MAY block until accepted by OS/driver. |
| `flush()` | Optional: ensure writes visible to peer where applicable. |

**Ordering:** Bytes written by the host MUST arrive at the L3 peer in the same order (as observed by the L3 parser on the device). Same for device → host.

**Framing:** L2 does **not** interpret L3; it only moves bytes. **L3** consumes the stream and detects frame boundaries via `MAGIC` + `PAYLOAD_LEN`.

---

## 3. Cross-platform requirements

Implementations SHOULD support **Windows**, **Linux**, and **macOS** for the same L3 behavior. Practical notes:

- **Device discovery:** Document how users select a serial/COM port or device path (`COM*`, `/dev/ttyACM*`, etc.).
- **Permissions (Linux):** Document udev/group rules if non-root access is required (common for USB serial).
- **Steam Deck / SteamOS:** Treat as **Linux x86_64** unless a separate build is provided; no L3 change.

---

## 4. Backend: SummerCart64 (reference target for v1)

- **Role:** Map the host’s `read`/`write` byte stream to/from the SC64’s documented PC↔device protocol (see official **SummerCart64** repository: USB interface and N64 command documentation).
- **N64 side:** Firmware/game uses SC64 primitives to exchange application bytes that encode **L3 frames** end-to-end.

Normative for Multi64: the **same L3 bytes** MUST be recoverable on the PC after a full round-trip through the SC64 path once the adapter is correct.

Concrete mapping for this repository: **[l3-over-sc64.md](./l3-over-sc64.md)** (`USB_WRITE` / `PKT` **DATA**).

---

## 5. Backend: EverDrive 64 X7 (draft in repo)

- **Scope:** USB-capable **X7-class** carts only; models without USB (e.g. X5) **cannot** implement this L2.
- **Role:** Map `read`/`write` to the vendor USB model (host serial / usb64-style framing). **Normative mapping (draft):** [**l3-over-everdrive-x7.md**](./l3-over-everdrive-x7.md). References: **N64brew** wiki (EverDrive-64 X7), **[krikzz/ed64-x-pub](https://github.com/krikzz/ed64-x-pub)** (reference `usb64` + N64 samples), Krikzz dev pack.
- **Implementation:** Rust crate **`multi64-ed64-l2`** (`crates/ed64-l2`) — implements the §4 host wire rules and performs real serial I/O. It has never been run against a cart, so treat a failure as possibly the mapping rather than the ROM (see [§4.5](./l3-over-everdrive-x7.md)).

### 5.1 Normative constraints for adapters (inform interoperability)

Implementations SHOULD document how they satisfy these; L3 already caps `PAYLOAD_LEN` at handshake so games can size buffers.

| Topic | Guidance |
|-------|----------|
| **Chunk size** | ED64 USB paths often expose fixed-size I/O (e.g. 512-byte data areas). The L2 adapter MAY buffer partial reads/writes until a full L3 frame is assembled or a write block is filled. |
| **Padding** | If the hardware requires fixed block sizes, the adapter MAY pad writes and strip padding on reads **without** changing L3 content: padding MUST NOT appear inside the L3 stream as seen by L3 parsers (adapter strips it). |
| **Min read/write sizes** | If the device requires minimum transfer sizes, the L2 adapter MUST coalesce/split transparently. |

These rules keep **one L3 spec** for all carts; differences stay inside L2/L1.

---

## 6. Error mapping

L2 SHOULD surface to the daemon:

- **Open failures** (device not found, permission denied).
- **I/O errors** (USB stall, disconnect mid-frame).

The daemon MAY translate irrecoverable L2 errors into L3 `ERROR` / `SESSION_RESET` toward clients, or close the session. Exact policy is implementation-defined but SHOULD be documented.

---

## 7. Testing expectations

- **Unit tests:** L3 framing without hardware (loopback buffer).
- **Hardware tests:** Echo or handshake tests per backend when hardware is available.
- **CI:** At minimum, L3 tests on **Linux and Windows** runners; macOS optional.

---

## 8. Revision

**Spec-Revision** counts edits to **this** document only. It is **independent** of L3 **Protocol-Major/Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).

| Value | Meaning |
|-------|---------|
| **1** | L2 link contract (SC64, EverDrive **§5** / **ed64-x-pub**, …); pre-release tree. |

**Normative compatibility (preserved):** The obligations in §1–7 — ordered bidirectional **L3 octet stream**, **L2** does not interpret L3, backends MAY pad/split only **outside** the L3 view, **same L3** across carts — remain **fixed requirements** for any conforming adapter. Future edits to this spec MUST NOT contradict those rules without a coordinated L3 change.

**Spec-Revision policy:** All normative docs use **Spec-Revision** **1** for now; edits **accumulate** under **1** until maintainers announce a bump. This is independent of L3 **Protocol-Major** / **Protocol-Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).
