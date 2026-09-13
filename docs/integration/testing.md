# Testing an integration

> [Integration guides](README.md) · [Placing the agent](placing-the-agent.md) · [The host side](host-connector.md)

Test in this order. Each step is cheap compared with the next, and each catches failures the
next one would show only as "the console froze" or "nothing happened".

---

## 1. Static checks

Before running anything:

- the agent links with **no undefined symbols** (`make symbols`, or `nm -u`);
- the hook stub, or the hook function with its padding, is **exactly** the size it replaces;
- the built or spliced ROM differs from the ROM it came from **only** in the ranges you meant to
  change — list them;
- the header CRC verifies: [`tools/n64crc.py`](../../n64/agent/tools/n64crc.py);
- a rebuild from committed files reproduces the ROM byte for byte.

## 2. Emulator, Expansion Pak on

Run the ROM in an emulator with
[`tools/agent-probe.lua`](../../n64/agent/tools/agent-probe.lua), settings filled from your build,
and play for several minutes of varied content. Expect:

| Field | Expect | If not |
|---|---|---|
| `osMemSize` | `0x800000` | The Expansion Pak is not really enabled (§7) |
| `magic` | `0x4D363450` | The loader never ran, or copied from the wrong place |
| `text` | `intact` for the whole session | Something writes into the agent's RAM: move it |
| `ticks` | rising steadily, at the rate of your call site | The site is not per-frame, or not reached |
| `init_attempts` | `16`, then unchanged | No cart in an emulator: the agent must go dormant and stay dormant |
| `errors` | `0` | |

And the game must play normally. **This step is not optional.** The agent's dormancy guarantee
was wrong for the life of one project because it had only ever run where a cart existed; an
emulator found it.

## 3. Emulator, 4 MB

Run the same ROM with the Expansion Pak off. `osMemSize` must read `0x400000`, the load marker
must stay absent, and the game must run normally. That proves the memory guard, which is what
makes the ROM safe on a console without an Expansion Pak.

## 4. Hardware: the agent

1. **Power the console fully off**, then write the ROM to the SD card and verify it byte for byte
   ([`sc64-sd-e2e`](../../crates/sc64-sd-e2e) `--verify`). Only one process can hold the cart's serial
   port: release `multi64d`'s link first and resume it after
   ([daemon-api-v1.md](../spec/daemon-api-v1.md)).
2. Boot it. A ROM that does not boot at all has usually failed the CRC or IPL3 check
   ([placing-the-agent.md §6](placing-the-agent.md#6-the-boot-crc-and-ipl3)).
3. Get into the game, then send `HELLO`. Expect `HELLO_ACK` with the RDRAM size (8192 KiB with the
   Expansion Pak) and writes accepted.
4. `PEEKV` something you can change by playing — health, a counter — and watch it change. That proves
   the bytes are the game's, not staging garbage.

## 5. Hardware: a real session

With the host side running against the cart:

- the tool connects through the stand-in and recognises the game;
- a **check made in game reaches the server**;
- an **item sent from the server arrives in game, once** — count it;
- the stand-in reports no stalls or reconnects during normal play, and recovers from each when you
  cause one deliberately: restart the daemon, reset the console, disconnect the tool.

## 6. Soak

Hours, not minutes. The failure modes that remain after the steps above — PI contention during rare
long loads, a missed transient, a leak in the host — only show up with time. Keep the stand-in's
heartbeat log.

---

## 7. Traps

| Trap | What happened | Do instead |
|---|---|---|
| Emulator memory settings | BizHawk's N64 core reads the Expansion Slot setting only when the core is created: toggling it needs *Reboot Core*, not a reset, and it is saved on exit. One version reported an 8 MB RDRAM domain with the slot disabled | Read `osMemSize` (u32 at `0x318`) in every probe; never trust the setting or the domain size |
| RAM maps from an emulator | Regions that looked untouched were RSP-written buffers, heap tails and rarely used overlays | Treat a map as a candidate list; rule each out statically ([placing-the-agent.md §2.2](placing-the-agent.md#22-then-rule-candidates-out)) |
| Measuring at the title screen | The largest free region vanished within minutes of play | Play varied content before reading the map |
| A plausible explanation | A crash was blamed on 4 MB because the numbers fit; the same crash at 8 MB refuted it | Measure the thing you are blaming before recording it as the cause |
| Hook padding | A loader change grew the hook by 16 bytes and silently moved every later function | Re-measure the padded size after every change to the hook |
| Build-tool padding | A reserved ROM gap was accepted by the build tool and emitted nothing | Check the map output for the address you expected, every time |
| A modified IPL3 | A patch NOPed the checksum check; emulators never notice | Restore retail IPL3 and fix the CRC ([placing-the-agent.md §6](placing-the-agent.md#6-the-boot-crc-and-ipl3)) |
| The wrong serial port | A daemon started on a different USB-serial adapter and still reported itself healthy | Check `GET /` on `multi64d` names the cart's port before a session |
| Line endings | Build files copied from a Windows checkout into a Linux build tree carried CRLF | Normalise to LF when copying between checkouts |
| A stalled reply looks like a dead link | The game stops calling the agent during loads, longer than the reply timeout | Distinguish stalls from transport failures ([host-connector.md §7](host-connector.md#7-survive-everything-the-connector-does-not-expect)) |
