# L3 — Bridge protocol (version 1)

**Spec-Revision:** 1  
**Protocol-Major:** 1  
**Protocol-Minor:** 0  

**Pre-release:** Implementations in this repo target a single **L3 version 1** wire format (**Protocol-Major** 1, **Protocol-Minor** 0). There is **no** commitment to older or alternate wire layouts until an explicit release is published.

This document defines the **Multi64 L3** wire format: a cart-agnostic, bidirectional message layer between a **host** (PC daemon) and **firmware/game code** on the Nintendo 64. It does **not** define game semantics (items, flags, randomizer payloads); those are opaque application bytes carried inside L3.

---

## 1. Design constraints

- **Endianness:** All multi-byte integers are **big-endian** (network byte order) unless stated otherwise.
- **Alignment:** No implicit padding in the wire format; fields are packed as described.
- **Opaque payloads:** Bytes in the `PAYLOAD` region are opaque to L3 unless the frame type says otherwise.
- **Transport:** L3 assumes a **reliable ordered byte stream** between host and device. Mapping that stream to USB/serial is defined in [l2-link-adapter.md](./l2-link-adapter.md).

---

## 2. Frame format

Every L3 frame has a **fixed 16-byte header** followed by an optional **payload** of `PAYLOAD_LEN` bytes.

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                        MAGIC (4 bytes)                        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       TYPE (1)       |    CHANNEL (1)    |      FLAGS (2)     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                        REQUEST_ID (4)                         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                        PAYLOAD_LEN (4)                        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                      PAYLOAD (variable)                       |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

| Field | Size | Description |
|-------|------|-------------|
| `MAGIC` | 4 | Constant `0x4D363442` — ASCII **`M64B`**. If `MAGIC` is wrong, the receiver MUST discard input until the next valid `MAGIC` (resynchronization). |
| `TYPE` | 1 | Frame type; see §3. |
| `CHANNEL` | 1 | Logical channel; see §4. Ignored where noted per frame type. |
| `FLAGS` | 2 | Bitfield; see §5. |
| `REQUEST_ID` | 4 | Correlates related frames (e.g. request/response). Zero means “no correlation” unless a frame type defines otherwise. |
| `PAYLOAD_LEN` | 4 | Length in bytes of `PAYLOAD`. MUST be ≤ `MAX_PAYLOAD` negotiated in handshake (§6). |
| `PAYLOAD` | `PAYLOAD_LEN` | Frame-type-specific or opaque data. |

**Total frame size:** `16 + PAYLOAD_LEN` bytes.

---

## 3. Frame types (`TYPE`)

| Value | Name | Direction | Meaning |
|-------|------|-----------|---------|
| `0x01` | `HANDSHAKE` | Host → Device (first) | Host proposes protocol version and limits. |
| `0x02` | `HANDSHAKE_OK` | Device → Host | Device accepts parameters. |
| `0x03` | `HANDSHAKE_REJECT` | Device → Host | Device refuses; payload carries error (§7). |
| `0x10` | `DATA` | Either | Application or control payload on `CHANNEL`. |
| `0x11` | `ACK` | Either | Optional positive acknowledgment for a prior `DATA` (see `FLAGS`). |
| `0x12` | `ERROR` | Either | Session or protocol error; payload carries error (§7). |
| `0x20` | `HEARTBEAT` | Either | Keepalive; `PAYLOAD_LEN` MAY be 0. |
| `0x21` | `HEARTBEAT_ACK` | Either | Response to `HEARTBEAT`. |
| `0x30` | `SESSION_OPEN` | Host → Device (first) | Optional session layer: open a logical session (§11). |
| `0x31` | `SESSION_OPEN_OK` | Device → Host | Session accepted; echoes effective session id. |
| `0x32` | `SESSION_CLOSE` | Either | Graceful session teardown request (§11). |
| `0x33` | `SESSION_CLOSE_ACK` | Either | Acknowledges `SESSION_CLOSE`. |

Any other `TYPE` value: receiver MUST respond with `ERROR` and code `UNKNOWN_FRAME_TYPE` if a response is expected; otherwise MAY drop and resync.

---

## 4. Channels (`CHANNEL`)

| Value | Name | Use |
|-------|------|-----|
| `0x00` | `APPLICATION` | Opaque game/randomizer payloads. |
| `0x01` | `LOG` | UTF-8 text or implementation-defined log lines (still opaque to L3). |
| `0x02` | `CONTROL` | Session control payloads reserved by L3 or implementations (e.g. future extensions). |

Values `0x03`–`0x7F` are reserved for future standard channels. Values `0x80`–`0xFF` are **experimental** (private use between named implementations).

---

