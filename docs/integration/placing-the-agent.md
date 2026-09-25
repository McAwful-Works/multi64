# Placing the agent in a ROM

> [Integration guides](README.md) · [The cart agent](cart-agent.md) · [Testing](testing.md)

The agent needs four things from a game: RAM to live in, a per-frame call, a place in ROM,
and something that copies it from ROM to RAM — its own loader, or a copy the game or patch
already makes at boot. This guide is how to find each one, in the
order that avoids redoing work, and what to verify before trusting the result.

Every rule here came from an integration that got it wrong first.

---

## 1. Start from the ROM you will actually run

If the game will run patched — a randomizer seed, a mod, a translation — **generate that ROM
first** and do everything below against it. A patch changes what is free:

- Randomizer and mod payloads commonly load their code into the Expansion Pak starting at
  `0x80400000`, fill the ROM past the retail image, and overwrite dead functions. An agent
  layout that worked on the retail ROM collided with a mod on exactly those three points.
- Some patch output depends on options. One randomizer wrote text over a dead function only
  when a particular option was on. A splice that assumes one option setting must **refuse**
  a ROM built with another, not guess.

Diff the patched ROM against retail before anything else: changed ranges, where its data now
ends, whether IPL3 or the header CRC changed, and which RAM addresses its new code loads with
`lui`. That diff is the map of what not to touch.

## 2. Find RAM for the agent

About 24 KB, never touched by the game or its patch, for as long as the game runs.

### 2.1 Measure during play

Run [`tools/ram-usage.lua`](../../n64/agent/tools/ram-usage.lua) in BizHawk with the Expansion Pak
enabled, and **play**: new file, several map or stage transitions, a battle or boss, every
menu, a save. Ten to twenty minutes. It reports pages that stayed zero and never changed.

The title screen lies. In one game the largest untouched region at the title screen, 640 KB,
was entirely filled by a heap within minutes of play; in another the largest region was split
in two.

### 2.2 Then rule candidates out

"Untouched in the emulator" is necessary, not sufficient. Rule out, in this order:

1. **Anything the game knows about.** With a decomp or symbol map, look up what lives in each
   candidate. Regions that measured untouched turned out to be a graphics task's output buffer,
   the middle of an audio heap, stage-overlay space used by one stage only, and a file buffer a
   randomizer had enlarged.
2. **Anything the RSP writes.** BizHawk's N64 core normally runs the RSP at a high level, so
   memory the RSP writes on real hardware never shows up as written in the emulator.
3. **Heap tails.** A heap grows up from its base. An untouched region just above an address the
   game or patch loads with `lui` is probably a heap that has not filled yet. Scan the code — the
   game's and especially the patch's — for `lui` values and 32-bit pointer words into each
   candidate; a region nothing points at is far safer than one that is merely quiet.
4. **Rare events.** Save buffers, cutscenes, the ending, a debug mode. If you cannot rule these
   out, prefer another region.

**Look for the bounds, not for pointers into the region.** A pointer scan asks "does anything
address these bytes", and a heap answers no right up until it grows. A heap is defined by the
addresses on *either side* of it, and neither of them is an address in the region.
[`tools/ram-bounds.py`](../../n64/agent/tools/ram-bounds.py) reports every RAM address the ROM
builds with `lui`+`addiu`/`ori` near the agent and flags the pairs that bracket it:

```sh
python n64/agent/tools/ram-bounds.py seed.z64 0x80480000 0x80482400 --code 0x1000:0xC0000
```

Give `--code` for each ROM range that really holds instructions, and treat every hit as a
thing to go and disassemble rather than a verdict. Castlevania 64, Paper Mario and Kirby 64 come
back clean. Mario Kart 64 did not: its largest untouched region, 2.3 MB of Expansion Pak, has
both of its bounds built in code, so the agent went above it instead, where nothing brackets
it. And Legacy of Darkness is the game that shows why a bracket matters.

Castlevania: Legacy of Darkness is why this exists. Its main heap ends at `0x80400000` normally
and `0x80634000` in the game's high quality mode, chosen by two instructions at `0x80000680`.
The agent sat at `0x80480000`, inside the second. It measured untouched and nothing pointed at
it, and both facts were true and useless. **Check every mode the player can choose**: a quality
setting, a language, a debug menu, and an expansion-pak-aware allocator are all reasons for a
game to size its heap differently on a run you did not measure.

### 2.3 Prefer the Expansion Pak, guarded

