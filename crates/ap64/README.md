<img src="../../branding/ap64.svg" alt="" width="72" align="left" />

# AP64

**Archipelago on a real N64** · a Multi64 product

<br clear="left" />

Play Archipelago N64 seeds on a real console, from Windows. AP64 does two jobs:

1. **Patch.** It adds the M64P cart agent to a seed that Archipelago has already
   patched. The agent is a small program that lets the PC read and write the game's
   RAM over the cart's USB link.
2. **Play.** It runs a connector for that game against the cart, through
   [Multi64](../multi64/README.md)'s bridge, so Archipelago's own client can talk to the console
   as it would to an emulator. The ROM is read from the cart itself (M64P `PEEKROM`),
   so there is no file to choose: pick the game, press Start, open the client.

Supported today, each patched and played on a SummerCart64 with checks sent and items
received (the first three on 2026-09-18, Kirby 64 on 2026-09-21, Banjo-Tooie and Mario Kart 64
on 2026-09-22, Donkey Kong 64 on 2026-09-23):

- **Castlevania 64 (US 1.0)**, through Archipelago's BizHawk Client.
- **Paper Mario (US 1.0)** with the Paper Mario Randomizer, through BizHawk Client.
- **Ocarina of Time (NTSC 1.0)**, Archipelago's OoT world, through OoT Client. The agent is
  loaded with the randomizer's payload; the connector is a fork of Archipelago's OoT connector.
  The patch is made in the decompressed game, but only the files it changes are stored
  uncompressed: every other file keeps the seed's compressed bytes, so the output is about
  1.2 MiB larger than the seed rather than 21 MiB. The game's one in-scene sign that a
  check was collected is a slot the next event overwrites, which an emulator reads every
  frame. The agent watches that slot on the console, also every frame, and its events ride
  back on requests that were already going out, so a check does not wait for the scene to
  change. A ROM patched before this falls back to AP64 sampling the slot from the host,
  which narrows the gap rather than closing it.
- **Kirby 64: The Crystal Shards (US)**, Archipelago's k64 world, through BizHawk Client.
  The hook is `jal gtlScheduleGfxEnd` at `0x800059C0`, and both it and the stub are in the
  `main` segment, which is resident for the whole run. The stub goes in 1088 bytes of zeros
  in that segment's data, watched byte for byte through attract mode and a play session
  without one of them changing, and identical with every option off and at maximum. The
  agent's RAM was measured across 67,000 frames with the Expansion Pak untouched throughout,
  and the one instruction in the 32 MiB that forms `0x80400000` is a dead store nothing
  reads back. In BizHawk the agent loads, its image stays identical to the ROM's, and it
  ticks once per frame.
