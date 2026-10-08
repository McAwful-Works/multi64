#include "stages.h"

#include <libdragon.h>
#include <stdio.h>

#include "load.h"
#include "report.h"
#include "spin_limits.h"
#include "variants.h"

/* Registers the stages read directly. The drivers own the rest. */
#define ROM_BASE 0x10000000u
#define D64_REG_MAGIC 0x180002ECu
#define D64_MAGIC 0x55444556u /* 'UDEV' */
#define SC64_SR_CMD 0x1FFF0000u
#define SC64_DATA_0 0x1FFF0004u
#define SC64_DATA_1 0x1FFF0008u
#define SC64_IDENT 0x1FFF000Cu
#define SC64_BUFFER 0x1FFE0000u
#define ED_REG04 0x1F800004u /* X-series USBCFG, PRO FIFOSTAT */
#define ED_SYSSTAT 0x1F800008u
#define ED_REG14 0x1F800014u /* X-series VERSION, PRO EDID */
#define ED_USBDAT 0x1F800400u
#define BR_PI_STATUS (*(volatile uint32_t *)0xA4600010u)

#define EDX_VERSION 0xED640013u
#define ED3_VERSION 0xED640008u

#define ED_USBMODE_RDNOP 0xC400u
#define ED_USBMODE_WRNOP 0xC000u

#define SC64_CMD_BUSY (1u << 31)
#define SC64_CMD_ERROR (1u << 30)
#define SC64_CMD_CONFIG_SET 'C'
#define SC64_CFG_ROM_WRITE_ENABLE 1u

static uint32_t s_runs;

static int probe(uint32_t addr, uint32_t *value, uint32_t bit)
{
    if (pio_pi_io_read(addr, value)) {
        g_report.probe_ok |= bit;
        return 1;
    }
    *value = 0u;
    return 0;
}

void stage_identify(void)
{
    struct report *r = &g_report;
    uint32_t tried = 0u;
    uint32_t ok = 0u;

    /* Reads only, before anything is unlocked. Each step names itself first: during boot the
       console shows each line as it is printed, so a hang leaves its step on screen. */
    printf("identify: raw reads\n");
    if (pio_pi_io_load_words(r->rom_header, ROM_BASE, 16u)) {
        r->probe_ok |= PROBE_ROM_HEADER;
    }
    (void)probe(D64_REG_MAGIC, &r->d64_magic, PROBE_D64_MAGIC);
    (void)probe(SC64_IDENT, &r->sc64_ident_locked, PROBE_SC64_IDENT);
    (void)probe(ED_REG14, &r->ed_reg14_locked, PROBE_ED_REG14);
    (void)probe(ED_REG04, &r->ed_reg04_locked, PROBE_ED_REG04);
    if (probe(ED_SYSSTAT, &r->pro_sysstat[0], PROBE_PRO_SYSSTAT)) {
        (void)probe(ED_SYSSTAT, &r->pro_sysstat[1], PROBE_PRO_SYSSTAT);
    }

    /* Then each driver's own init, in the order libdragon and the test ROM probe: a 64drive first,
       and nothing written to it; the PRO, whose check only reads until it has seen a PRO; the
       X-series key; the SC64 unlock last. Each stops the search when it answers, so no cart is sent
       another cart's unlock once identified. */
    if (r->d64_magic == D64_MAGIC) {
        r->cart = CART_OTHER;
    } else {
        tried |= 1u << VARIANT_PRO;
        printf("identify: PRO init\n");
        if (bpro_ed64pro_init()) {
            ok |= 1u << VARIANT_PRO;
            r->cart = CART_PRO;
        } else {
            tried |= 1u << VARIANT_X7_IO;
            printf("identify: X7 init\n");
            if (bx7_ed64_init()) {
                ok |= 1u << VARIANT_X7_IO;
            }
            (void)pio_pi_io_read(ED_REG14, &r->ed_reg14_unlocked);
            (void)pio_pi_io_read(ED_REG04, &r->ed_usbcfg_after);
            if (ok & (1u << VARIANT_X7_IO)) {
                r->cart = CART_X_SERIES;
            } else if (r->ed_reg14_unlocked == EDX_VERSION || r->ed_reg14_unlocked == ED3_VERSION) {
                /* An X-series whose USB unit reads as off (an X5, or no cable): libdragon stops
                   here too rather than try the SC64 unlock on it. */
                r->cart = CART_X_SERIES;
            } else {
                tried |= 1u << VARIANT_SC64;
                printf("identify: SC64 init\n");
                if (bsc_sc64_init()) {
                    ok |= 1u << VARIANT_SC64;
                    r->cart = CART_SC64;
                }
                (void)pio_pi_io_read(SC64_IDENT, &r->sc64_ident_unlocked);
            }
        }
    }
    r->init_tried = tried;
    r->init_ok = ok;
    r->stages |= STAGE_IDENTIFY;
}

