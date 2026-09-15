#include "sc64.h"

/* ---- cart address space ------------------------------------------------- */

#define SC64_BASE 0x10000000u
#define SC64_REGS 0x1FFF0000u

#define REG_SR_CMD (SC64_REGS + 0x00u)
#define REG_DATA_0 (SC64_REGS + 0x04u)
#define REG_DATA_1 (SC64_REGS + 0x08u)
#define REG_IDENT (SC64_REGS + 0x0Cu)
#define REG_KEY (SC64_REGS + 0x10u)

#define SR_CMD_ERROR (1u << 30)
#define SR_CMD_BUSY (1u << 31)

#define IDENT_V2 0x53437632u /* 'SCv2' */

#define KEY_RESET 0x00000000u
#define KEY_UNLOCK_1 0x5F554E4Cu
#define KEY_UNLOCK_2 0x4F434B5Fu

#define CMD_CONFIG_SET 'C'
#define CMD_USB_WRITE_STATUS 'U'
#define CMD_USB_WRITE 'M'
#define CMD_USB_READ_STATUS 'u'
#define CMD_USB_READ 'm'

#define USB_WRITE_BUSY (1u << 31)
#define USB_READ_BUSY (1u << 31)

#define CFG_ROM_WRITE_ENABLE 1u

/*
 * Staging area: the SC64's dedicated 8 KiB data buffer.
 *
 * The cart transfers to and from its own memory, never RDRAM, so every packet is
 * staged first. libdragon stages into the top of the 64 MiB ROM window because it
 * has to work on 64drive and EverDrive too -- but that window is SDRAM, and a
 * running ROM large enough reaches into it. Staging there would land inside the
 * game.
 *
 * The SC64 has a separate buffer in BlockRAM for exactly this, at PI address
 * 0x1FFE_0000 (SummerCart64 docs, PI memory map). It is not SDRAM, so no ROM of any
 * size can collide with it, and it is always mapped once register access is
 * unlocked. USB_READ/USB_WRITE take a pi_address, so this address is usable
 * directly, and nothing here depends on the size of the ROM.
 *
 * Staging here once looked like it needed no ROM_WRITE_ENABLE toggle. Observed on
 * hardware: reads from this buffer land, writes do not, and the cart then
 * retransmits whatever the previous USB_READ left there. So the toggle is kept,
 * around the staging write only. See sc64_write().
 *
 * 8 KiB is the hard ceiling on one packet. M64P's limits were chosen so a whole
 * frame fits: 8152 bytes worst case.
 */
#define SC64_BUFFER 0x1FFE0000u
#define SC64_BUFFER_SIZE 8192u

/* ---- PI access ----------------------------------------------------------
 *
 * The agent must not disturb the game's own PI traffic. A commercial game usually
 * DMAs from ROM constantly, through libultra's PI manager, on a thread of its own.
 *
 * An injected agent normally cannot join that. The game's own ROM-loading routines
 * take ROM offsets (or a virtual ROM through a filesystem table) and cannot reach
 * the cart's register window, and a ROM without a symbol source gives no addresses
 * for libultra's PI entry points.
 *
 * So two rules, which together make direct access safe enough to reason about:
 *
 *   1. NEVER write the PI control registers (DRAM_ADDR, CART_ADDR, RD_LEN,
 *      WR_LEN). Those are what a DMA in progress is using; clobbering them
 *      mid-setup corrupts the game's transfer, and that is the failure mode worth
 *      designing out. Data moves by CPU load/store through the uncached window
 *      instead of by DMA, so the agent owns no PI state.
 *   2. Mask interrupts across each access, having first checked the PI is idle.
 *      With interrupts off no thread switch can occur, so the DMA manager cannot
 *      start a transfer underneath us.
 *
 * Bulk copies are chunked so interrupts are never masked for long: an 8 KiB
 * transfer in one masked span would be milliseconds of latency and would show up
 * as audio and video glitches.
 *
 * The complete fix is still to hold the PI the way libultra does
 * (osPiGetAccess/osPiRelease, or the non-raw osPiReadIo/osPiWriteIo which acquire
 * it internally). That needs the host game's addresses for those functions. See
 * docs/integration/cart-agent.md.
 */

#define PI_BASE 0xA4600000u
#define PI_STATUS (PI_BASE + 0x10u)

#define PI_STATUS_DMA_BUSY (1u << 0)
#define PI_STATUS_IO_BUSY (1u << 1)

