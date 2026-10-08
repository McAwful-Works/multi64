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
   so there is no file to choose: pick the game, press Start, open the client. The window
   calls every game's client "the AP client"; the list below says which entry in the
   Archipelago Launcher that is, and the session log names it too.

Supported today, each patched and played on a SummerCart64 with checks sent and items
received (the first three on 2026-09-18, Kirby 64 on 2026-09-21, Banjo-Tooie and Mario Kart 64
on 2026-09-22, Donkey Kong 64 on 2026-09-23, Bomberman 64 on 2026-09-26, Bomberman Hero,
Bomberman 64: The Second Attack and Star Fox 64 on 2026-09-27):

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
  #312). The hook is in the game's main loop rather than in the frame it runs for ordinary
  play, because the DK arcade and Jetpac run their own code in that frame's place: hooked in
  the ordinary frame, the agent went silent in Jetpac and the link dropped until the player
  left it. Played with checks going out and items arriving, among them a Golden Banana and the
  Donkey kong, and through a session in Jetpac with no drop.
- **Bomberman 64 (US)**, Happyhappyism's Bomberman 64 world, through BizHawk Client. Addresses
  come from the bomberhackers/bm64 decomp, whose mapping the world's own patch uses too. The
  hook is the game loop's call to `HuPrcCall`, which runs every Hudson process once a pass, and
  the stub goes over `memalign` in the game's copy of GNU malloc, which nothing calls: 0 calls
  over 41,940 frames of play while `malloc` ran 167,420 times. The agent lives in the Expansion
  Pak, which the game never touches, and stays out of the way without one. The world's optional
  enemy shuffle needs its companion Lua script, which runs inside BizHawk, so it cannot work on a
  console. That script also shows the name of each item received, so on a console items arrive
  without that text. Played for 13 checks across four stages, with the keys that open new
  stages arriving and working. The game loop pauses for a second or two when a stage loads,
  which the link rides out. On a console the world's ROM freezes during the title screen
  cutscene, with or without the agent (seen with the world's own ROM); skipping past it avoids
  the freeze, and play is unaffected. Start says so, along with the two Lua limits, before the
  first session.
- **Bomberman Hero (US)**, Happyhappyism's Bomberman Hero world, through BizHawk Client.
  Addresses come from the Bomberhackers/bmhero decomp. Every screen, from the title to a level,
  builds its frames through one loop, and the hook is that loop's call to the function that
  builds each frame. The stub goes over `Debug_BackupMemTest`, a leftover test screen nothing in
  the ROM refers to: 0 calls over 57,210 frames of play, and its bytes never changed. The game
  keeps all of its code uncompressed, so that was checked against every instruction in the ROM,
  not a shortlist. The agent lives in the Expansion Pak, which the game never touches, and stays
  out of the way without one. The world's companion Lua script runs inside BizHawk and does no
  game logic, but it is what shows each item received and the stage tracker on D-pad Down, so
  on a console neither appears; Start says so before the first session. Played for 6 checks
  (radios and stage clears), with the stages, Adok Bomb and Gold Heart received arriving and
  working. The link rides out the second or two the frame loop pauses while a stage loads.
- **Bomberman 64: The Second Attack! (US)**, Happyhappyism and SavageWizzrobe's Bomberman The
  Second Attack world, through BizHawk Client. Addresses come from the Bomberhackers/bm64tsa
  decomp's symbols. Like Bomberman 64, the hook is the main loop's call to the function that
  runs every Hudson process, here once per game frame at 30 Hz, and the agent lives in the
  Expansion Pak, which the game never touches. The stub goes over `bmLoadBitmapTile`, which no
  resident code calls; the compressed overlays link to the boot code by name, so that could
  only be settled by play: 0 calls over 45,420 frames, and its bytes never changed. The world's
  seed is not finished when its patch is applied: BizHawk Client runs the world's ROM adjuster
  the first time it connects, which sets up the doors, three power-ups and the chosen models
  and repacks the ROM. On a console that comes too late, and adjusting a ROM AP64 has already
  patched breaks it, so AP64 runs the adjuster itself before the checks (see
  [Patching a seed](#patching-a-seed)). Played for 12 checks across Alcatraz and Thantos,
  power-ups, a boss and a Pommy transformation among them, with the Thantos Coordinates that
  open the next planet arriving and working, and traps and power-ups landing.
- **Star Fox 64 (US 1.1)**, Auztin's Star Fox 64 world, through its Star Fox 64 Client. The
  world compiles all of the game's work into the ROM, which speaks a protocol of its own through
  two buffers in RAM, and its BizHawk Lua only relays packets between them and the client's TCP
  port. AP64 does that relay itself, from the cart: at Start it calls the client rather than
  waiting for it (`ap64-connector`'s `relay`,
  [host-connector.md §11](../../docs/integration/host-connector.md#11-relaying-a-rom-that-speaks-for-itself)).
  One patched ROM serves every seed. Star Fox 64 boots with CIC-6101, the only game that does,
  whose checksum AP64 computes as 6102's. The hook is the graphics loop's call to
  `Controller_UpdateInput`, once per game frame at 30 Hz; the stub goes over a function of
  libultra's remote debugger, which the game never starts (0 calls over 29,040 frames); and the
  agent lives in the Expansion Pak above the 2 MB the world reserves there. On a console the
  world's ROM also polls an EverDrive's USB registers, and on a SummerCart64 that dropped the
  connection every few seconds, so AP64 changes two instructions in the world's startup code to
  keep it on the buffers the relay reads. Played for 8 checks across Sector X, among them both
  Mission Accomplished and Mission Complete, with laser upgrades, rings and bombs received and
  landing, over one connection that held for the whole session.

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
  If the randomizer release differs from the one AP64 was measured against, a `note:` line
  says so, once a session.
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

A session needs the Multi64 app, which has its own installer. If it is not running, Start
starts it, looking beside AP64's own folder and then in the installer's default places
(`MULTI64_APP` names its `multi64.exe` if it is anywhere else). If it is not running and can't
be found, the Multi64 row says **Not installed** and the status says to install it, since that
is the one thing waiting won't fix. The session still keeps trying, so Multi64 started from
wherever it is gets picked up.

Every game here needs an **Expansion Pak**, because the agent runs in its memory. Without one
the agent never starts (the stub skips it, or the game does not boot at all) and nothing on the
console can answer, so AP64 cannot tell a
missing Pak from a console that is off or running another ROM. So it says what is needed once,
in a **Before you start** window the first time AP64 opens (and in the Playing help).

When the cart is connected but the ROM has been silent for 15 seconds, the status suggests why,
and the session log says so once:

- On an EverDrive (Multi64 set up for the X7 or the PRO), the port is open whether or not a
  ROM is running, and AP64's agent for those carts has never been shown to work on one. The
  status says the agent is experimental and to check the ROM was patched for that cart. For a
  game that runs without an Expansion Pak, the log adds that a missing Pak would also do it.
- On the SummerCart64, for a game that runs without an Expansion Pak, it names the Pak as the
  likely cause.
- For a game that does not run without one (Ocarina of Time, Banjo-Tooie, Donkey Kong 64:
  `game_needs_expansion_pak` in its profile), a running game has the Pak, so the status asks
  instead whether the ROM is the one AP64 added the agent to.

Before a session begins, Start may ask about two things, one at a time. First, a fix the game's
Archipelago client needs (Donkey Kong 64 and Banjo-Tooie, above). Then the game's notes: what
to know about its randomizer on a console that AP64 cannot change, such as Bomberman 64's title screen
freeze. They come from the game's profile (`notes`), show at every Start until **Don't show
these again for this game** is ticked, and show again if they change.

Each time the link goes, a client that connects to AP64 (the games played through a connector
script) has its connection reset rather than closed politely. That is deliberate: these clients
read a line and hand it to `json.loads`, and a clean close gives them an empty string whose decode
error their socket task does not catch — it dies without a word and the client goes on showing
itself connected. A reset is the one ending they recover from on their own, so the client
reconnects by itself once the console is back.
A client AP64 answers over UDP (Donkey Kong 64's and Banjo-Tooie's) has no connection to reset:
its requests go unanswered while the console is away, and it tries again by itself until they
are answered.
A client AP64 calls (Star Fox 64's) is simply let go: its client takes the closed connection as
the game going away and waits for another, and AP64 calls it again once the console is back and
the game has something to say.

Before anything else, Start checks that the ROM on the cart is one this version of AP64 patched.
A new version can change the agent or the stub that loads it, so a ROM patched by another
version is refused, with a note to add the agent to the seed again.

When the stub stands the agent down because its code in RAM changed (see
[Patching a seed](#patching-a-seed)), it stays down until the console is powered off. A reset
is not enough where the agent lives in the Expansion Pak, because that RAM survives a reset and
the changed code is found again.

## Patching a seed

Generate and patch your seed with Archipelago as usual, then either drop the `.z64` onto
the AP64 window or run:

```sh
cargo run -p ap64-cli -- <seed.z64>            # writes <seed>-agent.z64 beside it
cargo run -p ap64-cli -- <seed.z64> --check    # verify only
cargo run -p ap64-cli -- <seed.z64> --cart ed64    # for an EverDrive-64 X7 (experimental)
```

**The agent is built for one flash cart**, because it drives one: the SummerCart64, the
EverDrive-64 X7 or the EverDrive-64 PRO. The Patch dialog asks which, as **Flash cart**. It
defaults to the cart Multi64 is set up for when Multi64 is running, and otherwise to the last
one chosen. The command line takes `--cart sc64|ed64|ed64pro`, SummerCart64 by default. Only
the SummerCart64's agent has run on a cart. The EverDrive builds are **experimental**: they
build, fit, and load in an emulator, but no one has played through one yet, so the dialog
marks them. They differ in size, so a seed is checked for the cart chosen, and changing the
cart checks it again. Donkey Kong 64 is where that shows: the X7's agent, the largest, does
not fit under the randomizer's code, so that pairing is refused.

**A seed some worlds finish after the patch is finished first.** Bomberman 64: The Second
Attack's world leaves its seed for BizHawk Client to adjust the first time it connects: the
world's own `pack.exe` unpacks the ROM, the doors, three power-ups and the chosen models are set
up, and it is repacked. That is too late for a console, so when a seed has not had it, AP64 runs
the same steps itself, with the `pack.exe` from the world installed in Archipelago, and says so
in the Patch dialog, or as a line from `ap64-patch`. It takes about half a minute; a seed already adjusted, in BizHawk or by
AP64, is patched as it is. AP64 only runs the adjuster it was written against: every file it
takes from the apworld (the adjuster's source, whose steps it follows, and the program it runs)
must hash to world 1.0.2's. A world whose adjuster differs, a world that is not installed, or a
PC that is not running Windows leaves the seed unadjusted, and the dialog says why. The result
is byte for byte what the world's own adjuster makes of the same seed.

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

Each profile also records which release of its randomizer it was measured against. Where
the world installed in Archipelago is a different release, or MK64's header names a
different one, AP64 says so. That goes in the Patch dialog, as a `[warn]` line from
`ap64-patch`, and in the session log at Start. It is a note, not a refusal: most releases
change nothing AP64 depends on. It just means the seed is one AP64 has not been tested with.
The world's version is its `world_version`, or Archipelago's own for a world that ships
with it. Paper Mario's world declares no version, so it gets no note.

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
| [`crates/ap64-core`](../ap64-core) | ROM byte orders, the CIC-6101/6102/6103/6105 header checksum, profiles, verify/apply |
| `crates/ap64-core/profiles/<game>/` | `profile.toml` plus the agent image and hook stub it writes, with the build's `layout.env`: the SummerCart64's build here, the EverDrives' in `ed64/` and `ed64pro/` |
| `crates/ap64-core/agent/` | `build.sh <game>`, and per game a `game.env` and hand-written `stub.S` |
| `crates/ap64-core/tools/<game>/` | BizHawk probe scripts for checking a patched ROM before it goes on a cart |
| [`crates/ap64-cli`](../ap64-cli) | `ap64-patch`, a thin command line over the core |
| [`crates/ap64-cart`](../ap64-cart) | M64P over multi64d: RDRAM reads and writes, cart ROM reads (cached), retry and reconnect |
| [`crates/ap64-connector`](../ap64-connector) | Embedded Lua running a connector script, the `ap64` API it calls, the TCP side the client connects to; and the native connectors AP64 runs itself (RetroArch Network Commands over UDP, for Donkey Kong 64 and Banjo-Tooie; a TCP relay to a ROM that speaks for itself, for Star Fox 64) |
| `crates/ap64-connector/connectors/<id>/` | Forked Archipelago connector scripts (MIT, with `UPSTREAM` provenance) |

A profile's blobs are this repository's own cart agent ([`n64/agent`](../../n64/agent/README.md),
with the M64P handler from `n64/test-rom`), built flat at the profile's addresses with the
N64 toolchain (`crates/ap64-core/agent/build.sh <game>`, in WSL), once for each cart. CI has
no N64 toolchain, so the blobs are committed, and `BUILD_REV` records the revision they were
built from. Rebuild them after any change to the agent. `layout.env` is written by that build,
and tests check that the hand-typed profile agrees with every cart's.

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