In every integration but one the Expansion Pak was the answer: where the patch left it
alone, play never touched it, and it holds nothing the base game depends on. (The exception is
Donkey Kong 64, which needs the Pak and uses all of it; see §2.5.) Put the agent
there and **skip it entirely below 8 MB**: read `osMemSize` (u32 at `0x80000318`) in the loader
and do nothing if it is under `0x800000`. A console without the Expansion Pak then runs the game
exactly as it would without the agent.

Placing the agent in the base 4 MB is possible in principle and has not been done. Every
base-RAM candidate measured so far failed §2.2.

### 2.4 Keep a load marker

Expansion Pak RAM survives a soft reset. Give the image a known word
([`templates/segment_magic.c`](../../n64/agent/templates/segment_magic.c)), check it before copying,
and copy only when it is absent. That also tells a loaded agent apart from whatever else was in
that RAM.

Read it every frame, before calling the agent, **including when there is nothing to copy** because
the game's own loader brought the image in at boot. That it arrived once does not say it is still
there, and nothing reserves that RAM from the game: a frame that calls into RAM now holding
something else hard locks the console, where a marker that no longer reads back only makes the
agent go quiet.

### 2.5 When nothing is free, move a bound

Donkey Kong 64 touches every page of the 8 MB in play; with Archipelago live, the largest run
left untouched was 20 KB. There is nothing to find, so room has to be made, and the way to make
it is the one the randomizer used for itself: move a bound. The game's heap setup
(`func_global_asm_80610350`) takes the top of its arena as one constant, built by
`lui`/`ori` at `0x80610510`, and carves its fixed buffers downward from there; the heap ends
below them. That constant is `0x805C1040`, which is exactly where the randomizer's own code
starts. Lowering it by 32 KB moves every buffer and the heap's end down with it, and leaves a gap
nothing is allocated in.

What made that safe to believe, each checked against a control:

1. **The bound is the one the heap is built from.** Found by a write hook on the heap list's
   head during boot, which gives the program counter and the values written (start and size), not
   by reading a name.
2. **Nothing else builds the constant.** Only two instruction pairs do: the heap setup, and the
   randomizer's boot code, which loads its own code *to* that address and must not move.
3. **Nothing addresses the gap.** A reference scan that follows each `lui` register until it is
   redefined found no address into the new gap; the same scan over a known-used range found 51.
   `ram-bounds.py` alone is too coarse here: it flagged 117 builds of `0x805C0000`, and reading
   them showed every one resolving elsewhere.
4. **The heap still has room.** A watch of the heap's free list over 121,920 frames with
   Archipelago live put its low point at 739 KB free (largest block 504 KB), so the 32 KB is about
   4% of the worst seen. Measure this through the heaviest scenes the game has.
5. **The gap stays empty.** Zero in every one of 4,064 samples, while a control that must change
   did.

A profile makes the change with an `imm` write on the pair. The split follows the low
instruction: an `ori` does not sign-extend, so `0x805B9040` is `lui 0x805B`/`ori 0x9040` where an
`addiu` pair would need `lui 0x805C`.

A bound like this one can move between randomizer releases: it is wherever the randomizer's code
starts. So don't pin its value. Write the lowered bound as a constant, and check the seed's value
with an `imm` require, which pins the two instructions apart from their immediates and accepts a
range. For DK64, that range runs from the agent's end up to where the randomizer's code ends.
What lies under the written bound is then exactly what was measured, whatever the seed held.
Pin the rest of the function that uses the bound by hash, since that decides how much is carved
out under it.

## 3. Find a per-frame call site

### 3.1 What the site must be

