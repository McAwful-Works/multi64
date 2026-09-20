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
| **RAM** | about 24 KB: 6.7–8.3 KB code, 0–1.1 KB data (both depend on compiler, flags and cart), 16.4–19 KB BSS |
| **Symbols** | none undefined. Nothing from libultra, libdragon, the C library or the game |
| **Cart** | SummerCart64. EverDrive-64 X7 and PRO builds exist (`CART=ed64`, `CART=ed64pro`) and have **never run on a cart**; see §6 |

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

### 2.1 Two calls per frame

A hook calls `agent_tick()`, which polls the cart and services whatever arrived. It also calls
`m64p_watch_tick()` — in `agent.c` that is the first thing `agent_tick()` does, so an integration
using this agent gets it for free; one supplying its own hook must make both calls.

`m64p_watch_tick()` is what makes a **watched slot** mean anything
([spec §4.3](../spec/memory-l3-application-v0.md#43-watched-slots-watch)). A host can ask the agent
to follow a few bytes the game rewrites between its polls — the place a game records "this just
happened" and overwrites next time. The agent reads them every frame, queues what changed, and the
queue rides back on responses that were already being sent, so it costs no exchange. Reading those
bytes costs a handful of byte compares, and nothing at all until a host asks.

It must run **every frame, whether or not a request arrived**. A host that sets a watch is promised
a per-frame sample and cannot tell a slow sampler from a quiet game, so a hook that cannot make the
call that often must report no watch support rather than call it less.

## 3. What it does to the machine

An injected agent touches the PI bus while the game is using it. These rules are in
[`sc64.c`](../../n64/agent/sc64.c), and each one was paid for. The EverDrive drivers follow them
through [`pi_io.c`](../../n64/agent/pi_io.c), and so does every build's
[`cart_rom.c`](../../n64/agent/cart_rom.c), which reads the cartridge ROM for `PEEKROM`
([spec §4.2](../spec/memory-l3-application-v0.md#42-cartridge-rom-peekrom)).

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
make CART=ed64pro              # an EverDrive build (experimental; see §6)
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

## 6. EverDrive builds (experimental)

**Neither has run on a cart.** They exist so that the first person with an EverDrive has something
to test. Nothing about them counts as support.

| Build | Driver | Wire | RAM, flat image |
|---|---|---|---|
| default | `sc64.c` | [l3-over-sc64.md](../spec/l3-over-sc64.md) | 23,664 B |
| `CART=ed64` | `ed64.c` | [l3-over-everdrive-x7.md](../spec/l3-over-everdrive-x7.md) §4: `DMA@` messages through the cart's 512-byte USB window | 26,976 B |
| `CART=ed64pro` | `ed64pro.c` | [l3-over-everdrive-pro.md](../spec/l3-over-everdrive-pro.md): the cart FIFO | 23,136 B |

Every build also links `cart_rom.c` and `pi_io.c`. Sizes are the object's code, data and BSS with
libdragon's GCC 16.2; a flat image adds only the few bytes of the host glue.

What differs from the SC64 build:

- **The driver is chosen when you build.** Nothing probes for the cart at run time, because probing
  one cart's registers on another is exactly the hazard found between libdragon and the PRO. Build
  for the cart the game will run on; an X7 build writes X7 registers. The default build compiles none
  of the EverDrive code. The SC64 build as it stands, with every bus wait bounded, was re-tested on
  an SC64 inside a game on 2026-09-15.
- **Frames are reassembled.** An SC64 packet carries a whole L3 frame. The EverDrive hosts send
  512-byte messages (X7) or 1024-byte FIFO writes (PRO), so these builds collect bytes across
  receives and ticks, and find frames by their magic, skipping a header whose type, channel or length
  the protocol does not allow. A host message that arrives while the agent's buffer has less room
  than it needs is not lost: the X7 driver keeps the part that does not fit, up to 512 bytes, and
  hands it over on the next receive, and the PRO leaves it in the cart FIFO. When the driver reports
  a lost piece (a failed read, a bad `DMA@` header or `CMPH` trailer, an X7 message more than 512
  bytes larger than the room left) the partial frame is dropped at once, so it
  cannot be completed with a later request's bytes.
- **A loss the driver cannot see is not recovered from cleanly.** The partial frame keeps the length
  its header claimed, and whatever arrives next is read as the rest of it. The 60-tick guard counts
  only ticks in which *nothing at all* arrives — any arrival resets it — so a host that retries
  inside that window has its retry spliced into the frame the loss broke. Once enough bytes have
  arrived to fill the claimed length, the agent answers that frame: under the **first** request's
  `rid`, carrying bytes that belong to the retry, and the retry is never seen as a request of its
  own. L3 frames carry no checksum, and the header check only ever looks at a header, so nothing
  catches this. A host that leaves 60 ticks of silence before retrying (a second at 60 Hz, two at
  30 Hz) finds a clean buffer; one that retries sooner can lose the retry the same way.
  `make host-test` runs this logic on a PC against a fake driver for both carts, and CI runs it on
  every change.
- **Nothing is staged in ROM space.** libdragon's X7 driver copies received packets to the top of the
  ROM window; this one reads straight out of the cart's USB window. The PRO needs no staging.
- **A reply holds the tick while it is sent.** The X7 driver waits for the cart after each 512-byte
  block, the PRO driver after each 1024-byte block. Every wait is bounded.
- **The X7 driver writes `SYSCFG = 0` at init**, as libdragon does. Whether that is harmless inside a
  commercial game is unverified.

The host side is `multi64d --cart ed64` or `--cart ed64pro`.
