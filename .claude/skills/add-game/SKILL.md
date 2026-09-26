---
name: add-game
description: Add a new game to AP64 - checking its Archipelago world is one the generic connector can drive, building a seed to work against, finding RAM and a per-frame hook site by measurement, writing the profile, and verifying it before it reaches a console. Use when asked to add, integrate, support or evaluate a game for AP64, or to build an Archipelago seed for one.
---

# Adding a game to AP64

Seven games are in: Castlevania 64, Paper Mario, Ocarina of Time, Kirby 64, Banjo-Tooie,
Mario Kart 64 and Donkey Kong 64, with Legacy of Darkness written and held back in
`ap64_core::withheld()`.
Each took a day or two, and most of that was spent on things this file now answers.

[`docs/integration/placing-the-agent.md`](../../../docs/integration/placing-the-agent.md)
is normative for the ROM side and is not repeated here. This is the spine around it,
including the Archipelago half, which no document covers.

Work the sections in order. §1 and §3 are both gates that end the job early when they
fail, and they are cheap on purpose: everything expensive is behind them.

## 1. Can the generic connector drive it?

Check before anything else. A game whose world needs its own client is a different and
much larger job: AP64 would need a connector of its own, like `connectors/oot/`.

Open the apworld and look for a client that subclasses Archipelago's BizHawk client:

```sh
python -c "
import zipfile; z = zipfile.ZipFile(r'C:\ProgramData\Archipelago\custom_worlds\GAME.apworld')
print([n for n in z.namelist() if n.endswith('client.py')])
print([l for l in z.read('GAME/client.py').decode('utf8','replace').split(chr(10))[:40] if 'import' in l])
"
```

`from worlds._bizhawk.client import BizHawkClient` means **generic**: stock Archipelago's
BizHawk Client runs the world's own logic, and AP64 needs no connector work. Paper Mario,
CV64, CVLoD, Kirby 64 and Mario Kart 64 all look like this.

