# The host side: an emulator-API stand-in

> [Integration guides](README.md) · [Daemon API](../spec/daemon-api-v1.md) · [M64P spec](../spec/memory-l3-application-v0.md)

Tools that read a game's memory are usually written against an emulator: a Lua connector
calling memory-read functions, advancing frames, and talking to the tool over a socket. The
cheapest way onto real hardware is to keep that connector **unmodified** and replace what is
underneath it: implement the emulator functions it calls on top of M64P, sent through
[`multi64d`](../spec/daemon-api-v1.md).

multi64's stand-in is AP64's [`ap64-connector`](../../crates/ap64-connector), which serves
Archipelago's clients. This guide is what any stand-in has to get right.

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
API, has nothing to stand in for, unless it can also reach an emulator over the network. Check
for that before ruling it out. EmuLoader, which some Archipelago clients read emulator memory
through, falls back to RetroArch's Network Commands over UDP when it finds no emulator process,
and a stand-in can answer those. §10 covers what changes.

And a script that does no game logic at all, only moving packets between the client and buffers
in the game's RAM, needs no stand-in: the stand-in's job is the script's, done from the cart. §11
covers that.

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

The reads and the writes are still two requests, though, and the agent serves one a frame, so at
least a frame passes between the guard's snapshot and its write (seconds, if the write has to wait
out a stall). A guard on something only the client fills and only the game empties, such as a
mailbox or a received-count, still holds across that gap: the game can only move it the way the
guard already allows for. A guard on other game state, such as the scene or a mode flag, can pass
on the snapshot and be false by the time the write lands. M64P has no conditional write, so such a
guard narrows the window rather than closing it.