## 5. Flags (`FLAGS`)

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x0001` | `FINAL` — For segmented transfers (future); in v1, MUST be set for every `DATA` frame unless both sides negotiated otherwise (default: **always set**). |
| 1–15 | — | Reserved; MUST be sent as 0 and ignored on receive in v1. |

---

## 6. Handshake

After the L2 byte stream is established:

1. The **host** sends exactly one `HANDSHAKE` frame (`TYPE=0x01`, `CHANNEL=0x02`).

**`HANDSHAKE` payload layout (binary, 12 bytes):**

| Offset | Size | Field |
|--------|------|--------|
| 0 | 1 | `PROTO_MAJOR` — MUST be `1` for this document. |
| 1 | 1 | `PROTO_MINOR` — MUST be `0` for L3 version 1 (this document). |
| 2 | 2 | Reserved; MUST be `0`. |
| 4 | 4 | `MAX_PAYLOAD` — Maximum `PAYLOAD_LEN` this host will send (and requests the device honor for frames it sends). MUST be ≥ `64` and ≤ `1048576` (1 MiB). |
| 8 | 4 | `FEATURES` — Bitfield; see §6.1. Bits not defined here MUST be `0`. |

2. The **device** responds with either:
   - `HANDSHAKE_OK` (`TYPE=0x02`), **or**
   - `HANDSHAKE_REJECT` (`TYPE=0x03`) with `ERROR` payload (§7), **or**
   - `ERROR` (`TYPE=0x12`) with code `VERSION_UNSUPPORTED` or similar.

**`HANDSHAKE_OK` payload (8 bytes):**

| Offset | Size | Field |
|--------|------|--------|
| 0 | 1 | `PROTO_MAJOR` — MUST match agreed major. |
| 1 | 1 | `PROTO_MINOR` — MUST match agreed minor. |
| 2 | 2 | Reserved; MUST be `0`. |
| 4 | 4 | `MAX_PAYLOAD` — The **effective** maximum for this session: `min(host MAX_PAYLOAD, device limit)`. All subsequent frames MUST use `PAYLOAD_LEN` ≤ this value. |

Until `HANDSHAKE_OK` is received by the host, the host MUST NOT send `DATA` except as required by an implementation-specific bootstrap (none in v1).

### 6.1 `FEATURES` bitfield (handshake)

| Bit | Mask | Name | Meaning |
|-----|------|------|--------|
| 0 | `0x00000001` | `SESSION_LAYER` | Request session-layer behavior (§11): `SESSION_OPEN` / `SESSION_OPEN_OK` before `APPLICATION` `DATA`. If clear, `SESSION_*` frames MUST NOT be sent and `DATA` on `APPLICATION` is allowed immediately after `HANDSHAKE_OK`. |
| 1–31 | — | — | Reserved; MUST be `0` on send and ignored on receive in v1. |

If the host sets `SESSION_LAYER` and the device does not implement it, the device MUST respond with `HANDSHAKE_REJECT` or `ERROR` with code `FEATURE_UNSUPPORTED` (§8).

---

## 7. Error payload (`ERROR`, `HANDSHAKE_REJECT`)

When `TYPE` is `ERROR` or `HANDSHAKE_REJECT`, `PAYLOAD` MUST be at least **4 bytes**:

| Offset | Size | Field |
|--------|------|--------|
| 0 | 2 | `CODE` — `u16` big-endian (see §8). |
| 2 | 2 | `MESSAGE_LEN` — Length of UTF-8 message in bytes (MAY be `0`). |
| 4 | `MESSAGE_LEN` | UTF-8 text (no BOM), optional human-readable detail. |

If `PAYLOAD_LEN` is less than `4 + MESSAGE_LEN`, the frame is malformed.

---

## 8. Error codes (`CODE`)

| Code | Name | Notes |
|------|------|--------|
| `0x0000` | `UNSPECIFIED` | |
| `0x0001` | `VERSION_UNSUPPORTED` | Handshake failed. |
| `0x0002` | `FRAME_TOO_LARGE` | `PAYLOAD_LEN` exceeds negotiated `MAX_PAYLOAD`. |
| `0x0003` | `MALFORMED_FRAME` | Header or payload invalid. |
| `0x0004` | `UNKNOWN_FRAME_TYPE` | |
| `0x0005` | `BUFFER_FULL` | Device or host cannot accept more data. |
| `0x0006` | `TRANSPORT_TIMEOUT` | L2 idle or stalled beyond policy. |
| `0x0007` | `SESSION_RESET` | Logical reset; counterpart SHOULD re-handshake if continuing. |
| `0x0008` | `FEATURE_UNSUPPORTED` | Handshake or session: requested optional feature is not available. |
| `0x0009` | `SESSION_LAYER_REQUIRED` | `SESSION_*` frame received but `SESSION_LAYER` was not negotiated. |
| `0x000A` | `SESSION_NOT_OPEN` | Application `DATA` on `APPLICATION` before session open completed (when `SESSION_LAYER` is active). |

---

## 9. Heartbeat

Either side MAY send `HEARTBEAT` (`TYPE=0x20`). The peer SHOULD respond with `HEARTBEAT_ACK` (`TYPE=0x21`) with the same `REQUEST_ID` if non-zero. `PAYLOAD_LEN` MAY be zero.

No mandatory interval is specified in v1; implementations SHOULD document their default (e.g. every 2–5 seconds) and behavior when `HEARTBEAT_ACK` is missing (e.g. emit `ERROR` / `SESSION_RESET` or close L2).

---

## 10. `DATA` and `ACK`

- `DATA` (`TYPE=0x10`) carries opaque bytes on the given `CHANNEL`.
- `ACK` (`TYPE=0x11`) MAY be used when an implementation requires explicit delivery confirmation; `REQUEST_ID` SHOULD match the `DATA` being acknowledged. v1 does **not** require `ACK` for every `DATA`.

---

## 11. Optional session layer

When **`SESSION_LAYER`** is negotiated (§6.1), peers use an explicit **session open** step on `CHANNEL` **`CONTROL`** (`0x02`) before exchanging **`DATA`** on **`APPLICATION`** (`0x00`). When `SESSION_LAYER` is **not** negotiated, `SESSION_*` frames MUST NOT be sent; receivers SHOULD respond with `ERROR` / `SESSION_LAYER_REQUIRED` (or MAY drop and resync per policy).

### 11.1 Ordering

After `HANDSHAKE_OK`:

1. The **host** sends **`SESSION_OPEN`** (`TYPE=0x30`, `CHANNEL=CONTROL`).
2. The **device** responds with **`SESSION_OPEN_OK`** (`TYPE=0x31`, `CHANNEL=CONTROL`).
3. Either side may then send **`DATA`** on **`APPLICATION`** (and other channels per policy).

Until step 2 completes, the host MUST NOT send `DATA` on `APPLICATION`. The device SHOULD NOT send `DATA` on `APPLICATION` until after it has sent `SESSION_OPEN_OK` (unless an implementation documents an exception).

If `SESSION_LAYER` is not negotiated, steps 1–2 are skipped; `DATA` on `APPLICATION` is allowed immediately after `HANDSHAKE_OK`.

### 11.2 `SESSION_OPEN` payload

| Offset | Size | Field |
|--------|------|--------|
| 0 | 4 | `SESSION_ID` — `u32` big-endian. **`0`** means “assign a non-zero id”; otherwise the device SHOULD use this value as the effective session id if acceptable. |
| 4 | 2 | Reserved; MUST be `0`. |

Total: **6** bytes.

### 11.3 `SESSION_OPEN_OK` payload

| Offset | Size | Field |
|--------|------|--------|
| 0 | 4 | `SESSION_ID` — `u32` big-endian effective session id (non-zero unless both sides agree otherwise). |

Total: **4** bytes.

### 11.4 `SESSION_CLOSE` and `SESSION_CLOSE_ACK`

**`SESSION_CLOSE`** (`TYPE=0x32`, `CHANNEL=CONTROL`) — graceful teardown of the logical session (the L2 stream may remain up for re-handshake).

| Offset | Size | Field |
|--------|------|--------|
| 0 | 2 | `REASON` — `u16` big-endian; `0` = normal shutdown; other values implementation-defined. |
| 2 | 2 | `MESSAGE_LEN` — UTF-8 diagnostic length (MAY be `0`). |
| 4 | `MESSAGE_LEN` | Optional UTF-8 text. |

**`SESSION_CLOSE_ACK`** (`TYPE=0x33`, `CHANNEL=CONTROL`) — `PAYLOAD_LEN` MAY be `0`. `REQUEST_ID` MAY match the `SESSION_CLOSE` being acknowledged if non-zero.

After a close, implementations MAY send a new `HANDSHAKE` / `SESSION_OPEN` sequence or tear down L2.

---

## 12. Versioning

### 12.1 Wire protocol (`PROTO_MAJOR` / `PROTO_MINOR`)

These fields appear in the **handshake** and identify the L3 wire level peers implement.

- This document defines **Protocol-Major** **1** and **Protocol-Minor** **0**.
- Future **major** bumps denote incompatible framing or required behavior; peers reject unknown majors at handshake.
- **Minor** bumps (future) denote compatible extensions (new optional frame types, `FEATURES` bits, etc.).

**Normative compatibility (preserved):** Peers that both accept **major 1** MUST interoperate at the framing and handshake rules in §1–11 for the **minor** levels they negotiate. Changes that break on-the-wire compatibility require a **Protocol-Major** bump and a new spec document.

### 12.2 Document (`Spec-Revision`)

**Spec-Revision** counts edits to **this Markdown document** only. It is **not** the same as **Protocol-Major/Minor** (wire level).

| Value | Meaning |
|-------|---------|
| **1** | L3 v1 protocol (`M64B`, handshake, channels); pre-release tree. |

**Spec-Revision policy:** This document stays at **Spec-Revision** **1** for now; prose edits **accumulate** under **1** until maintainers announce a bump. **Wire-level** incompatible changes still require **Protocol-Major** or **Protocol-Minor** bumps per §12.1 — **Spec-Revision** tracks Markdown-only drift separately from those counters.

---

## 13. Reference implementation notes (non-normative)

- Host-side parsers SHOULD buffer until `16 + PAYLOAD_LEN` bytes are available, then validate `MAGIC` and length caps.
- N64-side code SHOULD use a fixed buffer of size `16 + MAX_PAYLOAD` after handshake.
