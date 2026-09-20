# L3 APPLICATION — RDRAM peek/poke (M64P, v0)

**Spec-Revision:** 1  

Payloads carried in L3 **`DATA`** frames on **`CHANNEL = APPLICATION` (`0x00`)** that let a host read and write **console RDRAM**, and read the **cartridge ROM**, while a ROM runs.

**Magic:** ASCII **`M64P`** — bytes `0x4D 0x36 0x34 0x50`.

Deliberately **game-agnostic**: it exposes bytes at addresses and nothing else. The cart is a PI-bus slave and cannot reach RDRAM on its own, so every request is serviced by code running on the console. Where that code lives is out of scope here — [`../../n64/README.md`](../../n64/README.md) implements it as a mode of the test ROM; a game-resident agent would implement the same messages.

M64P shares the APPLICATION channel with **`M64T`** ([`test-l3-application-v0.md`](./test-l3-application-v0.md)); receivers dispatch on the 4-byte magic.

---

## 1. Payload layout

| Offset | Size | Description |
|--------|------|-------------|
| `0`–`3` | 4 | Magic **`M64P`** |
| `4` | 1 | `msg` — request or response code (§2–§3) |
| `5`– | * | Body; length = `L3_PAYLOAD_LEN - 5` |

`L3_PAYLOAD_LEN` is the APPLICATION payload length from the L3 header ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md)). All multi-byte integers are **big-endian**.

Every request carries a **`rid`** (`uint16`), echoed in its response. A host may therefore match a reply after an L3 resync; `rid` is opaque to the cart.

---

## 2. Host → cart (requests)

| `msg` | Name | Body |
|-------|------|------|
| `0x01` | `HELLO` | Empty |
| `0x02` | `PEEKV` | `rid:uint16`, `n:uint8`, then `n` × (`addr:uint32`, `len:uint16`) |
| `0x03` | `POKEV` | `rid:uint16`, `n:uint8`, then `n` × (`addr:uint32`, `len:uint16`, `len` bytes) |
| `0x04` | `PEEKROM` | As `PEEKV`; addresses are cartridge ROM offsets (§4.2) |
| `0x05` | `WATCH` | `rid:uint16`, `n:uint8`, then `n` × (`addr:uint32`, `len:uint8`, `at:uint8`, `nvalues:uint8`, `nvalues` bytes) — §4.3 |

`HELLO` has no `rid`; `HELLO_ACK` carries none either.

---

## 3. Cart → host (responses)

| `msg` | Name | Body |
|-------|------|------|
| `0x81` | `HELLO_ACK` | `proto:uint8`, `agent_ver:uint16`, `rdram_bytes:uint32`, `flags:uint8` |
| `0x82` | `PEEKV_RESP` | `rid:uint16`, `n:uint8`, then `n` × (`len:uint16`, `len` bytes) — same order as the request |
| `0x83` | `POKE_ACK` | `rid:uint16`, `applied:uint8` — count of regions written |
| `0x84` | `PEEKROM_RESP` | As `PEEKV_RESP` |
| `0x85` | `WATCH_ACK` | `rid:uint16`, `watching:uint8` — slots now watched |
| `0xE0` | `ERR` | `rid:uint16`, `code:uint8` (§5) |

`proto` is **0** for this revision. `flags` bit `0` set means writes are accepted; a read-only agent clears it and answers `POKEV` with `ERR`/`E_READONLY`.

`flags` bit `1` set means the agent answers `PEEKROM`, and `HELLO_ACK` then carries one more field after `flags`: `rom_bytes:uint32`, the size of the ROM window `PEEKROM` may address (§4.2). An agent without it clears bit `1`, sends no `rom_bytes`, and answers `PEEKROM` with `ERR`/`E_UNSUPPORTED`. The field is appended, so every earlier field stays where a host that predates it reads it, and such a host can ignore both the bit and the extra four bytes.

`flags` bit `2` set means the agent watches slots (§4.3), and `HELLO_ACK` carries `watch_slots:uint8` — how many it can watch at once — after `rom_bytes` if that is present, else straight after `flags`. **Appended fields appear in bit order, each present only if its bit is set**, so a host reads them in that order and an agent with neither sends neither. An agent with bit `2` clear answers `WATCH` with `ERR`/`E_UNSUPPORTED`. Other `flags` bits are reserved and sent as `0`.

---

## 4. Addressing and limits