uint32_t default_variant(uint32_t cart)
{
    switch (cart) {
    case CART_SC64:
        return VARIANT_SC64;
    case CART_X_SERIES:
        return VARIANT_X7_IO;
    case CART_PRO:
        return VARIANT_PRO;
    default:
        return VARIANT_NONE;
    }
}

int variant_fits(uint32_t variant, uint32_t cart)
{
    if (cart == CART_X_SERIES) {
        return variant == VARIANT_X7_IO || variant == VARIANT_X7_DMA;
    }
    return variant != VARIANT_NONE && variant == default_variant(cart);
}

/* ---- stage 2: register read costs ---------------------------------------------------------- */

static void time_pi_status(struct timing *t)
{
    uint32_t i;

    t->id = TIMING_PI_STATUS;
    t->spins = SPIN_PI_WAIT;
    t->ticks_total = 0u;
    t->ticks_max = 0u;
    for (i = 0u; i < TIMING_READS; i++) {
        uint32_t t0 = C0_COUNT();
        (void)BR_PI_STATUS;
        uint32_t dt = C0_COUNT() - t0;
        t->ticks_total += dt;
        if (dt > t->ticks_max) {
            t->ticks_max = dt;
        }
    }
}

/* What one spin of a driver's wait costs: each spin is one pi_io_read of this register. */
static void time_register(struct timing *t, uint32_t id, uint32_t addr, uint32_t spins)
{
    uint32_t i;
    uint32_t v;

    t->id = id;
    t->spins = spins;
    t->ticks_total = 0u;
    t->ticks_max = 0u;
    for (i = 0u; i < TIMING_READS; i++) {
        uint32_t t0 = C0_COUNT();
        (void)pio_pi_io_read(addr, &v);
        uint32_t dt = C0_COUNT() - t0;
        t->ticks_total += dt;
        if (dt > t->ticks_max) {
            t->ticks_max = dt;
        }
    }
}

static void timings(unsigned slot)
{
    struct timing *t = g_report.timing[slot];
    unsigned i;

    for (i = 0u; i < TIMING_SLOTS; i++) {
        t[i].id = 0u;
    }
    printf("blocks %u: timing\n", slot);
    time_pi_status(&t[0]);
    switch (g_report.cart) {
    case CART_X_SERIES:
        time_register(&t[1], TIMING_X7_USBCFG, ED_REG04, SPIN_ED_BUSY);
        break;
    case CART_SC64:
        time_register(&t[1], TIMING_SC64_SR_CMD, SC64_SR_CMD, SPIN_SC64_CMD);
        break;
    case CART_PRO:
        time_register(&t[1], TIMING_PRO_SYSSTAT, ED_SYSSTAT, SPIN_PRO_MCU);
        time_register(&t[2], TIMING_PRO_FIFOSTAT, ED_REG04, SPIN_PRO_STATUS);
        break;
    default:
        break;
    }
}

/* ---- stage 2: buffer moves ----------------------------------------------------------------- */

/* One SC64 command, through pio_. The stage needs write-enable around its SC64 buffer test, as
   sc64_write has around staging; the driver's own command helper is private to sc64.c. */
static int sc64_command(uint8_t cmd, uint32_t arg0, uint32_t arg1, uint32_t *result1)
{
    uint32_t sr = SC64_CMD_BUSY;
    uint32_t spins;

    if (!pio_pi_io_write(SC64_DATA_0, arg0) || !pio_pi_io_write(SC64_DATA_1, arg1) ||
        !pio_pi_io_write(SC64_SR_CMD, cmd)) {
        return 0;
    }
    for (spins = 0u; spins < SPIN_SC64_CMD && (sr & SC64_CMD_BUSY); spins++) {
        if (!pio_pi_io_read(SC64_SR_CMD, &sr)) {
            return 0;
        }
    }
    if (sr & (SC64_CMD_BUSY | SC64_CMD_ERROR)) {
        return 0;
    }
    return result1 == 0 || pio_pi_io_read(SC64_DATA_1, result1);
}