/** Words moved per masked span. 64 words is a few microseconds of latency. */
#define PI_CHUNK_WORDS 64u

#ifdef SC64_HOST_TEST
/* make host-test: tests/sc64_test.c stands in for the PI bus and the interrupt mask. The N64 build
   never defines this, and compiles exactly the uncached loads and stores below. */
uint32_t sc64_test_pi_load(uint32_t addr);
void sc64_test_pi_store(uint32_t addr, uint32_t value);
uint32_t sc64_test_int_mask(void);
void sc64_test_int_restore(uint32_t sr);
#define PI_LOAD(addr) sc64_test_pi_load(addr)
#define PI_STORE(addr, value) sc64_test_pi_store((addr), (value))
#define int_mask() sc64_test_int_mask()
#define int_restore(sr) sc64_test_int_restore(sr)
#else
/* Uncached (KSEG1) view: cart and register access must never be cached. */
static volatile uint32_t *io(uint32_t phys)
{
    return (volatile uint32_t *)(0xA0000000u | (phys & 0x1FFFFFFFu));
}

#define PI_LOAD(addr) (*io(addr))
#define PI_STORE(addr, value) (*io(addr) = (value))

/** Clear the CP0 interrupt-enable bit, returning the previous Status. */
static uint32_t int_mask(void)
{
    uint32_t sr;
    __asm__ __volatile__("mfc0 %0, $12" : "=r"(sr));
    __asm__ __volatile__("mtc0 %0, $12" : : "r"(sr & ~1u));
    __asm__ __volatile__("nop; nop; nop"); /* CP0 hazard */
    return sr;
}

static void int_restore(uint32_t sr)
{
    __asm__ __volatile__("mtc0 %0, $12" : : "r"(sr));
    __asm__ __volatile__("nop; nop; nop");
}
#endif

/**
 * Spin until the PI is idle, or give up.
 *
 * This once spun without a bound, and the second call in `io_read` / `io_write`
 * runs with interrupts masked -- so a PI that stayed busy hung the console with no
 * way back. That is not hypothetical: it hard locked a game at room loads, where the
 * game's own DMA holds the bus longest.
 *
 * The cap is deliberately generous. A legitimate wait here is microseconds; a
 * DMA of a whole room is far shorter than this. Anything longer is a fault, and
 * returning a failure that the caller reports as "no cart" is always better than
 * freezing the machine.
 */
#define PI_WAIT_SPINS 100000u

static int pi_wait_clear(uint32_t bits)
{
    uint32_t spins = PI_WAIT_SPINS;
    while (PI_LOAD(PI_STATUS) & bits) {
        if (--spins == 0u) {
            return 0;
        }
    }
    return 1;
}

static int pi_wait_idle(void)
{
    return pi_wait_clear(PI_STATUS_DMA_BUSY | PI_STATUS_IO_BUSY);
}

/*
 * One register access with the PI idle and interrupts masked. Both return 0 if the PI stayed busy,
 * having touched nothing. This once reported nothing: a store that never happened looked like one
 * that did, and a failed load read as 0 -- "not busy, no error" to sc64_cmd (#153).
 */
static int io_read(uint32_t addr, uint32_t *value)
{
    uint32_t sr;
    if (!pi_wait_idle()) {
        return 0;
    }
    sr = int_mask();
    if (!pi_wait_idle()) {
        int_restore(sr);
        return 0;
    }
    *value = PI_LOAD(addr);
    int_restore(sr);
    return 1;
}

static int io_write(uint32_t addr, uint32_t value)
{
    uint32_t sr;
    if (!pi_wait_idle()) {
        return 0;
    }
    sr = int_mask();
    if (!pi_wait_idle()) {
        int_restore(sr);
        return 0;
    }
    PI_STORE(addr, value);
    int_restore(sr);
    return 1;
}

/**
 * Copy words between RDRAM and the cart, by CPU rather than DMA.
 *
 * `len` is rounded up to a word; callers size their buffers with that slack. Both
 * directions go through the uncached window, so no cache maintenance is needed and
 * no stale line can be left behind.
 *
 * Returns 1, or 0 if the PI stayed busy, in which case some words may have moved and interrupts are
 * as they were. Every wait is bounded: this once ignored pi_wait_idle() failing and spun on IO_BUSY
 * with no limit and interrupts masked, the hang the comment on PI_WAIT_SPINS describes (#152).
 */
