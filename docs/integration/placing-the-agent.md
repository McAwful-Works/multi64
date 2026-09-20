# Placing the agent in a ROM

> [Integration guides](README.md) · [The cart agent](cart-agent.md) · [Testing](testing.md)

The agent needs four things from a game: RAM to live in, a per-frame call, a place in ROM,
and something that copies it from ROM to RAM. This guide is how to find each one, in the
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

### 2.3 Prefer the Expansion Pak, guarded

In all three integrations so far the Expansion Pak was the answer: where the patch left it
alone, play never touched it, and it holds nothing the base game depends on. Put the agent
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

## 3. Find a per-frame call site

### 3.1 What the site must be

- Called **once per frame**, on the game's thread, after its game logic. Never an interrupt or
  exception handler ([cart-agent.md §1](cart-agent.md#1-the-contract)).
- An existing **`jal`** whose target you can call first from your own code. Retargeting one
  instruction to your code keeps the instruction count, so nothing in the game moves.
- **Unique**: the only call to that target, so the retarget changes exactly one path.
- **Untouched by the patch**, now and — because patches change — checked again by the splicer
  every time.

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

Two details that bit:

- **Do not assume the copy is synchronous.** Check the marker again after the copy returns, and
  run nothing until it reads back. If it does not, return and try next frame.
- **Zero the BSS explicitly.** Segment-copy helpers copy the loadable image; they do not clear
  BSS. Uninitialised agent state is a first-frame crash that looks like a hardware fault.

Find the game's ROM-copy routine by what the game or its patch already calls to load code: its
arguments (ROM offset? virtual ROM through a file table? file index?) decide how the loader calls
it. [`templates/hook_stub.S`](../../n64/agent/templates/hook_stub.S) assumes
`(rom_offset, dst, size)` in `a0`–`a2`.

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

## 6. The boot CRC and IPL3

The header CRC covers ROM `0x1000`–`0x101000`; retargeting a `jal` or writing a stub there
invalidates it, and IPL3 on a console checks it. Recompute it for the ROM's boot chip with
[`tools/n64crc.py`](../../n64/agent/tools/n64crc.py) (`--fix`), which supports CIC-6102 and CIC-6103.

**A patch may have disabled the check instead.** One randomizer's patch replaced IPL3's two
checksum branches with NOPs and left the header CRC stale. Emulators do not care. Rather than ship
a ROM whose IPL3 is modified, restore the retail IPL3 and write a correct CRC; that boots by the
normal rules, and it booted on hardware. `n64crc.py` refuses a ROM whose IPL3 matches no known boot
chip for the same reason.