Addresses are **RDRAM physical offsets** — `0` is the start of RDRAM, not a KSEG0 virtual address. This is the convention N64 debuggers and memory tools already use, so a host needs no translation layer.

| Limit | Value | Why |
|-------|-------|-----|
| `n` per request | **32** | Enough to gather scattered state in one round trip |
| Bytes per request or response | **7936** | Keeps the whole frame inside one 8192-byte L3 payload, and so one `usb_write` |
| `len` per region | **4096** | Bounds a single copy inside a frame; also fits a whole save context in one region |

The 7936 comes from the worst case, which is a `POKEV` **request**: its per-region
header is 6 bytes against the 2 in a `PEEKV` response, so the wire cost is
`8 + 6n + total`. At `n = 32` that is `200 + total`, leaving 7992 under an 8192-byte
payload; 7936 takes the round number below it.

Round-trip latency dominates transfer time by orders of magnitude — measured at ~67 ms
per exchange on SC64 hardware, near-flat in payload size, because it is set by the
ROM's per-frame polling rather than the link. Requests should therefore be packed as
full as these limits allow; a caller that splits work across more exchanges than
necessary pays ~67 ms for each one.

A request that exceeds any limit is answered with `ERR`, not truncated. `addr + len` beyond RDRAM is `E_RANGE` — the cart must range-check rather than fault, since a bad address from the host would otherwise bus-error the console.

`n = 0` is legal and returns an empty `PEEKV_RESP` / `POKE_ACK` with `applied = 0`, and an empty `PEEKROM_RESP`.

### 4.1 Consistency

All regions in one request are serviced **in a single pass, from the ROM's per-frame hook**, never from an interrupt. Every read in a `PEEKV` therefore observes one consistent point in the frame, and a `POKEV` lands entirely between frames rather than mid-update.

A host may therefore treat one request as atomic with respect to the running ROM. That property comes from the **hook site**, not the transport: an implementation that services M64P from an interrupt does not satisfy this section even though its wire format is identical.

Access is through **cached KSEG0**. The game manipulates its own structures with the CPU, so cached access is what stays coherent; uncached reads can return data the CPU has not written back.

### 4.2 Cartridge ROM (`PEEKROM`)

Hosts need the ROM as well as RAM: tools read it to recognise the game and to find data the game's patch wrote there, such as a player's login key. `PEEKROM` reads it from the cart itself, so what the host sees is the image the console is running, not a file that is meant to match it.

- **Addresses are ROM offsets:** `0` is the first byte of the ROM, which the console sees on the PI bus at `0x10000000`. Any `addr` and `len` are allowed; alignment is the agent's problem, not the host's.
- **Limits are §4's:** 32 regions, 4096 bytes per region, 7936 bytes per request. `addr + len` beyond `rom_bytes` is `E_RANGE`.
- **`rom_bytes` is a window, not the image size.** Nothing on the console records how large the booted image is, so an agent reports the most it can address (64 MiB, the largest N64 ROM). Past the end of a smaller image a cart returns whatever it maps there. A host that needs the image size must learn it elsewhere.
- **The agent reads the ROM over the PI bus, between the game's own transfers,** under the same rules as its cart driver: it never writes the PI control registers, it checks the PI is idle and masks interrupts around each short burst of loads, and every wait is bounded. If the PI stays busy past that bound, the whole request is answered with `ERR`/`E_BUSY` and nothing else. The host may simply retry.
- **No consistency question arises:** the ROM does not change while the console runs. A host may therefore cache what it has read, but must drop that cache whenever the console may have been reset or another image loaded. The M64P link going silent and a new `HELLO` is the signal it has.

### 4.3 Watched slots (`WATCH`)

Some games record "this just happened" in one place that the next event overwrites, and
nothing else says it happened until much later. A host polling over USB reads such a slot
once per exchange at best, so events between its reads are gone before it looks. The
agent runs every frame, which is where the game writes them.

`WATCH` gives the agent a list of slots to follow. Each is an RDRAM `addr`, a `len` of **1 to
8** bytes, and a filter: byte `at` within the slot must equal one of `nvalues` listed bytes, with
`nvalues = 0` meaning every change is kept and `at` ignored. `n = 0` clears every watch, which is
also the state after `HELLO`. `WATCH_ACK` reports how many slots are watched, which on success
is `n`.

A request is refused whole, leaving the slots the host set last time untouched:

| Condition | Error |
|-----------|-------|
| `n` above `watch_slots` | `E_TOO_MANY` |
| `len` of 0, `len` above 8, or `nvalues` above 8 | `E_TOO_LARGE` |
| `addr + len` outside RDRAM, or `at` not less than `len` while `nvalues` is non-zero | `E_RANGE` |

A `len` of 0 names no bytes to compare, and an `at` outside the slot could never match, so both
are refused rather than watched and never reported.

What the agent does with them, once per frame, from the same hook as §4.1:

- It reads each slot and compares it with what it read last frame. The first read after a
  `WATCH` is a baseline and is never an event.
- A change **to all zero bytes** is not an event: a slot being cleared is the game
  finishing with it, not something happening.
- Any other change whose filter admits it is appended to one queue, shared by all slots.
  The queue holds **16** events; a seventeenth drops the oldest and counts as dropped.

The queue is returned to the host on responses it was already sending. **While any slot is
watched, `PEEKV_RESP` carries a trailer** after its last region:

| Offset | Size | Description |
|--------|------|-------------|
| `0` | 1 | `n` — events in this response |
| `1` | 2 | `dropped:uint16` — events lost to a full queue since `WATCH`, saturating |
| `3`– | * | `n` × (`slot:uint8`, `len:uint8`, `len` bytes), oldest first |

An event is removed from the queue when it is put in a trailer. The agent sends as many as
the response has room for under §4's total, and the rest follow on later responses. A host
that has set no watch never sees a trailer, so this changes nothing for one that does not
use the feature, and a response with no room for the trailer's three bytes carries none.

**What a queued event is, and is not.** It is a value the slot really held, at a frame
boundary, in the order it held them. It is not a claim about the slot now: by the time a
host reads one, the game has usually moved on, which is the whole point. Nor is it every
value the slot ever held — a game that writes it twice between two frames leaves only the
second, exactly as a host reading the slot every frame would see. An agent that cannot
sample once per frame must leave bit `2` clear rather than sample less often, since a host
cannot tell a slow sampler from a quiet game.

---

## 5. Error codes

| Code | Name | Meaning |
|------|------|---------|
| `0x01` | `E_MALFORMED` | Body shorter than the declared regions require |
| `0x02` | `E_TOO_MANY` | `n` exceeds 32, or the watched slots the agent has room for (§4.3) |
| `0x03` | `E_TOO_LARGE` | Region or total exceeds the §4 limits, or a watch's `len` is 0 or its `len` or `nvalues` exceeds §4.3's 8 |
| `0x04` | `E_RANGE` | `addr + len` outside RDRAM, or a watch's filter byte `at` is outside its slot (§4.3) |
| `0x05` | `E_READONLY` | `POKEV` on an agent that does not accept writes |
| `0x06` | `E_UNSUPPORTED` | `PEEKROM` on an agent that does not read the cart ROM (`flags` bit `1` clear), or `WATCH` on one that does not watch slots (bit `2` clear) |
| `0x07` | `E_BUSY` | `PEEKROM` could not get the PI bus within the agent's bound; nothing was read. Retry |

An agent that predates a request type answers it with `E_MALFORMED`, as any unknown `msg`.
---

## 6. Revision

| Spec-Revision | Change |
|---------------|--------|
| **1** | M64P v0: `HELLO`, `PEEKV`, `POKEV`. No change to the L3 byte contract — this is an APPLICATION payload, so **Protocol-Major/Minor are unaffected**. Per-request byte cap set to 7936 (§4) after hardware measurement showed latency is per-exchange, not per-byte. |
| *unassigned* | `PEEKROM` / `PEEKROM_RESP` (§4.2), `HELLO_ACK` `flags` bit `1` and `rom_bytes`, `E_UNSUPPORTED`, `E_BUSY`. Additive: `proto` stays **0**, every earlier field keeps its offset, and an agent without `PEEKROM` still conforms. L3 **Protocol-Major/Minor are unaffected**. The Spec-Revision number is the maintainer's to assign. |
| *unassigned* | `WATCH` / `WATCH_ACK` and the `PEEKV_RESP` trailer (§4.3), `HELLO_ACK` `flags` bit `2` and `watch_slots`. Additive: `proto` stays **0**, the trailer appears only for a host that asked for it, and an agent that does not watch still conforms. L3 **Protocol-Major/Minor are unaffected**. The Spec-Revision number is the maintainer's to assign. |
