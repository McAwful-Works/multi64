# The host side: an emulator-API stand-in

> [Integration guides](README.md) · [Daemon API](../spec/daemon-api-v1.md) · [M64P spec](../spec/memory-l3-application-v0.md)

Tools that read a game's memory are usually written against an emulator: a Lua connector
calling memory-read functions, advancing frames, and talking to the tool over a socket. The
cheapest way onto real hardware is to keep that connector **unmodified** and replace what is
underneath it: implement the emulator functions it calls on top of M64P, sent through
[`multi64d`](../spec/daemon-api-v1.md).

multi64 does not ship a stand-in. This guide is what one has to get right.

---

## 1. Can this tool be served at all?

Read the connector before anything else, and list every emulator function it calls. A stand-in
can serve:

- memory reads and writes against RDRAM, and reads of the ROM (from the cart with `PEEKROM`, or
  from the ROM file on the PC where an agent predates it);
- frame advance and frame counters, paced to real time;
- sockets, JSON, bit operations — anything that is really the scripting environment, not the
  emulator;
- messages to the player, as log lines.

It cannot serve, and a stand-in must **fail loudly** on:

- pausing or locking emulation — a console cannot be paused from the PC;
- memory callbacks (on read, write or execute) and breakpoints;
- savestates, input injection, framebuffer or screenshot reads.

A client that finds the emulator by scanning its process memory, instead of through a scripting
API, has nothing to stand in for.

## 2. Be the emulator, not correct

**The stand-in's job is to be indistinguishable from the emulator the connector was written
against — not to be right.** Where the two disagree, the connector was tested against the
emulator, and the emulator wins.

The example that established this: a connector read a 32-bit value and tested bits 32–35 of it.
In the emulator's bit library, shift counts on 32-bit values are masked to five bits, so "bit 32"
was bit 0 — the right flag, by accident. The stand-in's Lua shifted honestly, returned 0, and a set
of locations never registered. The fix was to mask shift counts like the emulator, with a
regression test that loads the real file.

## 3. Batch everything

The link is fast; round trips are not. An M64P exchange costs 67–100 ms on hardware, set by the
game's frame loop, almost independent of size ([spec §4](../spec/memory-l3-application-v0.md#4-addressing-and-limits)).

A connector that makes hundreds of small reads per poll cannot be served one read per exchange.
Collapse them:

- **A page cache** for connectors that read scattered bytes: fetch whole pages in as few `PEEKV`
  requests as the limits allow (32 regions, 7936 bytes each), then answer the individual reads from
  the cache. One connector's 400+ reads per poll became about 14 block reads.
- **A per-batch prefetch** for connectors that send a list of requests at once: fetch every RDRAM
  region the batch names in one exchange before evaluating any of it.

## 4. One request batch is one instant

This is the rule that matters most for anything that **writes**.

Randomizer clients typically deliver an item with a *guarded write*: write the item only if a
mailbox is empty and a received-count is unchanged. On an emulator the check and the write happen in
the same frame. On a console they are separate round trips unless the stand-in makes them one.

If the guard's reads come from different exchanges, the game can move between them, the guard passes
on stale values, and the item is delivered twice. That happened. So:

- read every region a batch's guards and reads name in **one** `PEEKV`, before evaluating anything;
- answer the batch's reads from that snapshot, overlaying the batch's own writes onto it;
- send the writes in one `POKEV` before replying, so the next batch sees them.