static int pi_copy(void *ram, uint32_t cart_addr, uint32_t len, int to_cart)
{
    uint32_t words = (len + 3u) / 4u;
    uint32_t *r = (uint32_t *)ram;
    uint32_t done = 0u;

    while (done < words) {
        uint32_t n = words - done;
        uint32_t sr;
        uint32_t i;

        if (n > PI_CHUNK_WORDS) {
            n = PI_CHUNK_WORDS;
        }

        if (!pi_wait_idle()) {
            return 0;
        }
        sr = int_mask();
        if (!pi_wait_idle()) {
            int_restore(sr);
            return 0;
        }
        if (to_cart) {
            for (i = 0u; i < n; i++) {
                /*
                 * Stores to PI space are posted, so back-to-back writes overrun
                 * the PI's write path and are silently dropped -- which is what
                 * staging did before this wait existed. Loads stall the CPU until
                 * data returns, which is why the read direction never needed it
                 * and why the failure looked like "reads work, writes vanish".
                 *
                 * Only IO_BUSY is polled: a DMA cannot start inside the masked
                 * span, and pi_wait_idle() above already cleared DMA_BUSY.
                 */
                if (!pi_wait_clear(PI_STATUS_IO_BUSY)) {
                    int_restore(sr);
                    return 0;
                }
                PI_STORE(cart_addr + (done + i) * 4u, r[done + i]);
            }
            if (!pi_wait_clear(PI_STATUS_IO_BUSY)) {
                int_restore(sr);
                return 0;
            }
        } else {
            for (i = 0u; i < n; i++) {
                r[done + i] = PI_LOAD(cart_addr + (done + i) * 4u);
            }
        }
        int_restore(sr);

        done += n;
    }
    return 1;
}

/* ---- command interface -------------------------------------------------- */

/**
 * Status reads while a command is in progress, and USB_READ_STATUS polls while a read is staged,
 * before giving up. Generous, like PI_WAIT_SPINS: both loops once had no bound at all (#153).
 */
#define SC64_CMD_SPINS 100000u

/** Run one command. Returns 0 if the cart reported an error or any register access failed. */
static int sc64_cmd(uint8_t cmd, const uint32_t *args, uint32_t *result)
{
    uint32_t sr;
    uint32_t spins;

    if (args != 0) {
        if (!io_write(REG_DATA_0, args[0]) || !io_write(REG_DATA_1, args[1])) {
            return 0;
        }
    }
    if (!io_write(REG_SR_CMD, cmd)) {
        return 0;
    }

    for (spins = 0u;; spins++) {
        if (spins >= SC64_CMD_SPINS || !io_read(REG_SR_CMD, &sr)) {
            return 0;
        }
        if (!(sr & SR_CMD_BUSY)) {
            break;
        }
    }

    if (result != 0) {
        if (!io_read(REG_DATA_0, &result[0]) || !io_read(REG_DATA_1, &result[1])) {
            return 0;
        }
    }
    return (sr & SR_CMD_ERROR) ? 0 : 1;
}

/**
 * Set cart write-enable, writing the previous setting through `previous` so it can be
 * restored exactly. CONFIG_SET reports the old value in result[1]. Returns 0 on failure.
 */
static int set_rom_writable(uint32_t enable, uint32_t *previous)
{
    uint32_t args[2];
    uint32_t result[2];

    args[0] = CFG_ROM_WRITE_ENABLE;
    args[1] = enable;
    if (!sc64_cmd(CMD_CONFIG_SET, args, result)) {
        return 0;
    }
    *previous = result[1];
    return 1;
}

/*
 * What write-enable goes back to after sc64_write stages a packet, and whether putting it back
 * failed. A restore that does not land leaves the cart writable under the running game (#153), so
 * it is retried by every sc64_poll and sc64_write until it does. While one is pending, the old value
 * CONFIG_SET reports is the agent's own leftover and is not taken as the value to restore.
 */
static uint32_t s_rom_write_restore;
static int s_rom_write_restore_pending;

static void rom_write_restore(void)
{
    uint32_t ignored;
    s_rom_write_restore_pending = !set_rom_writable(s_rom_write_restore, &ignored);
}