static uint32_t pattern(uint32_t run, unsigned slot, unsigned combo, uint32_t i)
{
    /* Different in every word, combo and run, so a read that returns an earlier test's bytes, or
       the same word twice, cannot pass. */
    return (0xA5000000u ^ (run << 16) ^ ((uint32_t)slot << 12) ^ ((uint32_t)combo << 8)) +
           i * 0x01030507u;
}

static void prepare(int for_write)
{
    if (g_report.cart == CART_X_SERIES) {
        (void)pio_pi_io_write(ED_REG04, for_write ? ED_USBMODE_WRNOP : ED_USBMODE_RDNOP);
    }
}

static void buffer_tests(unsigned slot)
{
    struct report *r = &g_report;
    static uint32_t src[BUF_BYTES / 4u];
    static uint32_t dst[BUF_BYTES / 4u];
    uint32_t addr;
    uint32_t restore = 0u;
    int sc64_writable = 0;
    unsigned combo;

    switch (r->cart) {
    case CART_X_SERIES:
        addr = ED_USBDAT;
        break;
    case CART_SC64:
        addr = SC64_BUFFER;
        sc64_writable = sc64_command(SC64_CMD_CONFIG_SET, SC64_CFG_ROM_WRITE_ENABLE, 1u, &restore);
        break;
    default:
        addr = 0u;
        break;
    }
    r->buf_addr = addr;

    for (combo = 0u; combo < BUF_COMBOS; combo++) {
        struct buffer_test *b = &r->buffer[slot][combo];
        int dma_write = combo == BUF_DMA_IO || combo == BUF_DMA_DMA;
        int dma_read = combo == BUF_IO_DMA || combo == BUF_DMA_DMA;
        uint32_t i;
        uint32_t t0;
        int ok;

        b->first_bad = 0xFFFFFFFFu;
        b->bad_bytes = 0u;
        b->ticks_write = 0u;
        b->ticks_read = 0u;
        if (addr == 0u) {
            b->result = BUF_NOT_APPLICABLE;
            continue;
        }
        printf("blocks %u: buffer %u\n", slot, combo);
        for (i = 0u; i < BUF_BYTES / 4u; i++) {
            src[i] = pattern(s_runs, slot, combo, i);
            dst[i] = ~src[i];
        }

        prepare(1);
        t0 = C0_COUNT();
        ok = dma_write ? pdma_pi_io_store_words(src, addr, BUF_BYTES / 4u)
                       : pio_pi_io_store_words(src, addr, BUF_BYTES / 4u);
        b->ticks_write = C0_COUNT() - t0;
        if (!ok) {
            b->result = BUF_WRITE_FAILED;
            continue;
        }

        prepare(0);
        t0 = C0_COUNT();
        ok = dma_read ? pdma_pi_io_load_words(dst, addr, BUF_BYTES / 4u)
                      : pio_pi_io_load_words(dst, addr, BUF_BYTES / 4u);
        b->ticks_read = C0_COUNT() - t0;
        if (!ok) {
            b->result = BUF_READ_FAILED;
            continue;
        }

        for (i = 0u; i < BUF_BYTES; i++) {
            if (((const uint8_t *)src)[i] != ((const uint8_t *)dst)[i]) {
                if (b->first_bad == 0xFFFFFFFFu) {
                    b->first_bad = i;
                }
                b->bad_bytes++;
            }
        }
        b->result = b->bad_bytes == 0u ? BUF_MATCH : BUF_MISMATCH;
    }

    if (sc64_writable) {
        (void)sc64_command(SC64_CMD_CONFIG_SET, SC64_CFG_ROM_WRITE_ENABLE, restore, 0);
    }
}

void stage_blocks(unsigned slot)
{
    s_runs++;
    timings(slot);
    buffer_tests(slot);
    if (slot == 0u) {
        g_report.stages |= STAGE_BLOCKS;
    }
}

void stage_conditions(void)
{
    uint32_t was = load_level();

    printf("conditions: load on\n");
    load_set(LOAD_MODERATE);
    g_report.cond_load = LOAD_MODERATE;
    /* Let the load get going before measuring under it. */
    wait_ms(20);
    stage_blocks(1u);
    load_set(was);
    g_report.stages |= STAGE_CONDITIONS;
}
