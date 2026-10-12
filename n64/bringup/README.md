# Cart bring-up ROM

> **Report format:** [cart-bringup-report-v0.md](../../docs/spec/cart-bringup-report-v0.md) · **Host tool:** [`multi64-test-connector bringup`](../../docs/connectors/test-rom.md#cart-bring-up) · **Agent:** [n64/agent](../agent/README.md)

`multi64_bringup.z64` runs the cart agent's **own drivers** on a cart, outside any game, and
reports what they do: on screen, and in a report a host reads through the agent itself. It exists
because the agent's EverDrive drivers are written against libdragon's and Krikzz's code, and when
it was written neither had answered inside a game: the one X7 try, on 2026-10-07, got no answer from
the ROM, and an agent that stays silent inside a game has no way to say why.

Run it on a **SummerCart64 first**. The SC64 driver is proven inside games, so its report is
the baseline: on another cart, the first result that differs from the SC64's is where to look.

**ROM 1.0 ran on a SummerCart64,** on 2026-10-08, where every check passed
([Hardware record](#hardware-record)); that run is the baseline. **And on an EverDrive-64 X7,** on
2026-10-09, where the X7 driver worked with the PI otherwise idle, and under load cut a reply off
part-way and left the host deaf to the cart. The committed binary is **1.1**, which carries the fix
for the first; the second is fixed in `multi64d`. **1.1 ran on the same X7 later that day,** where
the CPU-word build answered every request but one across three load levels. Its SC64 build is the
same source as 1.0's, so the baseline still stands for it.

**On 2026-10-12 the X7 agent answered inside a game for the first time:** an Ocarina of Time seed
patched for the X7, left at its title screen, where every `HELLO` and every read was answered
([Hardware record](#hardware-record)). Later that day the tester reported, by screenshot, an AP64
session on the same game with an Archipelago client connected. No item sent or received and no
sustained play has been shown on an X7, and one cart is not validation: the EverDrive builds stay
experimental.

## Design

**Where it lives.** A libdragon program of its own, beside `test-rom/`, not a mode of it. The
test ROM moves USB through libdragon's `usb.c`, and its 2026-09-18 X7 result depends on that
binary staying as it is. This ROM shares the agent's sources instead, including
`test-rom/mem_proto.c` through the agent, and draws on libdragon's text console as the test ROM
does. libdragon's `usb.o` is linked, because its `debug.o` refers to it, but nothing in this
ROM calls it.

**What runs.** Four complete agent builds, each made from the agent's own source files
(`agent.c`, a driver, `pi_io.c`, `cart_rom.c`, `mem_proto.c`) with the agent's own flags from
[`../agent/flags.mk`](../agent/flags.mk):

| Variant | Driver | Prefix |
|---|---|---|
| 1 SC64 | `sc64.c` | `bsc_` |
| 2 X7, CPU words | `ed64.c` over `pi_io.c` as shipped | `bx7_` |
| 3 X7, PI DMA | `ed64.c` over `pi_io.c` built with `PI_IO_DMA` | `bx7d_` |
| 4 PRO | `ed64pro.c` | `bpro_` |

Each build is linked into one object and every symbol given its prefix, so four agents share one
image (`Makefile`, `variants.h`). Before that link, the agent's calls into its driver, and the
driver's and `PEEKROM`'s calls into `pi_io`, are renamed to `wrap_*`. [`wrap.c`](wrap.c) counts and
times each one into the report and calls the real function. The agent's code is not edited. Two
more copies of `pi_io.c` stand alone, `pio_` and `pdma_`, for the harness's own probes.

One difference from a game build, unavoidable: libdragon is o64 and the agent is o32, so the
variants are compiled with `-mabi=o64` in place of `-mabi=32`. An o32 function saves only the low
half of the registers an o64 caller expects preserved. Same source, same flags otherwise, not the
same object.

**The IO-or-DMA switch.** `pi_io.c` moves a run of words by CPU load and store, as the agent
ships. Built with `PI_IO_DMA` it uses PI DMA through a bounce buffer, the way libdragon and Krikzz
move the X7's USB window. That breaks the agent's rule against touching the PI's DMA registers,
so it is a diagnostic build, not an option for games. It is exposed three ways:

- in this ROM, as variant 3: **R** on the controller, or a host writing `ctl_variant`;
- in the stage 2 buffer tests, which try all four combinations of the two;
- in the agent build, as `make CART=ed64 PI_IO=dma` (`build/ed64-dma/`), for a game test once a
  cart has shown it is needed.

**The stages:**

1. **Identify.** Raw reads of every cart's ID registers before anything is unlocked, then each
   driver's own initialization in libdragon's order (64drive, PRO, X7, SC64) until one answers.
2. **Blocks.** What one spin of each driver's wait costs, read 1024 times, against the spin limit
   [`spin_limits.sh`](spin_limits.sh) reads out of the driver sources at build time. Then 512 bytes
   written to the cart's buffer and read back by CPU words and by PI DMA, all four ways.
3. **Link.** The cart's agent build called once a frame, for a host to talk to.
4. **Conditions.** Stage 2 again under background ROM DMA started from a timer interrupt, where a
   game's PI manager starts its own. The timer also records how long interrupts were held off. The
   host repeats the link under each load level.

The report layout, what each field means and how a host finds it, is
[cart-bringup-report-v0.md](../../docs/spec/cart-bringup-report-v0.md).

## Build

```sh
cd n64/bringup
make
```

It needs `N64_INST`, like the test ROM ([n64/README.md](../README.md#toolchain)), and builds the
agent sources from `../agent` itself. A prefix installed by `setup-toolchain.sh` with the toolchain
[`toolchain.lock`](../toolchain.lock) pins rebuilds the committed `multi64_bringup.z64` byte for
byte: SHA-256 `dcb36cdec9fc6eafc8a9a2cea63a004743255fb0779bdb0fabfb30dbd42749c4` (1.1). The same
toolchain rebuilds 1.0, `58b347db…`, from its commit. That toolchain
is libdragon `e356bf3` with mips64-elf GCC 16.2.0, built by libdragon's toolchain script at that
same commit.

## Reading the screen

The screen shows everything that does not need a link, which is all there is to go on when the
link never comes up.

On the way up, each start-up and stage step prints a line as it begins (`boot: timer`,
`identify: X7 init`, `blocks 0: buffer 2` and so on), so a ROM that hangs leaves the step it hung
in as its last line. A screen that stays black, with not even the first line, never reached the
ROM's code. Once every stage has run, the screen below replaces those lines.

```
multi64 bring-up 1.1  cart SC64  link SC64  load off  f1234
identify ed14 ........>........  ed04 ........>........     raw ID reads, then after X7 init
         sc64 ........>53437632  sys ........ ........     SC64 IDENT before and after unlock
         init PRO- X7- SC64+  d64 ........  probes 3F     + answered, - did not, . not tried
         rom 80371240 multi64 bringup
blocks   per read x spins = longest wait | per read, moderate load
         PI_STAT  0.10 us x 100000 = 10.0 ms | 0.12 us
         SR_CMD   1.20 us x 100000 = 120.0 ms | 1.90 us
buffer   1FFE0000      no load       moderate load
         IO>IO    ok            ok              ok / BAD@offset/bytes / wbusy / rbusy / n/a
         ...
link     init 1/1 ready 1  ticks ...  frames ...  req ... err ... last ..
         rx .../... ...B lost ...  tx .../... ...B  rd .../...
         pi_io ... busy ...  max us: tick ... rx ... tx ...
load     irq gap max ...us  DMAs ...  skipped ...
```

The values above are placeholders, not measurements. **Z** steps the load (off, moderate, heavy),
**A** runs stages 2 and 4 again, and on an X7 **R** switches between the CPU-word and DMA builds.

## Tester checklist

### 1. SummerCart64 baseline (first, on the maintainer's cart)

1. Build the host tool: `cargo build --release -p multi64-test-connector`.
2. Start `multi64d` on the SC64, from Multi64 (Settings → **Cart**: SummerCart64 or
   Auto-detect, then **Start bridge**) or with `cargo run -p multi64d --release -- --serial COM4`.
3. Load `multi64_bringup.z64` onto the SC64 and boot it with the cart's own bootloader, then
   press Reset:

   ```sh
   cargo run -p sc64-smoke --release -- --port COM4 --boot-rom n64/bringup/multi64_bringup.z64
   ```

   The SC64 menu hung on every libdragon ROM on 2026-10-08, this one included, on a black
   screen before any ROM code ran. Afterward, `--boot-menu` puts the menu back; until then the
   cart keeps skipping it. See
   [Loading a ROM onto a SummerCart64](../README.md#loading-a-rom-onto-a-summercart64).
4. Run:

   ```sh
   target/release/multi64-test-connector bringup --out bringup-sc64-baseline.json
   ```

5. What a working SC64 shows: `identify.cart` and `identify.driver_init` PASS; all eight
   `blocks.buffer.*` checks PASS; `link.default.load_off.traffic` PASS. Under moderate and heavy
   load a few timeouts are normal, because the agent gives up on a busy PI by design rather than
   stall the game; they are reported as INFO with their count, which varies from run to run.
   The tool's own writes of the load level and the agent build cross the same link, so each is
   sent up to three times before the run stops there (`link.<build>.aborted`). After the checks,
   one `note:` line for each write that had to be sent again names it; the JSON's `notes` keeps
   them.
6. Keep the JSON: it is what another cart's run is compared against. Commit it under
   [`baselines/`](baselines/) as `sc64-<date>.json`. The newest by name is the one the bundle
   ships, as `sc64-baseline.json`, so record a new one whenever the ROM changes. Currently that is
   [`baselines/sc64-2026-10-12.json`](baselines/sc64-2026-10-12.json), from bring-up ROM 1.1; the
   first, [`baselines/sc64-2026-10-08.json`](baselines/sc64-2026-10-08.json), is from ROM 1.0.
   When a run and its baseline come from different builds of the ROM, the comparison says so on
   its first line.

Anything else on the SC64 is a fault in this ROM or the tool, not in the cart, and has to be
fixed before the ROM goes to anyone.

### 2. X7 handover

Build the bundle from a clean checkout of a commit, on Windows in Git Bash, with `npm install`
already run in `crates/multi64` and `crates/ap64`:

```sh
sh n64/bringup/handover/pack.sh
```

It writes `target/handover/cart-diagnostics-<commit>/` and a zip of it, holding:

- the Multi64 installer, built from that commit (optional for the tester);
- the AP64 installer, built from that commit, which only phase 3 below needs: the tester patches
  a seed with it for their cart, so the ROM carries that commit's agent;
- `multi64d.exe`, the same build as the one inside the Multi64 installer;
- `multi64-test-connector.exe`;
- `multi64_test.z64` and `multi64_bringup.z64`, the committed ROMs;
- `sc64-baseline.json`, a copy of the newest `baselines/sc64-<date>.json`;
- [`diagnose.bat`](handover/diagnose.bat), [`play-session.bat`](handover/play-session.bat),
  [`diagnose.ps1`](handover/diagnose.ps1), which both run,
  [`zip-folder.ps1`](handover/zip-folder.ps1), which zips both the results and the bundle itself,
  and the tester's [`README.txt`](handover/README.txt);
- `VERSION.txt`, which records the commit, which baseline was copied, and every file's SHA-256.

The tester copies the two ROMs to the SD card, closes Multi64, AP64 and Xfer64, and double-clicks
`diagnose.bat`, which walks them through the run. For phase 3 they first install the bundle's AP64,
patch their seed with it for their cart, and copy that ROM to the card too. The script does not use
Multi64's bridge, whose log lives only in the app. Instead it runs the bundled `multi64d` itself,
at debug level with `--serial-trace` (every byte read from the cart), with a fresh daemon for each
phase, and a second one in phases 1 and 3 when the experiment below calls for it. Phase 3 is
optional and the script asks before it:

1. **Control:** `multi64_test.z64` and `multi64-test-connector suite`. On an X7 or SC64 that ROM
   moves USB through libdragon, which passed on an X7 on 2026-09-18 and on the morning of
   2026-10-09, so a failure here points at the tester's cable, driver or port rather than the
   agent's driver. On a PRO it is no control: libdragon does not support the PRO, so the test ROM
   uses `n64/test-rom/ed64pro.c`, the same unproven mapping the agent uses, and the script says so.

   Phase 1 also carries an experiment. In the second X7 run on 2026-10-09 this ROM sent nothing
   through `multi64d` until the suite's direct-serial step had purged the port
   ([Hardware record](#hardware-record)). So before the suite, the script sends the suite's first
   request, `set-mode --mode 1`, on its own, up to three times, through a daemon started with
   `--clear-serial=false`. If none is answered, it stops that daemon, starts another with
   `--clear-serial=true`, which purges the port in both directions as it opens it, and sends the
   request again; the suite then runs against that second daemon. It has to happen before the suite,
   because the suite's direct-serial step purges the port too and would hide the result. Each
   daemon has its own log (`phase1-` and `phase1-purged-multi64d.log`), and the tries are in
   `phase1-probe.txt` and `phase1-purged-probe.txt`:

   | Phase 1 shows | What it means |
   |---|---|
   | The first daemon is answered | The silence did not happen this time, and the run says nothing about its cause |
   | Only the purged daemon is answered | Stale state in the USB path when the bridge started, which a purge clears |
   | Neither is answered, but the suite's direct-serial step is | A purge at open is not enough; something else the direct step did is. That step also reopens the port and talks to the cart without `multi64d` |
   | Nothing is answered at all | The ROM or the cart, not the host: the ROM's screen and the cart OS version are what is left to compare |

   The suite ends by printing the ROM's `tx_failures`, its writes that gave up since boot. When
   that count covers the tries no daemon answered, those requests reached the cart and only its
   replies were lost, as in the second X7 run.
2. **Bring-up:** `multi64_bringup.z64` and `bringup --baseline`. If it reports `link.hello` FAIL
   (no HELLO_ACK), the script has the tester press **R** (the top line changes to `link X7 DMA`)
   and runs it again.
3. **In a game (optional):** the agent as AP64 places it, with a game running around it. The first
   AP64 build tried on an X7, an Ocarina of Time seed on 2026-10-07, opened the port and was never
   answered, and nothing from that run shows whether the agent ever started; this phase keeps the
   bridge's log of such a run. The script asks whether a ROM patched by the bundle's AP64 for this
   cart is on the SD card, and skips the phase on anything but `y`. Otherwise it asks which game,
   has the tester boot it and wait at the title screen, and then:

   - sends M64P `HELLO` (`mem-hello`) five times, 3 seconds apart. All five go out whatever the
     answers: in a game the agent gives up on a busy PI rather than stall the game, so how many
     get through is part of the result (`phase3-hello.txt`);
   - if none was answered, runs phase 1's purge experiment again: it stops the daemon, starts one
     with `--clear-serial=true` and sends five more (`phase3-purged-hello.txt`, and that daemon's
     `phase3-purged-multi64d.log`);
   - once a `HELLO` has been answered, reads RDRAM with `mem-peek`: 28 bytes at `0x300`, the values
     the boot code left, then 64 at `0x400`, then `0x300` again, which should match the first, then
     4096 bytes at `0x400`, the most one region may hold. Last, it reads the game name from the ROM
     header with `mem-rom-peek`, which is how AP64 reads the cart when it starts a session. All of
     it goes to `phase3-reads.txt`. Nothing is written to the game;
   - asks whether the game is still running as normal, and puts the answer in the transcript.

   It ran on an X7 on 2026-10-12, with an Ocarina of Time seed, and every `HELLO` and read was
   answered ([Hardware record](#hardware-record)). Before the bundle went out, the script's phase 3
   ran alone (`-GameOnly`) on a SummerCart64 with a Kirby 64 seed running, and then as part of a
   whole run of the bundle: both times 5 of 5 `HELLO`s were answered through the first daemon,
   every read was answered, the two `0x300` reads were identical, and the header read back as
   `Kirby64`. The branch where no `HELLO` is answered has only run against a stand-in daemon that
   refuses every connection.

A phase whose daemon's link does not come up within about 8 seconds is skipped rather than left to
time out check by check, and the script prints the daemon's last warnings, which name the cause: a
port held by another program, a port that does not exist, or a PRO that did not answer its identity
check.
Everything goes into `results-<time>/` beside the script and is zipped: the session transcript,
each tool's output, each phase's `multi64d` log, the daemon's `GET /` before and after each phase,
the machine's serial ports and FTDI driver versions, Windows' version, the cart OS version the
tester types in and the bring-up JSON files. That one zip is what comes back. It asks for no
photos of the TV: what a ROM shows on screen only matters when its link never comes up, so at the
end the script asks the tester to describe a black or stuck screen, and the answer goes in the
transcript.

The zip is written even when something still holds a results file open. On a tester's PC on
2026-10-12, with an X7, the phase 2 daemon was still running after the script had stopped it, and
still held its log; the zip step failed with the whole zip lost, though the folder was intact. The
same tester reports Multi64 failing to close its bridge. Why that `multi64d` outlives being ended
is not known. So the script no longer trusts one attempt:

- after stopping a daemon it asks Windows whether the process is gone, and tries `taskkill` if not;
- before zipping, and at the start of a run, it stops any `multi64d` from the bundle that is still
  running, and says so in the transcript;
- a daemon that survives both is most likely waiting in the cart's USB serial driver, so the script
  asks the tester to unplug the cart's USB cable and records whether the process then goes, which
  is the evidence for or against that;
- a daemon that is still there after the unplug is left out of the "close Multi64" question at the
  start of a run, which it would otherwise never let end, and the transcript says its port may be
  held;
- the zip step reads files whoever else has them open, and one that still cannot be read is named
  on screen and left out, with a request to send the folder as well.

The phases run inside one `try`. An error part-way, such as a tool that an antivirus program will
not let run, is written to the transcript with where it happened, and the closing questions, the
sweep for daemons and the zip still follow, so a failed run sends back what it gathered. Each look
at the machine before the phases (the Windows version, the serial devices, their drivers) has a
`try` of its own: on a PC whose WMI will not answer, the transcript loses those lines and the run
goes on. It then asks for the cart and the port outright, showing the ports `multi64d --list-ports`
found before the port question.

#### A play session

`play-session.bat` (`diagnose.ps1 -Play`) runs none of the three phases. It is for a connection
that drops during play, which the phases cannot show: on 2026-10-12 an X7 playing Ocarina of Time
through AP64 dropped on leaving the Deku Tree and stayed silent until AP64 and Multi64 were both
restarted, with nothing done on the console, and no log of the bridge existed for it.

The script starts `multi64d` itself, logged as above, in place of Multi64, sends three HELLOs, and
has the tester open AP64, press Start and play against that daemon. When the connection drops the
tester comes back without restarting anything, and the script looks for what is stuck, each step
timed in UTC in the transcript to line up with the daemon's log:

1. the daemon's `GET /`, then five HELLOs through the same daemon, untouched since the drop.
   Answered: the bridge and the cart are fine, and what is stuck is on AP64's side;
2. unanswered: a new daemon that does not purge the port, and five more. Answered: the stuck state
   was in the old daemon, or went when its port was closed and opened again;
3. still unanswered: a new daemon with `--clear-serial`, and five more. Answered: bytes left in the
   USB link were in the way. Unanswered: the cart side is not answering at all, and the tester is
   asked what the game is doing.

Whatever the answers, the first daemon's log holds every byte the cart sent before and after the
drop. AP64's own session logs written during the run (`%APPDATA%\dev.multi64.ap64\logs`) are copied
into `ap64-logs/` in the results. Its dry runs here walked every step against a daemon with no
cart; it has not run against a cart or a real drop.

Before anything else the script looks for programs that would hold the cart's port. A `multi64d`
from an earlier run of the same bundle is stopped. While Multi64, AP64 or Xfer64 is open (AP64 may
stay open for a play session), it asks the tester to close them and looks again. A `multi64d` that
is still there once they are closed is taken to be the bridge of an installed Multi64 that has
gone, which one tester's PC had: it has no window and no tray icon to quit it from, so the script
names it and offers to stop it, on a `y` or `n`. Unless the answer is `y` or `yes` and Windows
ends it, the transcript says that it stays, and that the tests will most likely not be able to
start, and the run carries on to gather what it can. A daemon of the script's own that finds
another bridge on port 38765 exits, and its phase is skipped.

The script finds the cart and its port by the USB IDs in `crates/cart-probe` (X7 `0403:6001`,
SummerCart64 `0403:6014`), and uses neither without the tester's say. Both are stock FTDI IDs,
which an ordinary USB serial adapter reports too, so with the cart not plugged in the one matching
port could be some other device. When exactly one such port is plugged in, the script shows its
Windows name and the tester presses Enter to accept it or types another, once for the cart and
once for the port. Otherwise it asks outright; the PRO's IDs are not known, so a PRO is always
asked for. An answer that names no cart, or no COM port, is asked for again, five times at most;
after that the run stops on the error and zips what it has.
`-Cart ed64|ed64pro|sc64` and `-Port COMn`, which are not asked about when given, `-SkipControl`
(no phase 1) and `-GameOnly` (phase 3 alone, without asking whether there is a ROM for it) pass
through `diagnose.bat`. Every `multi64d` it starts is put in a Windows job object that kills it
when the script exits, so closing the window mid-run does not leave the port held. Running it on
the maintainer's SummerCart64 checks the bundle itself before it goes out. The offer to stop a
bridge and the Enter-to-accept prompts have been walked here only in dry runs, against a stand-in
device list and COM ports that do not exist; no tester has met them yet.

### 3. What the results say

| What differs from the SC64 baseline | What it means |
|---|---|
| `identify.cart` or `identify.driver_init` | The driver's register sequence is wrong for this cart. The raw values on screen show how |
| `blocks.buffer.IO>*` fail, `DMA>DMA` passes | The USB window does not take CPU words: the agent needs the DMA path |
| Every buffer combination fails | The window does not read back what was written. A fact about the window, not a verdict; look at the link checks |
| The CPU-word build's link fails and the DMA build's passes | The same conclusion as the buffer tests, shown end to end |
| Link passes with no load, fails under moderate | PI sharing: the driver loses to a game's DMA |
| Everything matches | The driver works on this cart, and a silent agent inside a game is about how it is placed in that game |

## Hardware record

**2026-10-12, EverDrive-64 X7, an AP64 session with an Archipelago client: reported, not logged.**
The same day as the fifth run below, the tester sent a screenshot. No results zip and no bridge log
came with it, so this entry is what the screenshot is reported to show, and nothing here was
checked against a log.

- **AP64's Play card:** game `The Legend of Zelda: Ocarina of Time`, connector `Ocarina of Time`,
  Multi64 `Connected`, Console `Running`, AP client `Connected`, status
  `Playing - AP client connected`, and activity
  `port 28921, 46 client requests, 142 cart round trips, 0 stalls, 0 reconnects`.
- **Archipelago's Ocarina of Time client,** beside it: `Starting n64 connector`, then
  `Successfully Connected to N64`.

So an Archipelago client has connected to an AP64 game on an X7, and AP64 counted 142 round trips
to the cart with no stall and no reconnect. **What it does not show:** how long the session ran,
any play beyond connecting, an item sent or received, or a clean shutdown. Which ROM and AP64 build
were used is not recorded either. Sustained play on an X7 is still unshown.

**2026-10-12, EverDrive-64 X7, fifth run, and the first inside a game** (`cart-diagnostics-f590935`,
the bundle with phase 3 from [#375](https://github.com/McAwful-Works/multi64/pull/375); X7 OS 3.11),
ROM 1.1, SHA-256 `dcb36cdec9fc6eafc8a9a2cea63a004743255fb0779bdb0fabfb30dbd42749c4`, on the same
cart, Windows, FTDI driver and port as the runs below. Started at 11:31 -04:00.

- **Phase 1, test ROM 1.14:** 34 passed, 0 failed, 3 skipped. The first request was answered on
  the first try, through a daemon started with `--clear-serial=false`. The second run's silence did
  not happen, so the purge experiment says nothing about its cause.
- **Identify and blocks:** every check passed, with the timings of the earlier runs: one
  `PI_STATUS` read 263 ns, one X7 USBCFG read 3.61 µs (5.87 µs under load).
- **Link, CPU-word build,** the build AP64's X7 agents ship: no reply came back wrong and the
  driver counted no failed send.
  - No load: 36 of 36, round trip 64/82/114 ms (min/mean/max).
  - Moderate load: 33 of 34, one timeout. `recv_lost` 67; `pi_io` found the PI busy on 4 of 10,304
    accesses.
  - Heavy load: 32 of 34, two timeouts. `recv_lost` 12; the PI busy on 79 of 189,563 accesses.
    The longest agent tick was 305.8 ms, 305.7 ms of it in receive, against 61 to 65 ms, mostly in
    send, in the second and fourth runs. Why one receive ran that long is not known.
- **Link, PI DMA build** (diagnostic only; no game ships it): **answered at all three load levels
  this time.** No load, 36 of 36. Moderate, 27 of 30, three timeouts, `recv_lost` 201. Heavy, 36 of
  36. The same ROM stopped answering under moderate load in the second and fourth runs, so this is
  one pass of a build that stalls some of the time, not a fix.
- **Against the SC64 baseline:** 0 checks failed.
- **Phase 3, inside a game:** an Ocarina of Time seed, booted and left at its title screen. The
  tester answered yes when the script asked whether the ROM had been patched for this cart by the
  bundle's AP64; nothing in the results can confirm which AP64 patched it.
  - The agent answered **5 of 5** M64P `HELLO`s, 3 s apart, through a daemon started with
    `--clear-serial=false`: protocol 0, agent version 256, 8 MiB of RDRAM, a 64 MiB cart ROM.
  - Every `mem-peek` was answered, and the two reads of `0x300` were identical. The 4096-byte
    read's reply is the 4122-byte message the first run saw cut off after 2048 bytes; here all of
    it arrived.
  - `mem-rom-peek` at `0x20` read back `THE LEGEND OF ZELDA`, the game name in the ROM header.
  - `multi64d` logged no error, and no warning but the one it prints at every start with
    `--cart ed64`. The tester reported the game still on its title screen afterward.

**What this run does not show:**

- **A play session.** Phase 3 sends five `HELLO`s and five reads in about 15 s, writes nothing to
  the game, and runs at the title screen, with no Archipelago client. One connected later that
  day, by the tester's report above.
- **Why the 2026-10-07 try was silent.** Three things could have done it, and none was tested as
  the cause. That ROM was patched by an older AP64 and carried an older X7 agent, from before the
  agents were rebuilt for [#372](https://github.com/McAwful-Works/multi64/pull/372). The second run's phase 1 showed that
  this cart can say nothing through `multi64d` from the bridge's start, for a reason not yet
  explained. And on this tester's PC a `multi64d` has outlived being stopped (the third run, under
  [X7 handover](#2-x7-handover)), which nobody knew to look for on 2026-10-07.
- **That the X7 works.** This is one cart, one game and one boot.

**2026-10-12, EverDrive-64 X7, fourth run** (`cart-diagnostics-355fbbf`, the bundle with the
hardening from [#374](https://github.com/McAwful-Works/multi64/pull/374); X7 OS 3.11), ROM 1.1, on
the same cart, Windows, FTDI driver and port. Started at 10:35 -04:00. The third run, at 09:51 that
morning, is the one whose zip was lost ([X7 handover](#2-x7-handover)); its results are not
recorded here. This time the zip was written, and the transcript records no daemon outliving its
stop.

- **Phase 1, test ROM 1.14:** 34 passed, 0 failed, 3 skipped, answered on the first try through a
  daemon started with `--clear-serial=false`.
- **Identify and blocks:** every check passed, with the same timings as before.
- **Link, CPU-word build:** no load, 36 of 36, round trip 64/80/109 ms (min/mean/max). Moderate
  load, 33 of 34, one timeout, `recv_lost` 67. Heavy load, 33 of 34, one timeout, `recv_lost` 67,
  longest agent tick 61.1 ms. No failed send and no wrong reply.
- **Link, PI DMA build:** no load, 36 of 36. Under moderate load 5 of 30 timed out and it stopped
  answering, as in the second run and with the same shape: the last bytes from the cart arrived at
  14:37:27Z, a write to the cart could not finish 9 s later (`failed to write whole buffer`),
  `multi64d` dropped the link and reopened it, and nothing more came from the cart in the 36 s
  left. Its counters were again not read afterward.
- **Against the SC64 baseline:** 0 checks failed.

**2026-10-12, SummerCart64, bundle `cart-diagnostics-f590935`** (`SCv2`, firmware `2.20.2`), ROM
1.1, Windows 11 Pro 10.0.26300, FTDI driver 2.12.36.20. The whole bundle on the maintainer's cart,
at 11:09 -04:00, before the same bundle's X7 run above.

- **Phase 1, test ROM 1.14:** 34 passed, 0 failed, 3 skipped.
- **Phase 2, against the baseline:** 0 checks failed. No load, 36 of 36, round trip 60/88/210 ms
  (min/mean/max). Moderate load, 31 of 34, three timeouts; heavy, 35 of 36, one timeout.
- **Phase 3, inside a game:** a Kirby 64 seed. 5 of 5 `HELLO`s answered, every read answered, the
  two `0x300` reads identical, and the header read back as `Kirby64`.

**2026-10-09, EverDrive-64 X7, second run** (`cart-diagnostics-55cdfa9`, with the fixes from
[#372](https://github.com/McAwful-Works/multi64/pull/372); X7 OS **3.11**, updated from 3.09 since
the first run), ROM 1.1, SHA-256 `dcb36cdec9fc6eafc8a9a2cea63a004743255fb0779bdb0fabfb30dbd42749c4`,
on the same cart, Windows, FTDI driver and port as the first run below.

- **Phase 1, test ROM 1.14: 11 passed, 20 failed, 3 skipped.** The same ROM had passed on this
  cart that morning under OS 3.09. This time the cart sent nothing through `multi64d`: the
  daemon's serial trace has no byte from the cart between its start at 19:57:22Z and 19:59:21Z,
  and every check through it timed out. The cart was receiving, though: its `DIAG`, read afterward,
  counted 27 writes that gave up (`tx_failures`), and this ROM writes only once the host has sent it
  something. Then the suite's direct-serial step released the port and opened it itself, which
  purges it in both directions, and from there every check passed, including those through
  `multi64d` once it had taken the port back. `multi64d` had started with `clear_serial=false`, and
  taking the port back does not purge either.

  **Not established:** what the purge cleared, and whether the cause is OS 3.11 or stale state in
  the USB path when the bridge started. The direct step reopened the port as well as purging it,
  but the daemon's own start was a fresh open too, and did not help. Later bundles' phase 1
  tests a purge at the bridge's start ([X7 handover](#2-x7-handover)); the 2026-10-12 runs above
  never reached it, because the cart answered. See
  [l3-over-everdrive-x7.md](../../docs/spec/l3-over-everdrive-x7.md) §4.5 item 8.
- **Identify and blocks:** as in the first run, to within 1%. Every check passed; one `PI_STATUS`
  read cost 263 ns and one X7 USBCFG read 3.60 µs (5.87 µs under load).
- **Link, CPU-word build,** the build AP64's X7 agents ship: **answered every request but one, across all three load levels.**
  - No load: 36 of 36, round trip 64/83/114 ms (min/mean/max).
  - Moderate load: 34 of 35, one timeout. 67 of the driver's receive calls reported part of the
    stream lost (`recv_lost`), yet only that one request went unanswered; the counters do not say
    how the 67 map onto messages. `pi_io` found the PI busy on 7 of 11,632 accesses.
  - Heavy load: 36 of 36, round trip 64/92/128 ms. The longest agent tick was 64.7 ms, 62.9 ms of
    it in send.

  The driver counted no failed send at any level and no reply came back wrong: the retry in
  `ed64_send` held. No message stalled part-way either, so `ed64-l2`'s drop was never exercised.
  Against the SC64 baseline 0 checks failed, and the X7's longest ticks ran 1.6 to 1.7 times the
  SC64's: 13.7, 20.9 and 64.7 ms against 8.7, 12.5 and 39.9 ms, with no, moderate and heavy load.
- **Link, PI DMA build** (diagnostic only; no game ships it): with no load, 36 of 36. Under
  moderate load 4 of 19 timed out and it stopped answering, so the tool skipped the rest. The last
  bytes from the cart arrived at 20:00:13Z. Nine seconds later a write to the cart could not finish
  within its 1 s (`failed to write whole buffer`), so `multi64d` dropped the link and reopened it,
  and nothing more came from the cart in the 36 s left. A write that cannot finish suggests the cart
  was no longer reading, so this build had most likely stopped reading USB, not only answering.
  Its counters were not read afterward, so why is not known.

This run does not explain why the AP64 build tried on an X7 on 2026-10-07 never answered either.
Phase 1's silence has the same shape, a port that opens and a ROM that says nothing, but is not
shown to have the same cause. And nothing here ran inside a game.

**2026-10-09, EverDrive-64 X7, through the remote tester's bundle** (`cart-diagnostics-3c9ffb0`; X7
OS 3.09), ROM 1.0, SHA-256 `58b347db7fbc97d21e00f1fc82a4444eeec9ece4d74ddf9916f0a587e7049126`, Windows 11
Pro 10.0.26200, FTDI driver 2.12.36.20. `diagnose.bat` picked the X7 and its port by USB IDs. The
first run of the agent's own X7 driver anywhere but inside a game.

- **Phase 1, test ROM 1.14:** 34 passed, 0 failed, 3 skipped (HUD text, rumble, and the ROM
  version, which the script does not supply). libdragon's X7 path works on this setup, so the
  cable, driver and port are not in question.
- **Identify:** the X7 driver's init answered. The X-series version register read `0xED640013`
  once unlocked, and USBCFG reported the USB unit powered.
- **Blocks:** every buffer test read back what it wrote, by CPU words and by PI DMA, with and
  without load. One `PI_STATUS` read costs 263 ns and one X7 USBCFG read 3.61 µs (5.87 µs under
  load), so `ED_BUSY_SPINS` (20,000) lets one wait run 72 ms, 117 ms under load.
- **Link, CPU-word build, no load:** 36 of 36, round trip 64/84/116 ms (min/mean/max). The driver
  lost nothing and failed no send, and none of its 13,857 PI accesses found the PI busy. **The X7
  driver works**, outside a game.
- **Link, moderate load:** 4 of 23 timed out, and then the cart stopped answering as far as the
  tool could tell. One request got no reply at all. The next large one's 4122-byte reply stopped
  after 2048 bytes, the header and 4 of its 9 blocks. The cart went on answering later requests,
  but `multi64d` counted those messages as payload of the cut-off reply until 4122 bytes had
  arrived, 45 s later, and then dropped the link on a bad trailer. The tool skipped the rest of
  that build, and its switch to the DMA build timed out (`link.x7_dma.aborted`). The counters that
  would say which PI access failed were lost with the link.

Both faults were fixed in [#372](https://github.com/McAwful-Works/multi64/pull/372), and the run
above was made with both fixes: `ed64_send` retries a busy PI once a message's first block is out,
and `ed64-l2` drops a message that stalls part-way for 500 ms. The longest wait for a message's next
byte in this run was 15 ms, across 142 whole messages. See
[l3-over-everdrive-x7.md](../../docs/spec/l3-over-everdrive-x7.md) §4.5 item 7.

This run does not explain why the AP64 build tried on an X7 on 2026-10-07 never answered: that
ROM never answered `HELLO`, whose reply is one block, and here `HELLO` was answered in 22 ms.

**2026-10-08, SummerCart64, through the remote tester's bundle** (`cart-diagnostics-fe8ff00`;
`SCv2`, firmware 2.20 rev 2), ROM SHA-256
`58b347db7fbc97d21e00f1fc82a4444eeec9ece4d74ddf9916f0a587e7049126`, Windows 11 Pro 10.0.26300, FTDI
driver 2.12.36.20. `diagnose.bat` with no arguments: it found Multi64 running and waited for it to
be closed, then picked the SC64 and COM4 by USB IDs.

- **Phase 1, test ROM 1.14:** 34 passed, 0 failed, 3 skipped (HUD text, rumble, and the ROM
  version, which the script does not supply).
- **Phase 2, against the baseline:** 0 checks failed. Moderate-load traffic went Pass to Info, 1
  timeout in 35, inside the 0 to 2 the earlier runs saw. Every identify and buffer check passed.
- **Round trips:** with no load 60/117/481 ms (min/median/max), against a 67 ms median in the
  baseline; 79 and 109 ms medians under moderate and heavy load, against 67 and 64.

A second run the same day, with bundle `cart-diagnostics-3c9ffb0` (the one that went to the X7
tester) and the cart reporting firmware `2.20.2`, matched it: test ROM 34 passed, 0 failed, 3
skipped; bring-up 0 failed, with 1 timeout in 36 under moderate load and 2 in 35 under heavy. Its
round trips with no load were 62/81/144 ms, with medians of 81 and 88 ms under moderate and heavy
load. It also traced every cart read's bytes in hex, so it logged more than the first run did, yet
was faster. Logging is therefore not what made the first run's round trips slower; the two runs
differ by run-to-run spread, and every request with no load succeeded in both.

**2026-10-08, SummerCart64, the previous pin's build** (`SCv2`, firmware 2.20 rev 2), ROM SHA-256
`33a8e7ec7013b21cac1159971efb8d4113dcf74fe6ed8d5ac1f0d82dfdb6e17e`, Windows 11. This is the same
source as the run below, built by the toolchain [`toolchain.lock`](../toolchain.lock) pinned for a
few hours that day: libdragon's `c4a7e11` script, kept in the release
[`n64-toolchain-16.2.0-c4a7e119`](https://github.com/McAwful-Works/multi64/releases/tag/n64-toolchain-16.2.0-c4a7e119).
Only the compiler differs. Booted directly with `sc64-smoke --boot-rom`
([#368](https://github.com/McAwful-Works/multi64/pull/368)), then `multi64-test-connector bringup
--baseline baselines/sc64-2026-10-08.json`. **0 checks failed.**

- **Against the baseline:** the only change was moderate-load link traffic, Pass to Info, from 2
  timeouts where the baseline had 0. The run before the baseline also lost 2 of 35 there, so this
  is the run-to-run spread, not the compiler.
- **Identify and blocks:** every check passed. One `PI_STATUS` read costs 259 ns and one SC64
  `SR_CMD` read 3.64 µs (5.91 µs under load).
- **Link:** no load, 36 of 36, round trip 59/67/74 ms (min/median/max). Moderate load, 2 of 35
  timed out; heavy load, 3 of 36. The longest agent tick was 8.4 ms with no load and 38.5 ms under
  heavy load.
- **Interrupts:** the longest gap between 2 ms timer callbacks was 2.164 ms.

**2026-10-08, SummerCart64, ROM 1.0 and the baseline** (`SCv2`, firmware 2.20 rev 2),
ROM SHA-256 `58b347db7fbc97d21e00f1fc82a4444eeec9ece4d74ddf9916f0a587e7049126`. It was built with
libdragon's prebuilt toolchain, asset 564528689. The toolchain [`toolchain.lock`](../toolchain.lock)
pins now builds the same bytes. Windows 11, `multi64d` and `multi64-test-connector` from this branch. Booted by the cart's own bootloader, since
the cart's menu hung on every libdragon ROM that day (checklist step 3). Two runs; the second is the
committed baseline, [`baselines/sc64-2026-10-08.json`](baselines/sc64-2026-10-08.json).

- **Identify:** the SC64 driver's init answered; the PRO and X7 inits were tried first and did not.
- **Blocks:** every buffer test read back what it wrote, by CPU words and by PI DMA, with and
  without load. One `PI_STATUS` read costs 251 ns and one SC64 `SR_CMD` read 3.6 µs (5.9 µs under
  load), so `SC64_CMD_SPINS` (100,000) lets one wait run 361 ms, 590 ms under load: bounded, but
  some 20 frames.
- **Link:** no load, 36 of 36 requests, round trip 59–74 ms. Moderate load, 0 of 36 timed out in
  the second run and 2 of 35 in the first. Heavy load, 6 of 32 and 2 of 34. The longest agent tick
  was 8.7 ms with no load and 40 ms under heavy load, almost all of it in `sc64_write`.
- **Interrupts:** the longest gap between 2 ms timer callbacks was 2.17 ms, so the code under test
  held interrupts off for at most about 0.17 ms.

## Files

| File | What |
|---|---|
| `main.c` | Main loop, controls, screen |
| `stages.c`, `stages.h` | Identify, blocks, conditions |
| `load.c`, `load.h` | Background DMA and the interrupt-gap measurement |
| `wrap.c` | The `wrap_*` functions the variants call |
| `variants.h` | The prefixed entry points of the four builds |
| `report.h` | The report, checked against the spec's offsets at compile time |
| `spin_limits.sh` | Reads the drivers' spin limits for `build/spin_limits.h` |
| `multi64_bringup.z64` | The committed build |
| `baselines/` | Saved runs to compare against: `--baseline` or `bringup-compare` |
| `handover/` | The remote tester's bundle: `pack.sh` builds it, `diagnose.bat` and `diagnose.ps1` run it |