- **Banjo-Tooie (US)**, jjjj12212's Banjo-Tooie world, through its Banjo-Tooie Client. Needs
  an Expansion Pak, as the retail game does. Like Donkey Kong 64's client (below), it reads
  emulator memory through EmuLoader and falls back to RetroArch's Network Commands, which AP64
  answers from the cart. The client's own code does all of the game's work, so a new release
  of the world needs nothing from AP64 on the client side. As released, that fallback stops the
  client the moment it attaches, for want of one attribute its monitor loop reads, so Start
  offers a fix to the installed `banjo_tooie.apworld` exactly as it does for DK64's (below).
  Almost all of the game is compressed, so the hook, the stub and the agent all go in
  the block the randomizer appends, and the agent is in RAM before the first frame because it
  sits inside the part of that block the randomizer's boot code already copies there. That
  block is the randomizer's own code, so a release that changes it is refused until it has
  been measured again (#312). First played with a 1,077-location seed through a forked
  connector script, which this replaced (#319). Played natively on 2026-09-25: the settings
  written at the start, then 121 checks sent and items received over half an hour with no
  error. Death link, tag link and victory have not been tried yet.
- **Mario Kart 64 (US)**, Archipelago's Mario Kart 64 world, through BizHawk Client. The agent
  lives in the Expansion Pak and stays out of the way without one. Whether the world itself
  runs without a Pak is not yet known: its own code sits where the Pak is, and no console has
  been tried with the Pak removed. Played for 21 checks across three courses, with every item
  landing exactly once.
- **Donkey Kong 64 (US)**, the DK64 randomizer's Archipelago world (v1.5.8, with the ROM built
  on dk64randomizer.com), through its DK64 Client. Needs an Expansion Pak, as the retail game
  does. The client uses no connector script: it reads emulator memory through a library called
  EmuLoader, and with no emulator running it falls back to RetroArch's Network Commands over
  UDP. AP64 answers those itself, from the cart. As released, that fallback is missing a method
  the client calls on every loop, so pressing Start checks the installed `dk64.apworld` first
  and, if it needs the fix, asks once in a dialog before making it. The original is kept in
  AP64's own data folder (`%APPDATA%\dev.multi64.ap64\backups`). It checks again at every Start,
  because the world replaces itself when the randomizer updates. DK64 uses all 8 MB of RAM, so
  the agent lives in 32 KB taken from the top of the game's heap; the heap's low point measured
  739 KB free with the change. The randomizer moves the game's main code around in the ROM
  between releases, so AP64 finds it in each seed instead of expecting it at one offset, and a
  release that only moves it still patches. The heap's top, which is where the randomizer's own
  code begins, is read from each seed too: any top that keeps that code clear of the agent is
  accepted, and the game gets the same heap it was measured with. A release that changes the
  functions AP64 hooks or the heap setup is refused until it has been measured again (#309,
  #312). Played with checks going out and items arriving, among them a
  Golden Banana and the Donkey kong.

Written and working, but **not offered in the app**, because the game cannot be played
through for a reason outside AP64. Kept in the tree and checked by the same tests as the
rest (`ap64_core::withheld`), so re-offering it is a one-line change:

- **Castlevania: Legacy of Darkness (US)**, Archipelago's CVLoD world, through BizHawk Client.
  The same Konami engine as Castlevania 64, but relinked: two thirds of CV64's code is still
  in there and almost none of it at the same address, so every address was found in LoD
  itself. Archipelago's own patch is the map — it splices a boot stub into the front of a
  dead debug block and loads a 32 KB payload with the game's ROM-copy routine, so AP64 puts
  its hook stub in the tail of that same block and calls that same routine. Nothing in the
  16 MiB references that block, and the space AP64 writes is byte-identical to retail with
  every option off and with all 52 of them at maximum.

  Patched and played on a SummerCart64 on 2026-09-21: the agent loads, AP64 reads the ROM off
  the cart, the client connects and checks come through. **But a seed freezes**, in attract
  mode at a fixed point and again during play. That freeze is not AP64's: the same seed with
  no agent spliced into it freezes at the same point in BizHawk, and the retail ROM does not.
  It is a bug in the CVLoD world (seen on v2.0.2). AP64 refuses the seed rather than patch a
  game it knows freezes; the profile moves back into `builtin` when a seed can be played
  through.

## What the window shows

Two cards, each a fixed height: dropping a seed, patching it or a session failing changes what
they say, never how much room they take. Anything that grows — the checklist, the list of writes,
the session log, the bridge address — opens in a window of its own.

The patch window keeps its detail behind **More details**, which anyone can open: the seed's
header line and every check, then the SHA-1 and what was written. **Developer details**, the
switch in the corner, is for what only someone working on AP64 reads: the connector's name, the
round-trip counters, the bridge address, and the addresses behind a failed status. It is off
until turned on, and remembered after that.

## The session log

Each line starts with the local time it was logged, to line up with the client's log file in
Archipelago's `logs` folder. Each session's log is also written to a file as it happens, in
`%APPDATA%\dev.multi64.ap64\logs`, named by the time the session started. The newest 20 are
kept. **Show file** in the log window opens the folder with the current one selected, so a crash
or a closed window loses nothing, and the log can be sent with a report.

What it says:

- **First:** the AP64 version and the commit it was built from, the game, the Multi64 address, and
  where the log file is. Then, once the cart answers, where the agent is on it. For most games
  that's its ROM offset; if a seed's data ran into the agent's usual place, the log says so.
- **The cart going quiet.** The first stall of a run gets a line saying how long the agent was
  silent. The rest of that run is one line 30 seconds later, with the count and the longest. A
  reconnect says how long the cart was out of reach.
- **For a client AP64 answers itself** (DK64 Client, Banjo-Tooie Client), every reply that had
  to say the cart hadn't answered yet, with the address as the client wrote it, so it can be
  found in the client's log. Also any reply slower than the half second the client waits, the
  client asking again after a pause, and the client reconnecting, with the last request AP64
  answered before it and how fast. When the client logs "timed out", that line shows whether
  the request reached AP64 at all.
- **Every five minutes, a summary:** requests answered, errors, stalls, reconnects, and whether
  the client is connected.

## Starting and stopping

**Start** means keep at it until you say otherwise. A console reset, a ROM swapped, the wrong
game loaded, Multi64 restarted — none of those end a session. It goes back to waiting for the
cart, says which link is down, and picks up where it left off when the console returns. Only
**Stop** ends a session, because only you know whether you are done playing.

Each time the link goes, the Archipelago client's connection is reset rather than closed
politely. That is deliberate: these clients read a line and hand it to `json.loads`, and a clean
close gives them an empty string whose decode error their socket task does not catch — it dies
without a word and the client goes on showing itself connected. A reset is the one ending they
recover from on their own, so the client reconnects by itself once the console is back.
A client AP64 answers over UDP (Donkey Kong 64's) has no connection to reset: its requests go
unanswered while the console is away, and it tries again by itself until they are answered.

## Patching a seed

Generate and patch your seed with Archipelago as usual, then either drop the `.z64` onto
the AP64 window or run:

```sh
cargo run -p ap64-cli -- <seed.z64>            # writes <seed>-agent.z64 beside it
cargo run -p ap64-cli -- <seed.z64> --check    # verify only
```

The seed is checked before anything is written. AP64 refuses it if any of these fail:

- The header must match a known game and release.
- The code the agent hooks into, and the space its stub goes in, must hash to exactly
  what the profile was measured against. Some randomizer options move that code; the
  check says which.
- Where a randomizer moves the game's code between releases, the profile locates it by the
  same kind of hash, and it must be found exactly once. Everything placed relative to it is
  then checked where it was found.
- A constant a randomizer moves between releases must load a value in the range the profile
  accepts, with the instructions that load it unchanged.
- Nothing of the seed may lie where the agent is appended, and the agent must end within the
  64 MiB a cart holds. The agent goes at the offset its profile was tested at, and a seed whose
  data reaches that far gets it past the data instead, with the stub told where. Banjo-Tooie
  and Ocarina of Time are the exceptions: the game's own loader puts their agent in place, so a
  seed that reaches it is refused.
- After any known boot-code changes are undone, the boot code must be a retail one, so
  the header checksum can be recomputed.

After writing, the output is diffed against the seed. A change that no step accounts
for withholds the output.

Those checks are all of the ROM, but the agent runs from RAM, and a randomizer release that
starts using more RAM can overwrite it with every check above still passing. So on the
console, the stub also checks the agent's code each frame, a slice at a time, against a sum
taken when it was built. If the code has changed, the stub stops calling the agent and never
reloads it. The game plays on, the cart goes quiet, and the session log says the agent
stopped answering. On the seeds tested, an overwrite was caught within 30 frames.

No retail ROM is needed, and none of Nintendo's code ships with AP64. The profiles
contain hashes, addresses and a few single instruction words; the agent and stubs are
our own code.

## Layout

AP64 is the one part of this repository that is game-specific. Multi64, Xfer64, the cart
agent and the specs name no game; everything that does lives in these crates.

| Path | What |
|---|---|
| [`crates/ap64`](.) | The Tauri 2 app (`src-tauri/` + plain-JS `src/`, no bundler), `e2e/` headless checks |
| [`crates/ap64-core`](../ap64-core) | ROM byte orders, the CIC-6102/6103/6105 header checksum, profiles, verify/apply |
| `crates/ap64-core/profiles/<game>/` | `profile.toml` plus the agent image and hook stub it writes, with the build's `layout.env` |
| `crates/ap64-core/agent/` | `build.sh <game>`, and per game a `game.env` and hand-written `stub.S` |
| `crates/ap64-core/tools/<game>/` | BizHawk probe scripts for checking a patched ROM before it goes on a cart |
| [`crates/ap64-cli`](../ap64-cli) | `ap64-patch`, a thin command line over the core |
| [`crates/ap64-cart`](../ap64-cart) | M64P over multi64d: RDRAM reads and writes, cart ROM reads (cached), retry and reconnect |
| [`crates/ap64-connector`](../ap64-connector) | Embedded Lua running a connector script, the `ap64` API it calls, the TCP side the client connects to; and the native connector AP64 answers itself (RetroArch Network Commands over UDP, for Donkey Kong 64 and Banjo-Tooie) |
| `crates/ap64-connector/connectors/<id>/` | Forked Archipelago connector scripts (MIT, with `UPSTREAM` provenance) |

A profile's blobs are this repository's own cart agent ([`n64/agent`](../../n64/agent/README.md),
with the M64P handler from `n64/test-rom`), built flat at the profile's addresses with the
N64 toolchain (`crates/ap64-core/agent/build.sh <game>`, in WSL). CI has no N64 toolchain,
so the blobs are committed, and `BUILD_REV` records the revision they were built from.
Rebuild them after any change to the agent. `layout.env` is written by that build, and a
test checks that the hand-typed profile agrees with it.

## Checks

CI's Rust job covers the crates. The page has its own headless job, against a stubbed
Tauri bridge:

```sh
cd crates/ap64/e2e && npm ci && npx playwright install --with-deps chromium && npm test
```

Byte-identity against outputs that have run on a console. ROMs can't be committed, so
this test reads paths from the environment:

```sh
AP64_CV64_SEED=<seed.z64> AP64_CV64_EXPECTED=<spliced.z64> \
  cargo test -p ap64-core --test local_roms -- --ignored
```

## Building the app

```sh
cd crates/ap64 && npm install && npm run build    # NSIS installer under target/release/bundle
```

The icons, the header mark (`src/brand-mark.svg`) and the installer graphics (`src-tauri/windows/*.bmp`)
come from the brand master `branding/ap64.svg`: the Archipelago ring as low-poly spheres, generated by
[`branding/generate.py`](../../branding/generate.py), with the bitmaps from
[`branding/installer-images`](../../branding/installer-images/README.md).

## License

MIT OR Apache-2.0, like the rest of the repository. The forked connector scripts keep
Archipelago's MIT notice beside them (`UPSTREAM`).
