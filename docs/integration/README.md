# ROM integration

> [Doc map](../README.md) · [M64P spec](../spec/memory-l3-application-v0.md) · [Agent source](../../n64/agent/README.md)

How to make a game running on a real N64 readable and writable by a PC program, the way
an emulator's scripting API makes it readable and writable today — so that tools written
against an emulator (randomizer clients, trackers, practice tools) can run against the
console instead.

These guides are **not normative**. The wire format is
[memory-l3-application-v0.md](../spec/memory-l3-application-v0.md); what follows is how to put
it to use, and what went wrong the first times it was done.

---

## The shape of an integration

```
 tool written against an emulator
        │   (its own memory calls, unmodified)
 emulator-API stand-in  ─────────────  host-connector.md
        │   M64P requests over WebSocket
 multi64d  ──USB──  SummerCart64
        │   L3 over the PI bus
 M64P agent, inside the game ROM  ───  cart-agent.md, placing-the-agent.md
        │
      RDRAM
```

Three parts, and multi64 supplies two of them:

| Part | Provided | Your work |
|---|---|---|
| **Agent** — serves M64P from inside the ROM | [`n64/agent/`](../../n64/agent/README.md) | Choose its RAM, a per-frame call site, and a ROM location; build and splice it in |
| **Bridge** — cart link over USB | [`multi64d`](../spec/daemon-api-v1.md) | Nothing |
| **Stand-in** — answers the tool's memory calls with M64P | — | Write it, following [host-connector.md](host-connector.md) |

The agent itself contains nothing about any game. Everything game-specific is **where** it
goes and **what calls it**, and that is almost all of the work.

---

## Before you start: three questions

1. **Can you build the game from source?** A decompilation that builds a matching ROM lets
   the agent be compiled in as its own segment, with the hook written in C. Without one,
   everything is placed by address in the binary. Both are proven; the second takes more
   care. See [placing-the-agent.md](placing-the-agent.md).
2. **Is the ROM you will run already modified?** Randomizers and mods ship their own code
   and data, and they tend to take exactly the spaces an agent would want: free ROM past
   the retail image, the Expansion Pak's first megabytes, dead functions. Measure against
   the **patched** ROM, never the retail one.
3. **How does the tool read memory?** If it goes through an emulator's scripting API
   (memory domains, frame advance, sockets), a stand-in can serve it unmodified. If it
   reads the emulator's process memory directly, there is no API to stand in for, and it
   cannot be served this way. Check this first; it decides whether the rest is worth doing.

---

## Checklist

| # | Step | Guide |
|---|---|---|
| 1 | Confirm the tool reaches memory through an API a stand-in can serve | [host-connector.md](host-connector.md) |
| 2 | Produce the exact ROM you will run (seed, patch, options) | [placing-the-agent.md §1](placing-the-agent.md#1-start-from-the-rom-you-will-actually-run) |
| 3 | Measure free RAM during varied play, then rule candidates out statically | [placing-the-agent.md §2](placing-the-agent.md#2-find-ram-for-the-agent) |
| 4 | Pick one per-frame call site, and a place for the code it will call | [placing-the-agent.md §3](placing-the-agent.md#3-find-a-per-frame-call-site) |
| 5 | Build the agent for that address, splice it in, fix the CRC | [cart-agent.md](cart-agent.md), [placing-the-agent.md §5](placing-the-agent.md#5-build-and-splice) |
| 6 | Verify in an emulator with and without the Expansion Pak | [testing.md](testing.md) |
| 7 | Verify on hardware: `HELLO_ACK`, a peek of known state, then a real session | [testing.md](testing.md) |
| 8 | Soak | [testing.md](testing.md#6-soak) |

---

## What has been proven

On a SummerCart64, the same agent source served three commercial games through randomizer
clients, with location checks reaching a server and items arriving in game:

| Host | How the agent went in | What it added to the evidence |
|---|---|---|
| A game with an existing assembly payload | Relocatable object appended to the payload; the payload's frame hook retargeted | The protocol and the PI rules, under a game that loads from ROM constantly |
| A decompiled game, then a randomizer seed built on it | A segment of the decomp's own build; the agent moved when the randomizer's mod took its RAM | Liftability; that a mod changes where things can go |
| A game with no buildable source | Flat image, a hook stub in dead code, one retargeted `jal` | That none of the above needs a decomp |

Two things none of them has exercised: a **4 MB** console with the agent in the base RAM (all
three put it in the Expansion Pak), and any cart other than the SummerCart64. EverDrive-64 X7 and
PRO builds of the agent exist ([cart-agent.md §6](cart-agent.md#6-everdrive-builds-experimental)),
but neither has run on a cart.

## Guides

| Guide | For |
|---|---|
| [cart-agent.md](cart-agent.md) | What the agent needs, what it does to the machine, how to build it |
| [placing-the-agent.md](placing-the-agent.md) | RAM, the hook, the ROM, the loader and the CRC — with and without a decomp |
| [host-connector.md](host-connector.md) | Writing the emulator-API stand-in on the PC |
| [testing.md](testing.md) | The order to verify things in, and the traps met doing it |
