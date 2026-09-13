# The cart agent

> [Integration guides](README.md) · [Source](../../n64/agent/README.md) · [M64P spec](../spec/memory-l3-application-v0.md)

The agent is what services [M64P](../spec/memory-l3-application-v0.md) inside a running game.
The cart is a PI-bus slave and cannot reach RDRAM itself, so code on the console has to do
it; this is that code, written to be dropped into a ROM its authors did not design for it.

---

## 1. The contract

| | |
|---|---|
| **Call** | `agent_tick()`, once per frame |
| **From** | the game's own thread, after its game logic — **never** from an interrupt or exception handler ([spec §4.1](../spec/memory-l3-application-v0.md#41-consistency)) |
| **Stack** | about 300 bytes at the deepest point, measured |
| **RAM** | about 21 KB: 4.2–4.7 KB code, 0–1.1 KB data (both depend on compiler and flags), 16.4 KB BSS |
| **Symbols** | none undefined. Nothing from libultra, libdragon, the C library or the game |
| **Cart** | SummerCart64 only |

The per-frame rule is not style. M64P promises that every region in one request is read at
one consistent point in the frame and that a write lands between frames. That property comes
from **where the agent is called**, not from anything in the agent: the same code serviced from
an interrupt would put identical bytes on the wire and break the guarantee.

How often "once per frame" is depends on the site. One integration hooked a callback that runs
on every other retrace and ticked at 30 Hz; another hooked the main loop and ticked at 60 Hz.
Both are fine. What matters is that the rate is steady and the site is not an interrupt.

## 2. What it costs, and why

Most of the RAM is two buffers sized for one maximum M64P exchange:

```
s_rx   8296 B   receives one L3 frame
s_tx   8040 B   holds the outgoing frame; PEEKV responses are built straight into it
```

Replies are built in place through `m64p_reply_buffer()` rather than in a third buffer and
copied, which saved about 8 KB. That hook exists because a game with no spare RAM is exactly
where an agent is hardest to place. If you supply the hooks yourself: the reply buffer must not
overlap the request being read, and `m64p_transport_send()` must not copy a payload that is
already in place. See [`mem_proto.h`](../../n64/test-rom/mem_proto.h).

## 3. What it does to the machine

An injected agent touches the PI bus while the game is using it. These rules are in
[`sc64.c`](../../n64/agent/sc64.c), and each one was paid for.

- **It never writes the PI control registers** (`DRAM_ADDR`, `CART_ADDR`, `RD_LEN`, `WR_LEN`).
  Those belong to whatever DMA the game has in flight. Data moves by CPU load and store through
  the uncached window instead, so the agent owns no PI state at all.
- **It masks interrupts around each access**, after checking the PI is idle, so the game's PI
  manager cannot start a transfer underneath it. Bulk copies are chunked at 64 words, so
  interrupts are never masked long enough to glitch audio or video.
- **Every wait is bounded.** An earlier version spun without limit, inside the masked section,
  waiting for the PI to go idle — and hard locked a game at room loads, where its own DMA held
  the bus longest. A bounded wait that reports "no cart" is always better than a frozen console.
- **Stores to PI space are posted; loads stall.** Back-to-back stores were silently dropped
  while reads worked, so the staging copy now waits on `IO_BUSY` per store. The symptom was a
  well-formed reply frame carrying the *previous* request, which read as a protocol bug. The
  write path now reads back the ends of what it staged and drops the reply if they differ: a
  clean timeout beats a plausible lie.
- **Packets are staged in the SC64's BlockRAM buffer** (`0x1FFE0000`, 8 KiB), not at the top of
  the ROM window where libdragon stages. BlockRAM cannot collide with a ROM of any size, so
  nothing in the agent depends on how large the running ROM is.
- **It goes dormant permanently.** With no cart answering, it tries `sc64_init()` 16 times and
  then never again for that boot. It used to retry every frame, which is four PI accesses and
  eight spins sixty times a second forever — harmless in testing, because testing always had a
  cart, and a hard lock everywhere else. It was found in an emulator.

What is still open: the complete fix for PI sharing is to take the bus the way libultra does
(`osPiGetAccess`/`osPiRelease`, or the non-raw `osPiReadIo`/`osPiWriteIo`). An integration with
the game's addresses for those can do better than masking. None so far has had them.

**Latency**, measured on a SummerCart64: about 67 ms per M64P round trip against the test ROM,
and 67–100 ms inside games, where the per-store PI waits and the game's own frame loop add to
it. Round trips, not bytes, are the cost — pack requests as full as the spec's limits allow.

## 4. Building it

```sh
cd n64/agent
make                           # build/m64p_agent.o with libdragon's mips64-elf
make PREFIX=mips64-ultra-elf-  # with a libultra toolchain
make symbols                   # sizes, exports, undefined (must be empty)
```

The default flags are `-mabi=32 -G0 -mno-gpopt -mno-abicalls -fno-pic -march=vr4300`: o32, no
`$gp`-relative data, no position-independent code. The agent runs inside a game whose `$gp` and
relocation model are its own, so it must not rely on either.

**Match the objects you link beside.** If the agent is linked into an existing payload or a
decomp build, override `CFLAGS` with that build's flags. `-mgpopt` in particular has to agree
with the objects around it, or the link produces relocations the other side cannot resolve.

**No `<stdint.h>`?** Every fixed-width type comes from
[`m64p_types.h`](../../n64/test-rom/m64p_types.h), whose default includes `<stdint.h>` and
`<stddef.h>`. A `-nostdinc` build (common in decompilations) supplies its own copy defining
`uint8_t`, `uint16_t`, `uint32_t` and `size_t`, put first on the include path, and changes
nothing else.

**Three ways it has gone in**, all on hardware:

| Your build | Use | Notes |
|---|---|---|
| An existing payload built by an assembler | `build/m64p_agent.o` appended to the payload | Append-only: retarget an existing call rather than inserting one, so no existing symbol moves |
| A decompilation's own build | The three `.c` files as a segment of that build | The decomp's compiler (even GCC 2.8.1) builds it unchanged |
| No source at all | [`templates/link-flat.sh`](../../n64/agent/templates/link-flat.sh) | A flat image at a fixed address plus a hook stub; see [placing-the-agent.md §5](placing-the-agent.md#5-build-and-splice) |

After any build, run `nm -u` on the result. If anything is listed, the "no undefined symbols"
claim has rotted and the ROM will call through garbage.

## 5. Copies and forks

`mem_proto.c` is shared between the test ROM and the agent so there is exactly one M64P
implementation. A downstream project that vendors the agent should copy `n64/agent/*.c`, `*.h`
and `n64/test-rom/mem_proto.*`, record the multi64 commit they came from, and re-copy rather
than edit. A local fix to `mem_proto` is a fork of the protocol.