- Called **once per frame**, on the game's thread, after its game logic. Never an interrupt or
  exception handler ([cart-agent.md §1](cart-agent.md#1-the-contract)).
- An existing `jal` whose target you can call first from your own code. Retargeting one
  instruction to your code keeps the instruction count, so nothing in the game moves.
- **Unique**: the only call to that target, so the retarget changes exactly one path.
- **Untouched by the patch**, now and — because patches change — checked again by the splicer
  every time.

**Measure the rate; do not read it off the code.** [`tools/count-calls.lua`](../../n64/agent/tools/count-calls.lua)
puts an execution hook on each candidate and counts it against the frame counter. One
integration hooked a loop whose every case branched back to its head — 39 of them — and it
ran *once in three thousand frames*, because each case called into an overlay that ran its
own frame loop. The agent loaded, its image stayed intact, and it ticked twice. Count the
**call site**, not the function: two of that game's three callers of the right function
never ran at all, so a function-level count would have been just as misleading.

Good sites have been a graphics retrace callback's call into the game's step function, and a main
loop's call into its object or game-state update. A game-state manager's per-state function list
is also tempting; check how it dispatches before using it — one masked function pointers so that
only addresses below 4 MB could be called.

### 3.2 The code at the site must exist from the first frame

A retargeted `jal` runs from the first frame the game reaches it, often before any logo. Whatever
it jumps to has to be in RAM by then:

- **With a decomp**, the hook is compiled into the game's main code segment, so it is loaded at
  boot.
- **Without one**, the hook stub has to go over code that is loaded at boot and never executed:
  a dead debug function is ideal. Code a patch loads later (from a logo screen, say) is **not**
  usable for a hook that runs before it — the call would jump into RAM that is not there yet.

### 3.3 Proving code is dead

Before writing over a function:

- search the uncompressed code segments for any `jal`/`j` to its range and any pointer word into
  it; the only hits should be calls from inside the function itself;
- confirm the patch you will ship does not write into it, **for every option setting** your
  splicer accepts;
- say what you could not scan. Compressed overlays cannot be searched as bytes; if the evidence
  is "no references in the uncompressed segments", write that, not "unused".

For a few hundred bytes of padding inside a busy segment, the page-level map from
`ram-usage.lua` cannot help: the 4 KB page around them is in use whatever they do.
[`tools/watch-ranges.lua`](../../n64/agent/tools/watch-ranges.lua) watches the exact bytes
and reports the first frame any of them changes, and where — a difference at the very start
means something claims the run outright, one partway in suggests a neighboring array
reaching into it.

With a decomp, a function that exists but is called from nowhere (for example a case no dispatcher
reaches) is the cleanest host: replace it and pad the replacement back to the function's **exact**
size, so nothing after it moves.

## 4. Choose a ROM location for the image

- **Past everything.** Put the image after the patched ROM's last data, not the retail image's.
  The N64 header has no size field and the boot CRC only covers the first MiB, so appending past
  the old end is safe; round the new size to a 4-byte boundary. The SummerCart64 maps up to 64 MiB.
- **Not in padding a patch may grow into.** Patches that recompress files or append per-item data
  grow with options and seeds. Leave margin, or append past the end.

## 5. Build and splice

### 5.1 The loader

Whatever runs at the call site has to: call what the `jal` used to call; check `osMemSize`; check
the load marker; if absent, copy the image from ROM with the **game's own ROM-copy routine**; check
the marker again; zero the BSS once; then call `agent_tick()`.

Details that bit, the last two only on a console:

- **Do not assume the copy is synchronous.** Check the marker again after the copy returns, and
  run nothing until it reads back. If it does not, return and try next frame.
- **Zero the BSS explicitly.** Segment-copy helpers copy the loadable image; they do not clear
  BSS. Uninitialized agent state is a first-frame crash that looks like a hardware fault.
- **Invalidate the instruction cache after the copy**, unless the routine you call already does.
  One game's copy routine invalidated the data cache only; the game itself follows it with
  `osInvalICache` whenever it loads code, and so must the loader. Without it the CPU can run
  stale I-cache lines instead of the agent, and no emulator models that, so every test before the
  console passes.
- **Call only a copy routine whose transfer is safe during play.** One game's routine ran exactly
  once, at boot; called from a frame hook, its PI transfer did not survive the game's own traffic,
  and the console black-screened. A bisect on hardware isolated the copy as the only step at fault.
  What matters is how the routine transfers, not who calls it: Donkey Kong 64's wrapper is also
  called only at boot, but it goes through `osPiStartDma` and the PI manager, on the same message
  queue as the game's in-play loaders, and it loads the agent on a console. A routine that starts
  a raw PI DMA outside the manager is the unsafe kind: during play, its completion interrupt can
  be taken as the end of the game's next transfer. When the game has no routine that is safe
  mid-play, do not copy at all: put the image where something already copies it at boot (§5.5).

Find the game's ROM-copy routine by what the game or its patch already calls to load code: its
arguments (ROM offset? virtual ROM through a file table? file index?) decide how the loader calls
it. [`templates/hook_stub.S`](../../n64/agent/templates/hook_stub.S) assumes
`(rom_offset, dst, size)` in `a0`–`a2`, and does not invalidate either cache.

### 5.2 With a decomp

Build the agent as a segment of the game's own build, at the RAM address from §2, and keep every
retail address unchanged — then the result is still a patch against the retail ROM:

- Put the hook in a dead function (§3.3) padded to its exact size, and retarget the one existing
  `jal`. **Measure the padding after every change**: one loader change made the hook 16 bytes
  longer and moved every later function until the pad was shrunk to match.
- Link the agent segment at the RAM address you chose, and load it from an explicit ROM offset
  if the image will be spliced somewhere other than where the decomp's linker put it. Reserving a
  gap in the ROM layout through the build tool was tried and failed silently: the tool's padding
  segment type emitted nothing, so the ROM position never advanced.
- Diff the built ROM against retail. Only the header CRC, the `jal`, the hook function and the
  appended segment may differ, and no symbol may have moved.
- To run on a patched ROM, splice only those ranges into it (§5.4).

### 5.3 Without a decomp

[`templates/link-flat.sh`](../../n64/agent/templates/link-flat.sh) builds both pieces:

```sh
templates/link-flat.sh AGENT_VRAM AGENT_ROM AGENT_MIN_RAM STUB_VRAM HOOK_ORIGINAL ROM_COPY
```

| Argument | Meaning |
|---|---|
| `AGENT_VRAM` | RAM address from §2 |
| `AGENT_ROM` | ROM offset from §4 |
| `AGENT_MIN_RAM` | `0x800000` when the agent lives in the Expansion Pak |
| `STUB_VRAM` | RAM address of the dead code the stub replaces (§3.2) |
| `HOOK_ORIGINAL` | The address the retargeted `jal` used to call |
| `ROM_COPY` | The game's ROM-to-RAM copy routine (§5.1) |

It writes `agent.bin`, `stub.bin` and `layout.env` (addresses, sizes and the counter offsets
[`tools/agent-probe.lua`](../../n64/agent/tools/agent-probe.lua) needs). It refuses an agent with
undefined symbols.

One linker trap is worth knowing even if you write your own: a `jal` to a symbol imported with
`--just-symbols` failed with **"relocation truncated to fit: R_MIPS_26"**, because the imported
addresses arrive sign-extended to 64 bits. The stub calls through `jalr $t9` instead.

### 5.4 The splicer

Whatever writes the pieces into the ROM should refuse, rather than warn, when:

- the call site does not hold the expected `jal`;
- the code the stub replaces does not match the retail bytes;
- anything but known, expected edits differs in IPL3 (§6);
- anything of the ROM lies where the image goes.

It should then report every byte range it changed, and a rebuild from committed files should
reproduce the tested ROM byte for byte. That last check is what lets a later reader trust that the
ROM on the console came from the source in the repository.

### 5.5 When something already loads it

A patch that brings its own code in at boot usually copies a fixed ROM range to a fixed RAM
address. If the agent's image fits inside that range, past the patch's last data, it arrives with
the patch before the first frame, and the loader copies nothing: at most it checks `osMemSize`,
checks the marker, zeroes the BSS, and calls `agent_tick()`. Two integrations work this way. In
one, the agent is part of a payload the randomizer already loads, and that payload needs the
Expansion Pak and carries the BSS as zeros, so its loader only checks the marker. In the other,
no copy routine was safe to call mid-play (§5.1), so the agent was put where the patch's boot copy
would bring it in, and its loader does all four.

Two things change when the load is not yours:

- **Pin the load itself.** The splicer should check the instructions that set the copy's ROM
  range, length and destination. If a patch update shortens that copy, the agent is simply not
  loaded, and nothing else would notice.
- **BSS cannot tell you whether it was zeroed.** The boot copy brings in the load marker too, so the
  marker reads back from the first frame whether or not the BSS was cleared. Keep the "zeroed" flag
  inside the stub's own image, in the range the boot copy restores: it is then reset at every boot,
  including a soft reset, and the stub zeroes the BSS once after each.

## 6. The boot CRC and IPL3

The header CRC covers ROM `0x1000`–`0x101000`; retargeting a `jal` or writing a stub there
invalidates it, and IPL3 on a console checks it. Recompute it for the ROM's boot chip with
[`tools/n64crc.py`](../../n64/agent/tools/n64crc.py) (`--fix`), which supports CIC-6102 and CIC-6103.

**A patch may have disabled the check instead.** One randomizer's patch replaced IPL3's two
checksum branches with NOPs and left the header CRC stale. Emulators do not care. Rather than ship
a ROM whose IPL3 is modified, restore the retail IPL3 and write a correct CRC; that boots by the
normal rules, and it booted on hardware. `n64crc.py` refuses a ROM whose IPL3 matches no known boot
chip for the same reason.
