# Game-resident M64P agent

> **Guides:** [ROM integration](../../docs/integration/README.md) · **Protocol:** [memory-l3-application-v0.md](../../docs/spec/memory-l3-application-v0.md) · **Test ROM:** [n64/README.md](../README.md)

The agent is the console side of an [M64P](../../docs/spec/memory-l3-application-v0.md) integration: a
few kilobytes of code that live inside a game ROM, are called once per frame, and serve
RDRAM peek and poke to a host over a SummerCart64 (or, experimentally, an EverDrive-64 X7 or PRO). The test ROM's **MEM_AGENT** mode serves
the same protocol; this is the form that goes into someone else's ROM.

It has **no undefined symbols**: it calls nothing from libultra, libdragon, the C library or
the game. What a game has to give it is one call per frame and about 21 KB of RAM. Where
those come from is the whole of an integration — see
[placing-the-agent.md](../../docs/integration/placing-the-agent.md).

## Files

| Path | What |
|---|---|
| `agent.c`, `agent.h` | `agent_tick()`, L3 framing, the four hooks `mem_proto` needs |
| `sc64.c`, `sc64.h` | SummerCart64 USB driver over the PI bus: staging, bounded waits, interrupt masking |
| `ed64.c`, `ed64.h` | EverDrive-64 X7 driver (`CART=ed64`): `DMA@` messages through the cart's USB window. **Never run on a cart** |
| `ed64pro.c`, `ed64pro.h` | EverDrive-64 PRO driver (`CART=ed64pro`): the cart FIFO. **Never run on a cart** |
| `pi_io.c`, `pi_io.h` | PI access for the EverDrive drivers, under the same rules as `sc64.c` |
| [`../test-rom/mem_proto.c`](../test-rom/mem_proto.c), `mem_proto.h`, `m64p_types.h` | The protocol handler and types header, **shared with the test ROM** — not copied here |
| `Makefile` | One relocatable object, `build/m64p_agent.o`, for a build you control |
| `templates/flat.ld`, `templates/segment_magic.c` | Link the agent as a flat image at a fixed address, with a load marker |
| `templates/hook_stub.S`, `templates/link-flat.sh` | A per-frame hook stub and the script that builds image + stub, for a ROM with no buildable source |
| `tools/n64crc.py` | Check or fix the header CRC (CIC-6102, CIC-6103) after changing code in the first MiB |
| `tools/ram-usage.lua` | BizHawk: map which of all 8 MB of RDRAM a game touches |
| `tools/agent-probe.lua` | BizHawk: agent loaded, code intact, ticking, dormant or ready, errors |

## Build

```sh
cd n64/agent
make                           # libdragon's mips64-elf, as pinned in ../toolchain.lock
make PREFIX=mips64-ultra-elf-  # or a libultra toolchain
make symbols                   # sizes, exports, and undefined symbols (must be none)
make CART=ed64pro              # an EverDrive build instead: ed64 or ed64pro (experimental)
make host-test                 # the EverDrive builds' L3 reassembly, run on the PC (CI runs this)
```

For a ROM with no source to build against:

```sh
templates/link-flat.sh AGENT_VRAM AGENT_ROM AGENT_MIN_RAM STUB_VRAM HOOK_ORIGINAL ROM_COPY
```

`CART=ed64` or `CART=ed64pro` in the environment selects an EverDrive driver there too. Both are described, with what each argument means and how to find it, in
[cart-agent.md](../../docs/integration/cart-agent.md) and
[placing-the-agent.md](../../docs/integration/placing-the-agent.md).

## What has run on hardware

Every build below served M64P on a SummerCart64 inside a commercial game, with this source
(comments aside) and the shared `mem_proto`:

| Build | Toolchain | Where it ran |
|---|---|---|
| Relocatable object appended to an existing assembly payload | `mips64-ultra-elf-gcc` 15.2, the payload's own flags | a game with an existing randomizer payload |
| Compiled as a segment of a decompilation's own build | GCC 2.8.1, the decomp's flags | a decompiled game, and a randomizer seed built on it |
| `templates/link-flat.sh` image + `hook_stub.S` | `mips64-ultra-elf-gcc` 15.2 | a game with no buildable source |

`make` with libdragon's `mips64-elf-gcc` 16.2 builds and links with no undefined symbols, but
that build has **not** been run on hardware. Treat the first run of any new toolchain as a
test, and follow the [testing ladder](../../docs/integration/testing.md).

Only the **SummerCart64** driver has run on hardware, and it is the default. `CART=ed64` (EverDrive-64 X7) and `CART=ed64pro` (EverDrive-64 PRO) build the same agent around an EverDrive driver instead, and **neither has run on a cart**. The default build compiles none of that code and is byte-identical to the one in the table above. Build for the cart the game will run on: the drivers do not detect each other. See [cart-agent.md §6](../../docs/integration/cart-agent.md#6-everdrive-builds-experimental).

## Copies elsewhere

A project that vendors these files should copy them, not edit them, and re-copy after any
change here — the same rule the protocol handler already has. `mem_proto` in particular is
the M64P implementation both the test ROM and every agent run; a fork of it is a fork of the
protocol.
