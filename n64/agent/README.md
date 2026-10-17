# Game-resident M64P agent

> **Guides:** [ROM integration](../../docs/integration/README.md) · **Protocol:** [memory-l3-application-v0.md](../../docs/spec/memory-l3-application-v0.md) · **Test ROM:** [n64/README.md](../README.md)

The agent is the console side of an [M64P](../../docs/spec/memory-l3-application-v0.md) integration: a
few kilobytes of code that live inside a game ROM, are called once per frame, and serve
RDRAM peek and poke to a host over a SummerCart64 (or, experimentally, an EverDrive-64 X7 or PRO). The test ROM's **MEM_AGENT** mode serves
the same protocol; this is the form that goes into someone else's ROM.

It has **no undefined symbols**: it calls nothing from libultra, libdragon, the C library or
the game. What a game has to give it is one call per frame and about 24 KB of RAM. Where
those come from is the whole of an integration — see
[placing-the-agent.md](../../docs/integration/placing-the-agent.md).

## Files

| Path | What |
|---|---|
| `agent.c`, `agent.h` | `agent_tick()`, L3 framing, the four hooks `mem_proto` needs |
| `sc64.c`, `sc64.h` | SummerCart64 USB driver over the PI bus: staging, bounded waits, interrupt masking |
| `ed64.c`, `ed64.h` | EverDrive-64 X7 driver (`CART=ed64`): `DMA@` messages through the cart's USB window. **Experimental; no game has been played through it**: it has worked on an X7 in [`n64/bringup`](../bringup/README.md)'s ROM, and inside one game at its title screen, where it answered every request. That was before it kept its place in the stream across calls and ended its waits by the console's clock; as it now is, it has run only against the host tests |
| `ed64pro.c`, `ed64pro.h` | EverDrive-64 PRO driver (`CART=ed64pro`): the cart FIFO. **Never run on a cart** |
| `pi_io.c`, `pi_io.h` | PI access for the EverDrive drivers and `cart_rom.c`, under the same rules as `sc64.c` |
| `cart_rom.c` | The cartridge ROM for M64P's `PEEKROM`, the same on every cart |
| [`../test-rom/mem_proto.c`](../test-rom/mem_proto.c), `mem_proto.h`, `m64p_types.h` | The protocol handler and types header, **shared with the test ROM** — not copied here. Every agent build defines `M64P_POKE_ONCE`, which the test ROM does not: see [Repeated writes](#repeated-writes) |
| `Makefile` | One relocatable object, `build/m64p_agent.o`, for a build you control |
| `flags.mk` | The compiler flags, shared with [`../bringup`](../bringup/README.md), which builds these sources into its own ROM |
| `templates/flat.ld`, `templates/segment_magic.c` | Link the agent as a flat image at a fixed address, with a load marker |
| `templates/hook_stub.S`, `templates/link-flat.sh` | A per-frame hook stub and the script that builds image + stub, for a ROM with no buildable source |
| `tools/n64crc.py` | Check or fix the header CRC (CIC-6101, CIC-6102, CIC-6103) after changing code in the first MiB |
| `tools/ram-usage.lua` | BizHawk: map which of all 8 MB of RDRAM a game touches |
| `tools/watch-ranges.lua` | BizHawk: watch named RAM ranges byte for byte, for candidates too small for a page map |
| `tools/count-calls.lua` | BizHawk: how often each candidate hook site really runs, per frame |
| `tools/ram-bounds.py` | Find the RAM addresses a ROM computes, and flag any pair that brackets the agent |
| `tools/agent-probe.lua` | BizHawk: agent loaded, code intact, ticking, dormant or ready, errors |

## Build

```sh
cd n64/agent
make                           # libdragon's mips64-elf, as pinned in ../toolchain.lock
make PREFIX=mips64-ultra-elf-  # or a libultra toolchain
make symbols                   # sizes, exports, and undefined symbols (must be none)
make CART=ed64pro              # an EverDrive build instead: ed64 or ed64pro (experimental)
make CART=ed64 PI_IO=dma       # pi_io.c moves words by PI DMA: a diagnostic build, see below
make host-test                 # EverDrive L3 reassembly, SC64 driver failure handling, X7 driver receive and send, repeated writes, on the PC (CI runs this)
```

For a ROM with no source to build against:

```sh
templates/link-flat.sh AGENT_VRAM AGENT_ROM AGENT_MIN_RAM STUB_VRAM HOOK_ORIGINAL ROM_COPY
```

`CART=ed64` or `CART=ed64pro` in the environment selects an EverDrive driver there too. Both are described, with what each argument means and how to find it, in
[cart-agent.md](../../docs/integration/cart-agent.md) and
[placing-the-agent.md](../../docs/integration/placing-the-agent.md).

`PI_IO=dma` builds `pi_io.c` with `PI_IO_DMA`, into `build/<cart>-dma/` (`build/sc64-dma/` for the default cart). Its word copies, the X7
driver's USB window and every build's `PEEKROM`, then go by PI DMA through a bounce buffer, as
libdragon moves an X7's window, instead of by CPU load and store. It writes the PI's DMA registers,
which the agent otherwise never does ([cart-agent.md §3](../../docs/integration/cart-agent.md#3-what-it-does-to-the-machine)),
so it is for finding out whether a cart needs DMA, not for a game. [`../bringup`](../bringup/README.md)
runs both forms side by side. The default builds are byte-identical with or without this option
in the source.

## Repeated writes

Every build here, `make` and `templates/link-flat.sh` alike, compiles `mem_proto.c` with
`M64P_POKE_ONCE`. The agent then remembers the request it served last, if that was a `POKEV` it
applied, and answers a copy of it (same `rid`) with the same `POKE_ACK` and writes nothing; any
other request ends that. `HELLO_ACK` sets `flags` bit `3` to say so, which is what lets a host
send a write again after a timeout without risking it landing twice
([spec §4.4](../../docs/spec/memory-l3-application-v0.md#44-repeated-writes-flags-bit-3),
[cart-agent.md §2.2](../../docs/integration/cart-agent.md#22-a-write-sent-again)). It costs four
bytes of RAM. The test ROM and [`../bringup`](../bringup/README.md) build the same file without
it, so their images are unchanged and their `HELLO_ACK` leaves bit `3` clear. It has run only
against `make host-test`, never on a console.

## What has run on hardware

Every build below served M64P on a SummerCart64 inside a commercial game, with this source as it
stood before [repeated writes](#repeated-writes) (comments aside) and the shared `mem_proto`:

| Build | Toolchain | Where it ran |
|---|---|---|
| Relocatable object appended to an existing assembly payload | `mips64-ultra-elf-gcc` 15.2, the payload's own flags | a game with an existing randomizer payload |
| Compiled as a segment of a decompilation's own build | GCC 2.8.1, the decomp's flags | a decompiled game, and a randomizer seed built on it |
| `templates/link-flat.sh` image + `hook_stub.S` | `mips64-ultra-elf-gcc` 15.2 | a game with no buildable source |

`make` with libdragon's `mips64-elf-gcc` 16.2 builds and links with no undefined symbols, but
that build has **not** been run on hardware. Treat the first run of any new toolchain as a
test, and follow the [testing ladder](../../docs/integration/testing.md).

Only the **SummerCart64** driver has had a game played through it, and it is the default. `CART=ed64` (EverDrive-64 X7) and `CART=ed64pro` (EverDrive-64 PRO) build the same agent around an EverDrive driver instead, and **both are experimental**. The X7 driver has worked on an X7 outside a game, in [`n64/bringup`](../bringup/README.md)'s ROM, where under PI load it cut a reply off part-way; `ed64_send` now retries a busy PI once a message has started, and with that retry the same X7 answered every request but one across three load levels. Inside a game it has answered once, on that X7 on 2026-10-12: in an Ocarina of Time seed left at its title screen, every `HELLO` and every read, a 4096-byte one included ([hardware record](../bringup/README.md#hardware-record)). Later that day the tester reported an Archipelago client connecting through it, by a screenshot with no log; no item sent or received and no sustained play has been shown. An earlier X7 build, tried in an Ocarina of Time seed on 2026-10-07, opened the serial port and was never answered, for a reason not established. Since those runs the X7 driver's receive path has been changed to keep its place in the stream when a busy PI or the clock stops a call, where it used to report the message lost and read the rest of it as headers, and every wait in it now ends by the console's clock: at most about 0.15 s for a receive, 0.3 s for a send and 0.49 s for everything one `agent_tick` does in the driver ([`ed64.h`](ed64.h)). That driver has run only on a PC, against `make host-test`'s fake cart; no cart has run it. The PRO build has never run on a cart. The default build compiles none of that code; what it has gained since the table above is the handling of [repeated writes](#repeated-writes). Build for the cart the game will run on: the drivers do not detect each other. See [cart-agent.md §6](../../docs/integration/cart-agent.md#6-everdrive-builds-experimental).

## Copies elsewhere

A project that vendors these files should copy them, not edit them, and re-copy after any
change here — the same rule the protocol handler already has. `mem_proto` in particular is
the M64P implementation both the test ROM and every agent run; a fork of it is a fork of the
protocol.
