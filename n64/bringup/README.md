# Cart bring-up ROM

> **Report format:** [cart-bringup-report-v0.md](../../docs/spec/cart-bringup-report-v0.md) · **Host tool:** [`multi64-test-connector bringup`](../../docs/connectors/test-rom.md#cart-bring-up) · **Agent:** [n64/agent](../agent/README.md)

`multi64_bringup.z64` runs the cart agent's **own drivers** on a cart, outside any game, and
reports what they do: on screen, and in a report a host reads through the agent itself. It exists
because the agent's EverDrive drivers are written against libdragon's and Krikzz's code but have
not been shown to work on a cart (one X7 try inside a game got no answer from the ROM), and an
agent that stays silent inside a game has no way to say why.

Run it on a **SummerCart64 first**. The SC64 driver is proven inside games, so its report is
the baseline: on another cart, the first result that differs from the SC64's is where to look.

**Run on one cart: a SummerCart64,** on 2026-10-08, where every check passed
([Hardware record](#hardware-record)). That run is the baseline. It has not run on an EverDrive.

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
agent sources from `../agent` itself. The committed `multi64_bringup.z64` was built with libdragon
`c4a7e11` and mips64-elf GCC 16.2.0 from toolchain asset **564528689** (SHA-256
`fac8e6572493a66468b7d45df41b042f90b1a630ec96aa218bd5fb1f320a0a4d`). That is not the asset
[`toolchain.lock`](../toolchain.lock) pins: libdragon's rolling release replaced it on 2026-09-15,
and the pinned one no longer downloads. Two builds in a row from the same toolchain give the same
bytes: SHA-256 `58b347db7fbc97d21e00f1fc82a4444eeec9ece4d74ddf9916f0a587e7049126`.

## Reading the screen

The screen shows everything that does not need a link. Photograph it before running anything.

On the way up, each start-up and stage step prints a line as it begins (`boot: timer`,
`identify: X7 init`, `blocks 0: buffer 2` and so on), so a ROM that hangs leaves the step it hung
in as its last line. A screen that stays black, with not even the first line, never reached the
ROM's code. Once every stage has run, the screen below replaces those lines.

```
multi64 bring-up 1.0  cart SC64  link SC64  load off  f1234
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
4. Photograph the screen.
5. Run:

   ```sh
   target/release/multi64-test-connector bringup --out bringup-sc64-baseline.json
   ```

6. What a working SC64 shows: `identify.cart` and `identify.driver_init` PASS; all eight
   `blocks.buffer.*` checks PASS; `link.default.load_off.traffic` PASS. Under moderate and heavy
   load a few timeouts are normal, because the agent gives up on a busy PI by design rather than
   stall the game; they are reported as INFO with their count, which varies from run to run.
7. Keep the JSON and the photo. They are what the X7 run is compared against. The 2026-10-08
   baseline is committed as [`baselines/sc64-2026-10-08.json`](baselines/sc64-2026-10-08.json).

Anything else on the SC64 is a fault in this ROM or the tool, not in the cart, and has to be
fixed before the ROM goes to anyone.

### 2. X7 handover

Send four files:

1. the **Multi64 installer**, for `multi64d`;
2. `multi64_bringup.z64`;
3. `multi64-test-connector.exe` (from step 1 above);
4. the SC64 baseline, [`baselines/sc64-2026-10-08.json`](baselines/sc64-2026-10-08.json).

The tester:

1. Installs Multi64, sets Settings → **Cart** to EverDrive X7 (or Auto-detect), and starts the
   bridge.
2. Copies `multi64_bringup.z64` to the SD card and boots it from the EverDrive menu.
3. Photographs the screen before anything else.
4. Runs `multi64-test-connector.exe bringup --baseline sc64-2026-10-08.json`. It takes a few
   minutes: three load levels through the CPU-word build, then the same through the DMA build.
5. If it reports `link.hello` FAIL (no HELLO_ACK), presses **R** once (the top line changes to
   `link X7 DMA`) and runs the same command again with `--out bringup-x7-dma.json`.
6. Sends back the photo(s), every JSON file written, and the console output.

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

**2026-10-08, SummerCart64** (`SCv2`, firmware 2.20 rev 2), ROM SHA-256 `58b347db…`, Windows 11,
`multi64d` and `multi64-test-connector` from this branch. Booted by the cart's own bootloader, since
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