Clients also write a structure field by field: GoldenEye 007's rewrites Bond's whole inventory as
five 4-byte regions a slot, far past one request's 32 regions. Sent as several requests, those
writes land over several frames, and the game runs on a half-written structure; with a big enough
loadout GoldenEye crashed. So merge touching and overlapping writes into contiguous runs, the last
write to a byte winning and no gap filled, and send them as one request when they fit. Merged runs
are in address order, which across two requests could put a flag ahead of the data it announces,
so use them only when the batch fits one request; a larger one goes out in the client's order.

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
| Transport drops (daemon restart, setting change, USB hiccup) | Rebuild the transport and retry reads, which are safe to repeat. Do not resend a `POKEV` on the new transport, even to an agent that recognizes a copy: its `HELLO` ends what the agent remembers, and the console may have been reset in between, so the write was decided against the one before. Fail it and let the caller read again. Give up only after minutes, loudly |
| Game stops calling the agent for longer than the reply timeout (loads) | Retry on the **same** transport before declaring it dead, and discard a late reply by its `rid`. Count these separately as stalls; reconnecting here also knocked the client off. A `POKEV` the host gave up on is still served when the game reaches its hook, and a copy lands on the frame after. Writing the same bytes twice is not harmless, since a mailbox the game emptied between the two is filled again. So send a `POKEV` again only to an agent whose `HELLO_ACK` sets `flags` bit `3`, over a link that brings each request whole (a SummerCart64's; over an EverDrive a copy can be joined to a request a lost piece cut short, [cart-agent.md §2.2](cart-agent.md#22-a-write-sent-again)), under the same `rid` and before any other request; that agent acknowledges a copy of the write it served last without applying it ([spec §4.4](../spec/memory-l3-application-v0.md#44-repeated-writes-flags-bit-3)), so a write it never received lands and one it did is not repeated. Any other agent, including every one built before bit `3` existed, is waited on again for the first copy's reply and never sent the write twice, so a write it never received fails |
| Agent answers `ERR` | It is there and serving, so this is not a dead link. `E_BUSY` from `PEEKROM` means the game held the PI bus: wait a frame and ask again on the same transport, for as long as a silence would be waited out. Any other code is the answer to that request, and a copy, here or on a new transport, gets the same one: fail the request. Reconnecting for either costs a `HELLO`, and the client with it, and changes nothing |
| A reply that never comes, from an agent answering every frame | On a SummerCart64 in Ocarina of Time the agent handled every request, and now and then its reply never reached the PC, between 0.1% and 2% of requests depending on the area (measured 2026-10-18 with the agent's own counters and a raw serial trace). Waiting out the whole reply timeout for each cost 1.2 s, and three in a row a reconnect. Once about eight round trips have been timed, when the agent had answered within a second before the request went out and the reply has not come in about four round trips (150–600 ms), send the request again under the **same** `rid` and take whichever copy is answered first. Only where a copy is safe: a read, or a `POKEV` to an agent that recognizes one (`HELLO_ACK` bit `3`), and only over a link that brings each request whole. Never a `WATCH`, which would reset the agent's history. One early copy per request, and none for the retries after a timeout, so a scene load costs one extra request, not one per retry |
| A reply arrives too late for its request | Discard it by its `rid`, but keep its watch trailer first ([spec §4.3](../spec/memory-l3-application-v0.md#43-watched-slots-watch)): the agent hands each event out once, on whichever response goes next, and one thrown away with its response is lost. The same goes for a response rejected for holding the wrong regions |
| Late replies reach a transport just rebuilt | multi64d gives every WebSocket client the whole stream from the cart, so the new transport hears the old one's late replies before its own `HELLO_ACK`. Wait for the `HELLO_ACK` itself |
| The reply timeout | Make it a deadline on the whole wait, each socket read waiting only for what is left of it. A read given the whole timeout each time can come back a moment early (Windows rounds socket timeouts to its timer tick), find the deadline not quite passed, and wait the whole timeout again, so one attempt took twice as long and the retries outlasted the client's own deadline |
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

## 10. Serving a network protocol instead of a script

Some clients carry their game logic in their own code and read emulator memory themselves. When
the library they read it through can also talk to an emulator over the network, a stand-in can
answer that protocol directly, with no connector script at all. AP64 does this for RetroArch's
Network Commands, the fallback EmuLoader uses when no emulator is running
([`retroarch.rs`](../../crates/ap64-connector/src/retroarch.rs)): `READ_CORE_MEMORY` and
`WRITE_CORE_MEMORY`, four bytes at a time at a KSEG1 address, over UDP.

Most of the sections above still apply. What changes:

- **Never answer late.** The protocol carries no request id, so a reply that arrives after the
  client gave up is taken as the answer to its *next* request, and every exchange after that is
  off by one. On a console that showed as garbage pointers and a false "new ROM" at every scene
  change, because the agent stops answering while the game loads. So the cart is not asked
  anything on the request path: reads are served from a snapshot of every window the client has
  used, refreshed continuously in batched `PEEKV`s, and writes are acknowledged at once and queued
  in order. The one wait is for a window never asked for before, bounded well inside the client's
  timeout (EmuLoader gives up after half a second); past it, answer with an error, never late.
- **Write only what changed.** The client writes a whole word even to change one byte, so
  replaying the word would put back whatever the game changed in the other three. Write the bytes
  the client changed; in a flag field the client read-modify-writes a bit at a time, read the live
  byte first and apply only the bits it changed.
- **Run the real client against the stand-in early.** A fallback path is the one its authors
  exercise least. Both clients AP64 serves this way needed a fix to their own installed files before
  they could reach it at all: one called a method its RetroArch backend lacked, the other read an
  attribute that backend does not have. Neither showed up from reading the code.

## 11. Relaying a ROM that speaks for itself

Some worlds compile all of the game's work into the ROM, and give it a protocol of its own. Their
emulator script then does nothing but move packets: read what the ROM left in a buffer and send it
to the client, write what the client sent into another buffer. There is nothing to stand in for,
and nothing to fork: AP64 does the script's job from the cart ([`relay.rs`](../../crates/ap64-connector/src/relay.rs)).
Star Fox 64's world is the one it was written for. Its client listens on a TCP port and the script
connects to it; the ROM keeps the addresses of its two buffers at a fixed place in RAM, and each
buffer holds one framed packet, whose `cmd` word the reader sets back to zero when it has taken it.

What matters here:

- **Read the ROM's side, not only the script's.** The script says how packets move; the ROM's
  source says how large a buffer is. Star Fox 64's script writes whatever arrives into the input
  buffer, and its client would let through 4 bytes more than the buffer holds, into the output
  buffer beside it. The relay refuses a packet larger than the buffer and drops the client instead.
- **A packet is written whole, in one request.** The agent applies a request at the game's own
  hook, between frames, so a packet written with its `cmd` in one `POKEV` is whole when the ROM
  looks. Write the body and the `cmd` separately and the ROM can read a `cmd` whose body is not
  there yet.
- **Take the next packet only when the last one is gone.** The ROM writes its output buffer only
  when `cmd` is zero, and reads its input buffer only when `cmd` is not: one packet each way at a
  time, which the relay keeps to by checking both `cmd` words in the same read as the packet.
- **Dial the way the script does.** The script connects once the ROM has something to say, which
  is its handshake, and tries again every few seconds while nobody listens. Connecting before the
  ROM is ready gives the client a silent peer, and its ping timer drops it.
- **Check what the ROM does on a console, not only in an emulator.** Star Fox 64's ROM picks its
  transport at boot: in an emulator the script's buffers, on a console an EverDrive's USB
  registers as well. On a console it reads a packet from those registers whenever they say one is
  waiting, and sends its own and clears it whenever they say they are ready. On a SummerCart64,
  which has no such registers, the connection dropped every few seconds with the ROM restarting
  its handshake, as it does when it reads a command it does not know or its ping goes unanswered.
  So the profile tells the ROM it is in an emulator (two instructions in its startup code, a
  `replace` write), and one connection then held for a whole session. A ROM that picks its
  transport by what it detects has to be put on the one the relay speaks.
- **Pace passes by the agent, not the socket.** The agent answers one request per game frame,
  and Star Fox 64's ROM drops a handshake that takes more than a second. A pass that made four
  requests (the pointers, the buffers, the clear, the write) lost every handshake; one read of
  everything and at most one write, with the client's reply to a packet riding in the write that
  clears it, connects in about 130 ms.

## 12. Standing in for an emulator's memory-bridge script

Some clients talk to a small script of their own inside an emulator other than BizHawk, which
does no game logic and only reads and writes memory for them. Pokémon Snap's world loads one
into Project64's debugger: one request per line, `<id>:<command>`, answered `<id>:<payload>`,
for `read`, `write`, `dict` and `romInfo`. Where the world's ROM does all of the game's work,
that script is the whole of what a stand-in replaces, and AP64 answers it as a connector script
([`connectors/pj64`](../../crates/ap64-connector/connectors/pj64/connector.lua)), each request
one read or one write to the cart.

What matters here:

- **Find the port where the client finds it.** Pokémon Snap's client picks its port at random on
  its first run and keeps it in Project64's own settings file, and reads it back from there each
  time ([`pj64.rs`](../../crates/ap64-connector/src/pj64.rs)). The stand-in reads the same file
  and listens there; it never writes it, since the client rewrites it on every start.
- **Parse what the client sends, not what the script's own code suggests.** The client builds
  addresses with Python's `hex()`, `0x` and all, which JavaScript's `parseInt(s, 16)` accepts
  and Lua's `tonumber(s, 16)` does not. Test against the client's exact lines.
- **Answer errors the way the script does.** The client raises a payload that begins `Invalid`
  or `Unknown` as a script error, and checks each reply's id against its request's, so a reply
  is never taken for another one's. It waits 5 seconds for each; a cart answers in tens of
  milliseconds.