M64P makes each request atomic against the running game
([spec §4.1](../spec/memory-l3-application-v0.md#41-consistency)); the stand-in has to make each
*batch* one request.

## 5. The ROM comes from the cart

Clients read the ROM too: to recognize the game, to find seed data and a login key, to detect a ROM
swap by its hash. An agent whose `HELLO_ACK` sets `flags` bit 1 reads it from the cart with `PEEKROM`
([spec §4.2](../spec/memory-l3-application-v0.md#42-cartridge-rom-peekrom)), so the ROM the host
serves is the one the console is running, and the player has no file to choose.

- **Cache it.** A `PEEKROM` costs a round trip like any request, and one client read the ROM on
  every pass, 500 times in two minutes. The ROM cannot change while the console runs, so read each
  part once and serve it from memory after that.
- **Drop the cache when the link restarts.** A silent agent and a new `HELLO` may mean a reset or
  another game; read the header again and start over.
- **Batch it.** Gather the ROM regions a batch names, missing from the cache, into one `PEEKROM`,
  as §4 does for RDRAM.
- **Report a hash that is stable.** Hashing the whole image would mean reading all of it over the
  link. If the client only compares the hash with the value it saw first, a hash of the header is
  enough: it covers the boot checksum, which covers the first megabyte of code. If the client
  compares against a known value, this cannot be served, and the tool needs the ROM file instead.
- **`rom_bytes` is not the image size** (spec §4.2). A ROM domain size, if a client asks for one,
  has to come from somewhere else, or be reported as the window.

This has run on a SummerCart64: a game-resident agent advertised the window, served the header
and the client's login data from the cart, and a client session ran on nothing but that.

An agent that predates `PEEKROM` answers it with `ERR`. For those, serve the ROM from the exact file
the console booted, and log its internal name and hash at startup, where a mismatch is visible.

## 6. Timing

- **Pace frame advance to real time** (16.67 ms per frame). Connectors poll on frame counters, so
  pacing is what sets their poll rate.
- **Reclaim time a poll spent blocked**, from the idle frames that follow. Letting the cycle stretch
  instead lowered one connector's poll rate from 2 Hz to 1.4 Hz, and a lower rate misses short-lived
  state (§8). Write off only a stall longer than a whole cycle.

## 7. Survive everything the connector does not expect

On an emulator the memory never disappears. On hardware the USB link drops, the daemon restarts, the
console resets, and the game stops calling the agent during loads. Each needed its own handling:

| Event | What worked |
|---|---|
| Transport drops (daemon restart, setting change, USB hiccup) | Rebuild the transport and retry. `PEEKV` is a read and a repeated `POKEV` writes the same bytes, so retrying is safe. Give up only after minutes, loudly |
| Game stops calling the agent for longer than the reply timeout (loads) | Retry on the **same** transport before declaring it dead, and discard a late reply by its `rid`. Count these separately as stalls; reconnecting here also knocked the client off |
| Console reset | The agent is back as soon as the game reaches its hook; the stand-in only has to keep retrying and resync its stream decoder |
| Client abandons its socket without draining it | Put a deadline on every send. An unbounded write once blocked the only thread; the client still reported "connected", because the kernel completes a handshake nobody accepts |
| Connector exits after one missed `accept()` | Supervise it and restart it, but only on a clean return — a raised error must still propagate |
| Many abandoned connections queued during an outage | Accept the newest and discard the rest |
| Host started before the console | Wait for the cart instead of exiting, reporting progress, so the order of starting things does not matter |

Most connectors report **absolute** state on every poll, so nothing is lost across an outage: the
answer is re-derived from RAM when the link returns.

## 8. Transient state

An emulator reads memory every frame; a stand-in samples it a few times a second. State that exists
for less than a poll interval — a single "most recent event" slot that the next event overwrites — can
be missed entirely.

One connector relied on exactly such a slot for events in the current room; at hardware poll rates,
events changed it twice within a single read. What fixed it without touching the connector:

- sample that one small region much more often than the connector polls, out of the pacer's idle time;
- queue each value actually observed, and hand the connector the oldest one it has not seen when it
  reads the slot.

That is the one place a stand-in may knowingly return something other than current memory, and only
because every value it returns really happened, in order — the connector sees the sequence the
emulator would have seen, slightly later. Anything overwritten between samples is still lost; closing
that gap means the cart recording events, not the host sampling faster. Document such an exception
where the next reader will find it.

## 9. Before touching hardware

Build the stand-in so its memory backend can be a **RAM dump file** as well as the cart. Run the real,
unmodified connector against a dump captured from the emulator, over the real socket, and check what
the client receives. That exercises everything above except timing without a console, a cart or a
server — and is where most mistakes here were caught.