No such import, or a world that ships its own client (Ocarina of Time's OoT Client), means a
**forked** connector. Say so and stop; that is a separate piece of work, not a profile.

A client with no Lua at all -- one that reads emulator memory itself -- may still be reachable.
Donkey Kong 64's and Banjo-Tooie's clients do that through EmuLoader, which falls back to
RetroArch's Network Commands over UDP when no emulator is running, and AP64 answers those
natively (`ap64-connector`'s `retroarch` module). Look for that path before forking anything.
Banjo-Tooie was first played through a fork of a connector script its released world had
already stopped shipping, and the fork broke on every release until it was replaced by the
path the client already had (#319). Find out which emulator protocols the client can
speak before calling it out of reach, and run the real client against a stand-in on that
protocol early. That is how the missing `read_bytestring` in DK64's copy of EmuLoader turned
up, which no reading of the code had caught.

Diddy Kong Racing was dropped at exactly this step, after it was assumed from the game
rather than checked in the apworld.

The import tells you *whether* a connector is forked, not what forking it would cost, and
it misses a world that imports the generic client but ships a lua doing work of its own.
Both are the same failure: work the connector performs on a frame callback never reaches
the wire, so an AP64 stand-in replaying the client's requests never performs it. That is
what left Banjo-Tooie's ROM uninitialized for a week, presenting as a freeze.

[`ap64-connector/tools/connector-trace.lua`](../../../crates/ap64-connector/tools/connector-trace.lua)
measures it.
Load it in BizHawk with `CONNECTOR_TRACE_TARGET` set to the world's lua, play, and read the
report: accesses tagged `client` are replayable and free, and every other row is work a
stand-in would have to reimplement, priced in coalesced regions -- one region is one round
trip, about 67 ms. Compare that against a 16.7 ms frame before agreeing to a fork.

GoldenEye 007 was parked at this step. Its lua is a fork of the generic connector that also
walks an inventory linked list and rewrites objects on `onframestart`, and its ROM patch
removes the native paths that did that work, so none of it is optional.

## 2. Build a seed to work against

Everything downstream needs a real patched ROM, not the retail one: the randomizer's own
payload is usually the agent's nearest neighbor in RAM.

Generate without disturbing the user's own YAMLs, by pointing the generator at a scratch
directory:

```sh
"C:/ProgramData/Archipelago/ArchipelagoGenerate.exe" \
  --player_files_path <scratch>/players --outputpath <scratch>/out --seed 1
```

The output `.zip` holds a `.apXX` patch. Its `archipelago.json` names the procedure that
turns it into a ROM:

```sh
python -c "
import zipfile,json; z=zipfile.ZipFile(r'<patch>.apXX')
print(z.namelist()); print(json.dumps(json.loads(z.read('archipelago.json')),indent=2))
"
```

`apply_bsdiff4` + `apply_tokens` is a shape you can apply yourself (BSDIFF40 is three bzip2
blocks after a 32-byte header, and Python has `bz2`). Custom procedures like CV64's
`apply_patches`/`patch_ap_graphics` live in the apworld's Python — do not reimplement
those, run `ArchipelagoLauncher.exe <patch>` and let it write the `.z64`.

Fix the header CRC with [`n64/agent/tools/n64crc.py`](../../../n64/agent/tools/n64crc.py)
rather than your own; only CIC-6102 and 6103 are implemented, and it refuses rather than
guessing.

**Two static reads worth taking before any emulator.** They settled Paper Mario's RAM
margin in minutes when a play session had been the plan:

- A bsdiff4 header's third field is the **exact output size**, so the mod's ROM extent is a
  constant of the apworld version, not something a seed's options move.
- Parsing the token binary (`u32 count`, then `type:u8, offset:u32, len:u32[, bytes]`)
  gives every address the seed writes. Paper Mario's highest is `0x1D09ED0`, 14 MB below
  the agent, and min-options and max-options seeds differ by 32 bytes.

Generate a **maximal** seed — every check-count and hint option at its limit — when the
question is how big the payload can get.

**Check whether the ROM is seed-independent at all.** Diddy Kong Racing's generator emits
no patch file: the output holds only `.archipelago` and a spoiler, and its `RomPatcher.py`
applies one static bsdiff to vanilla and asserts a *fixed* output md5, with every choice
made at runtime over the socket. One ROM serves every seed, so the profile's pins are
measured once and can never drift per seed. The tell is a `patched_rom_md5` constant in
the world, or a generator output with no `.apXX` in it.

## 3. Boot the unmodified seed on the console, before measuring anything

**This gate exists because Diddy Kong Racing cost a day without it.** Its profile was
measured to completion -- RAM placement with Archipelago live, a hook site matched to a
control, 764 bytes of stub space proven dead by counting -- and its connector was forked
and tested. Then the seed turned out not to boot on a console at all. Vanilla DKR boots
from the same card and the same menu; every Archipelago ROM black-screens.

So before §4, put the seed on the cart and turn the console on. Nothing here is AP64's
code, and that is the point: what is being tested is whether the randomizer's own output
runs on real hardware.

```sh
cargo build -p sc64-sd-e2e --release
MSYS_NO_PATHCONV=1 target/release/sc64-sd-e2e --port COM4 --upload <seed>.z64 --to /
MSYS_NO_PATHCONV=1 target/release/sc64-sd-e2e --port COM4 --verify /<seed>.z64 --against <seed>.z64
```

The console must be powered off for that write. `--verify` reads the bytes back off the
card, so a bad upload cannot be mistaken for a bad ROM.

**If it does not boot, carry the retail ROM up as a control before concluding anything.**
It separates "this randomizer does not run on hardware" from "this cart, card or console
is unwell today", and they look identical from the sofa. Then work down this list, which
is the order they were eliminated for DKR:

| suspect | how to rule it out |
|---|---|
| header checksum | `n64/agent/tools/n64crc.py <rom>` -- a separate implementation from the patcher's |
| ROM size | Paper Mario's agent sits at 45 MB and OoT's at 56 MB on this cart, so size alone is not it |
| length alignment | pad the image to a 512-byte boundary past `0x101000` and retry; the CRC is unaffected |
| boot chip | compare the IPL3 sha1 against the retail ROM's -- if a patch left it alone, the CIC is the retail one |
| Expansion Pak | boot a **retail** game that requires one -- Banjo-Tooie, Donkey Kong 64, Majora's Mask. Takes a minute and answers it outright |

**DKR's cause is open, and the Expansion Pak is not it.** It was the leading suspect on
good evidence: the randomizer puts its data block at `0x80400000`, the first 24 KB of the
Pak's region, and upstream's connector dereferences a pointer at `0x400000` to reach it,
so without a Pak the first frame would write into nothing. It fit every observation. Then
retail Banjo-Tooie, which requires the Pak, ran on the same console from the same card --
so the Pak works, and the theory is dead. Whatever stops DKR is still unidentified.

Two things worth taking from that. Booting a Pak-requiring retail ROM is the cheapest
possible test of the whole question and should come before any reasoning about RAM; and a
hypothesis that fits every observation is still only a hypothesis, which is why the row
above no longer calls it the usual answer. An emulator always has 8 MB, so the class of
problem is real and invisible until a console sees it -- it just was not this.

**A randomizer that needs the Expansion Pak for the game is a different proposition from
one where only the agent does.** `AGENT_MIN_RAM` makes the agent skip itself below 8 MB
and the seed still plays. When the seed itself needs the Pak, that is a hardware
requirement for the game, and it belongs in the issue and the profile comment as the
headline rather than a remark about the agent.

Record the result on the game's issue either way. A "boots on a console" line is worth
more than anything else on it, and a game that does not boot comes off the candidate list
rather than waiting to be rediscovered.

**Batch the gate.** Five candidates were taken from nothing to a boot verdict in one
sitting, because the expensive parts amortize: one download pass, one `ArchipelagoGenerate`
per world with Archipelago's own option templates, one SD write session, one boot session.
All five passed -- the three Bombermans, Mario Kart 64 and Banjo-Tooie boot and play on a
console -- and every base ROM matched the md5 its world demanded, which is the other thing
worth checking before generating anything. DKR wanted a ROM revision nobody had for two
sessions; that comparison costs seconds.

One useful shortcut fell out of it. `apply_bsdiff4` and `apply_tokens` are procedures
Archipelago's own launcher will run for you -- `ArchipelagoLauncher.exe <patch>` writes the
`.z64` and then fails to find an emulator, which is fine, because the ROM is already
written. A world that ships a static patch and no generator output (Banjo-Tooie, DKR) needs
bspatch applied by hand instead, and those worlds state a fixed output md5, so the result
checks itself.

## 4. Find RAM, by measuring

Three tools, three different questions. Use them in this order; see §2 of the guide.

| tool | question |
|---|---|
| [`ram-usage.lua`](../../../n64/agent/tools/ram-usage.lua) | which 4 KB pages does the game touch? |
| [`watch-ranges.lua`](../../../n64/agent/tools/watch-ranges.lua) | do *these exact bytes* ever change? |
| [`ram-bounds.py`](../../../n64/agent/tools/ram-bounds.py) | what addresses **bracket** the region? |

**If the game will not run properly in BizHawk, measure in an emulator its client supports.**
Donkey Kong 64's client refuses stock BizHawk, which gives the game a 4 KB EEPROM where it
needs 16 KB, so its RAM, heap and call counts were measured in Project64 3.0.1 with these tools
ported to its JavaScript API (the ports are not in the repo). Its write and exec hooks fire only
on the interpreter core, and a changed core takes effect on a ROM reload, not a reset.

**If nothing is free, make room by moving a bound**, as the guide's §2.5 describes. DK64 touches
all 8 MB; its agent lives in 32 KB taken from the top of the heap.

`ram-bounds.py` exists because untouched-and-unpointed-at is not enough. Legacy of
Darkness passed both and froze anyway: its heap ends at `0x80400000` normally and
`0x80634000` in high quality mode, and the agent sat inside the second. A heap is defined
by its bounds, and neither bound is an address inside it.

**Give every watcher a control range that must change**, in the game's own base RAM. A
quiet result from a script that was never running looks identical to a clean region. The
first control tried for Paper Mario was the mod's static code, which proved nothing.

## 5. Find a per-frame hook site, by measuring

**Look for a decomp first.** Diddy Kong Racing went from "no symbol source" to a complete
set of profile inputs in one sitting because
[DavidSM64/Diddy-Kong-Racing](https://github.com/DavidSM64/Diddy-Kong-Racing) targets the
exact ROM: its splat config pins a sha1, and that sha1 was the vanilla ROM to hand. Check
`ver/splat/*.yaml` for the sha1 and for `symbol_addrs_path`, and take the vram↔rom mapping
from the code segment's `start` and `vram` — DKR's main segment is rom `0x1000` at vram
`0x80000400`, so vram = rom + `0x7FFFF400`. Kirby 64 and Paper Mario had decomps too. It is
worth ten minutes to look before disassembling anything.

**Then diff the patched ROM against vanilla at every address you plan to use.** A decomp
describes the retail game; you are hooking a randomizer's output. DKR's patch modifies
vanilla only up to `0xd36eb` and appends the rest — but `thread3_main` is inside that, and
*is* modified, because the randomizer hooks the game thread itself. Hooking there would
have collided with it. `main_game_loop` was untouched across all 1240 bytes, and that is
where the hook went.

When you write that diff, **assert the slice lengths**. A wrong vram→rom constant puts
every offset past the end of the file, and two empty slices compare equal, so every region
cheerfully reports "unchanged". That happened here and was caught only because an
unrelated hex dump printed empty.

Run [`count-calls.lua`](../../../n64/agent/tools/count-calls.lua) on every candidate and
read the rate. A loop is not automatically the frame loop. Kirby 64's first site — an
overlay loop head with 39 branches returning to it — fired **twice in three thousand
frames**. DKR's `main_game_loop` is the opposite trap: it looks per-frame and reads like
it, but runs at ~0.44 per video frame (~26 Hz). That is fine for an agent — OoT's cart poll
is ~300 ms — but write the measured rate in the profile, not "once per frame".

Hook **call sites, not functions**. Kirby's `gtlScheduleGfxEnd` has three callers and two
never run; a `jal` retarget changes one caller, so a per-frame function reached from a
site that never executes is worth nothing.

**Give `count-calls` a control that must fire**, for the same reason watchers need one. It
is what turns "these candidates read zero" into evidence rather than a possibly-detached
hook. It also settles which sites are unconditional: in DKR every site in the loop matched
the control's 37,194 calls exactly, except one that came up 12 short and was therefore
conditional.

**Proving the stub's space is dead is the same measurement.** A name is not evidence:
DKR's `debug_text_print` is called by `main_game_loop` on *every* iteration, so a stub
written over it would have landed on live code. Put the candidates and a known-live
control in `count-calls.lua` and play the parts that would plausibly wake them — a race for
a checkpoint renderer, character select for a character helper. Zeros against a climbing
control are the proof.

## 6. Write it

`agent/<game>/game.env` and `stub.S`, then `agent/build.sh <game>` (needs the
mips64-ultra-elf toolchain in WSL), then `profiles/<game>/profile.toml` by hand, then
register it in `ap64-core::builtin()`. Comment `game.env` with *why* each address is safe
— that comment is the evidence for a decision nobody will remember.

Start `stub.S` from an existing one and keep its two image-check lines
(`agent/common/image_check.inc`): `IMAGE_STOOD_DOWN done` before anything that touches
the agent's RAM, including a reload, and `IMAGE_CHECK done` just before `agent_tick`, with
`IMAGE_CHECK_STATE` at the end. They add about 150 bytes, so leave that much room in the
stub space. `every_stub_checks_the_agent_that_ships_with_it` fails if a stub doesn't load
the sum `build.sh` took. Where the stub copies the agent itself, the profile's `agent_rom`
write names the `lui`/`addiu` pair that loads `AGENT_ROM`: take its offset from the stub as
built, with the check in, and `a_moving_agent_rewrites_the_pair_the_stub_loads_it_by` fails
if it's wrong.

Pin generously in `[[require]]`: the hook site word, a sha1 of the space the stub goes in,
and of every function the stub calls. A pin is what turns "this seed is not the one this
profile was measured against" into a refusal instead of a crash.

Record the release you measured in `[measured]`. Give `world` and `version` for an apworld
that declares a `world_version` in its `archipelago.json`, or `world = "Archipelago"` for one
that ships with Archipelago. Give `header_name` if the randomizer stamps its release in the
header, as MK64's does. AP64 then notes a seed from another release (§8). If there is nothing
to record, say why in a comment, as Paper Mario's profile does.

**When the randomizer moves the code between releases, find it instead of pinning its
offset.** A `[[find]]` names a region by its first word and a sha1, and the seed is refused
unless exactly one word-aligned place matches. Give it the `vram` the region runs at, and
every other place in that code can be written by RAM address, as `name@0x8...`; `name+N`
works too. DK64's main code is the example: the randomizer ships it uncompressed after its
own data, so its ROM offset moves whenever that data changes size, but the functions in it
stay at their RAM addresses. Choose a region the randomizer never patches (DK64 uses the
copy routine its stub already needed pinned), and no write may land inside it, or a
patched ROM has nothing left to find. `Addr` in `ap64-core`'s `profile.rs` lists the
forms. Where the code is the game's own and the randomizer leaves its offsets alone
(CV64, Paper Mario, Kirby 64, Mario Kart 64), fixed offsets are simpler and just as safe.

## 7. Verify before hardware

```sh
cargo run -p ap64-cli --release -- <seed.z64> --check   # or target/release/ap64-patch
```

**`--check` already reports every pin and the room left for the agent.** Do not write a
script to do this; it has been written twice by mistake.

Then `agent-probe.lua` in BizHawk on the patched ROM: it reads `layout.env`, so it needs
`GAME` set at its top. `magic=0x4D363450` and `text=intact` with ticks climbing once per
frame means the agent is loaded, unclobbered and running on the game's thread.

Only then the cart. The console must be powered off for PC-side SD writes.

## 8. When the randomizer releases again

A profile is a measurement of one release. AP64 says when the installed world is a
different one: a `[warn]` line from `--check`, a note in the Patch dialog, and a line in the
session log. It notes, never refuses, because most releases change nothing AP64 depends on.
The pins refuse what they can see, and the stub stands the agent down if its RAM is
overwritten. That leaves anything else to find, which is this routine:

1. **The ROM side, in seconds.** Generate a fresh seed from the new release (a maximal one,
   where options change the payload) and run `--check`. A failed pin names what moved. Where
   the randomizer moves the code rather than changing it, a `[[find]]` absorbs the move
   (§6).
2. **The RAM side, in BizHawk.** Patch that seed and run `agent-probe.lua` with Archipelago
   live, through a few scene changes. `text=intact` and climbing ticks mean the agent is
   loaded and nothing overwrote it. If the randomizer's own RAM grew, §4's tools say where
   it now reaches.
3. **The client.** For a native connector (DK64, Banjo-Tooie), press Start and read what the
   client-fix check says: `ready`, `needed` or `unknown`. `unknown` means the fix's anchors
   moved, and the release's client has to be read again (§1). For the forked OoT connector,
   diff upstream's Lua against what `connectors/oot/UPSTREAM` records.
4. **One session on the console,** with checks going out and items coming in.
5. **Update the profile:** the pins that moved, and `[measured]` to the new release. Say
   what was checked in the commit.

## When it misbehaves

**Ask what is talking to the cart before theorising about why.** An agent error counter
climbing at a fixed rate was chased through four hypotheses about `osMemSize`, WATCH
filters, `PEEKROM` paging and probe contention. It was a `watch_agent.sh` left running
from earlier in the session, polling `mem-peek --addr 0x80482468` — a KSEG0 address, where
`mem-peek` takes a **physical** one, so the agent answered `E_RANGE` every six seconds
forever. Two copies running, 2/6 s = 0.33/s, which is what the counter did.

```sh
Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -match "watch|mem-peek" }
```

The tells were all in the data: a rate that did not move when the client, the game or AP64
changed; one error code and never a neighboring one; and a request id that was always
`0001`, meaning a fresh process each poll.

**Identify an unknown counter by causing a known event**, not by trusting an offset.
Triggering an `E_TOO_LARGE` moved one word, naming `s_last_error`; the next word went +1
and was `s_errors`.

**`0x04` is `E_RANGE`, not "outside RDRAM".** `validate_regions` is shared by `PEEKV`,
`POKEV` and `PEEKROM`, each with a different address space, and `WATCH` raises it for a
filter byte outside its slot. The code alone does not say which.