int sc64_init(void)
{
    uint32_t ident;

    if (!io_write(REG_KEY, KEY_RESET) || !io_write(REG_KEY, KEY_UNLOCK_1) || !io_write(REG_KEY, KEY_UNLOCK_2)) {
        return 0;
    }
    return io_read(REG_IDENT, &ident) && ident == IDENT_V2;
}

uint32_t sc64_poll(uint8_t *datatype)
{
    uint32_t args[2];
    uint32_t result[2];
    uint32_t size;
    uint32_t spins;

    if (s_rom_write_restore_pending) {
        rom_write_restore();
    }

    if (!sc64_cmd(CMD_USB_READ_STATUS, 0, result)) {
        return 0;
    }
    size = result[1] & 0x00FFFFFFu;
    if (size == 0u) {
        return 0; /* the usual case: nothing from the host this frame */
    }
    if (datatype != 0) {
        *datatype = (uint8_t)(result[0] & 0xFFu);
    }
    if (size > SC64_BUFFER_SIZE) {
        return 0; /* refuse rather than overrun the staging area */
    }

    args[0] = SC64_BUFFER;
    args[1] = size;
    if (!sc64_cmd(CMD_USB_READ, args, 0)) {
        return 0;
    }
    for (spins = 0u;; spins++) {
        if (spins >= SC64_CMD_SPINS || !sc64_cmd(CMD_USB_READ_STATUS, 0, result)) {
            return 0;
        }
        if (!(result[0] & USB_READ_BUSY)) {
            break;
        }
    }

    return size;
}

int sc64_read(void *dst, uint32_t offset, uint32_t len)
{
    /* PI DMA needs even lengths; round up into the caller's buffer, which the
       agent sizes with that slack. */
    return pi_copy(dst, SC64_BUFFER + offset, len, 0);
}

int sc64_write(uint8_t datatype, const void *data, uint32_t len)
{
    uint32_t args[2];
    uint32_t result[2];
    uint32_t previous;
    int staged = 0;
    uint32_t spins;

    if (len == 0u || len > SC64_BUFFER_SIZE) {
        return 0;
    }

    /* A still-running previous transfer means the host is not draining. Drop
       rather than block a game frame waiting for it. */
    if (!sc64_cmd(CMD_USB_WRITE_STATUS, 0, result)) {
        return 0;
    }
    if (result[0] & USB_WRITE_BUSY) {
        return 0;
    }

    /*
     * Write-enable spans the staging copy only -- microseconds, with interrupts
     * masked in 64-word chunks inside pi_copy -- so the cartridge is never left
     * writable underneath the running game. If the enable fails nothing is staged; if the
     * restore fails it stays pending for the next call (see s_rom_write_restore).
     */
    if (set_rom_writable(1u, &previous)) {
        if (!s_rom_write_restore_pending) {
            s_rom_write_restore = previous;
        }
        staged = pi_copy((void *)data, SC64_BUFFER, len, 1);
    }
    rom_write_restore();
    if (!staged) {
        return 0;
    }

    /*
     * Read back the ends of what we just staged. If a store was dropped the cart
     * still holds the previous request, and USB_WRITE would send that back as a
     * well-formed frame carrying the wrong message -- the exact symptom that hid
     * this bug. Dropping the response instead turns that into a clean timeout.
     */
    {
        uint32_t first = 0u;
        uint32_t last = 0u;
        uint32_t tail = ((len + 3u) / 4u - 1u) * 4u;
        const uint8_t *src = (const uint8_t *)data;

        if (!pi_copy(&first, SC64_BUFFER, 4u, 0) || !pi_copy(&last, SC64_BUFFER + tail, 4u, 0)) {
            return 0;
        }
        if (first != *(const uint32_t *)src) {
            return 0;
        }
        if (len >= 8u && last != *(const uint32_t *)(src + tail)) {
            return 0;
        }
    }

    args[0] = SC64_BUFFER;
    args[1] = ((uint32_t)datatype << 24) | (len & 0x00FFFFFFu);
    if (!sc64_cmd(CMD_USB_WRITE, args, 0)) {
        return 0;
    }

    /* Bounded wait: a game frame is 16.7 ms and must not be held hostage by a
       host that has stopped reading. */
    for (spins = 0u; spins < 100000u; spins++) {
        if (!sc64_cmd(CMD_USB_WRITE_STATUS, 0, result)) {
            return 0;
        }
        if (!(result[0] & USB_WRITE_BUSY)) {
            return 1;
        }
    }
    return 0;
}
