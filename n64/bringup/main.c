/**
 * multi64 cart bring-up ROM: runs the cart agent's own drivers on a cart and reports what they do.
 *
 * Stages (n64/bringup/README.md, docs/spec/cart-bringup-report-v0.md):
 *   1 identify    raw ID reads before any unlock, then each driver's own init until one answers
 *   2 blocks      register read costs against the drivers' spin limits; a 512-byte buffer written
 *                 and read back by CPU words and by PI DMA, all four ways
 *   3 link        the selected agent build's agent_tick() once a frame, for a host to talk to
 *   4 conditions  stage 2 again under background ROM DMA, and the link under it on request
 *
 * Everything lands in g_report, which a host reads through the agent itself with M64P PEEKV, and
 * the essentials are on screen for when nothing reaches the PC.
 *
 * Controls: R switches an X7 between its IO and DMA builds; Z steps the load off, moderate, heavy;
 * A runs stages 2 and 4 again.
 */
#include <libdragon.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "load.h"
#include "report.h"
#include "stages.h"
#include "variants.h"

struct report g_report __attribute__((aligned(16)));

static uint32_t s_variant;
/* The control words as last seen, so a host's change is acted on once and a button press is not
   undone by the host's older value on the next frame. */
static uint32_t s_seen_ctl_variant;
static uint32_t s_seen_ctl_load;

static void report_init(void)
{
    struct report *r = &g_report;

    memset(r, 0, sizeof *r);
    r->magic[0] = 0x43415254u; /* "CART" */
    r->magic[1] = 0x42525550u; /* "BRUP" */
    r->format = REPORT_FORMAT;
    r->size = sizeof *r;
    r->self = (uint32_t)r & 0x1FFFFFFFu;
    r->rom_version = BRINGUP_ROM_VERSION;
    r->count_hz = TICKS_PER_SECOND;
    r->mem_size = (uint32_t)get_memory_size();
}

static void reset_maxima(void)
{
    unsigned i;

    for (i = 0u; i < VARIANT_COUNT; i++) {
        g_report.link[i].recv_ticks_max = 0u;
        g_report.link[i].send_ticks_max = 0u;
        g_report.link[i].tick_ticks_max = 0u;
    }
}

static void select_variant(uint32_t v)
{
    if (v == s_variant || !variant_fits(v, g_report.cart)) {
        return;
    }
    s_variant = v;
    g_report.variant = v;
    reset_maxima();
}

static void select_load(uint32_t level)
{
    if (level >= LOAD_LEVELS || level == load_level()) {
        return;
    }
    load_set(level);
    reset_maxima();
}

static void rerun(void)
{
    stage_blocks(0u);
    stage_conditions();
}

static void controls(joypad_buttons_t pressed)
{
    struct report *r = &g_report;

    if (r->ctl_variant != s_seen_ctl_variant) {
        s_seen_ctl_variant = r->ctl_variant;
        select_variant(r->ctl_variant);
    }
    if (r->ctl_load != s_seen_ctl_load) {
        s_seen_ctl_load = r->ctl_load;
        select_load(r->ctl_load);
    }
    if (r->ctl_rerun != r->rerun_done) {
        rerun();
        r->rerun_done = r->ctl_rerun;
    }

    if (pressed.r && r->cart == CART_X_SERIES) {
        select_variant(s_variant == VARIANT_X7_IO ? VARIANT_X7_DMA : VARIANT_X7_IO);
    }
    if (pressed.z) {
        select_load((load_level() + 1u) % LOAD_LEVELS);
    }
    if (pressed.a) {
        rerun();
    }
}

static void tick_variant(void)
{
    const struct variant_ops *ops;
    struct link_stats *l;
    uint32_t t0;
    uint32_t dt;

    if (s_variant == VARIANT_NONE) {
        return;
    }
    ops = &g_variants[s_variant - 1u];
    l = &g_report.link[s_variant - 1u];

    t0 = C0_COUNT();
    ops->tick();
    dt = C0_COUNT() - t0;
    if (dt > l->tick_ticks_max) {
        l->tick_ticks_max = dt;
    }
    l->tick_ticks_total += dt;

    l->agent_ticks = ops->ticks();
    l->agent_frames = ops->frames();
    l->agent_ready = (uint32_t)ops->ready();
    l->m64p_requests = ops->requests();
    l->m64p_errors = ops->errors();
    l->m64p_last_error = ops->last_error();
}

/* ---- screen ------------------------------------------------------------------------------ */

static const char *cart_name(uint32_t cart)
{
    switch (cart) {
    case CART_SC64:
        return "SC64";
    case CART_X_SERIES:
        return "X7/3.0";
    case CART_PRO:
        return "PRO";
    case CART_OTHER:
        return "64drive?";
    default:
        return "none";
    }
}

static const char *load_name(uint32_t level)
{
    return level == LOAD_HEAVY ? "heavy" : level == LOAD_MODERATE ? "moderate" : "off";
}

static uint32_t ticks_us(uint32_t ticks)
{
    return (uint32_t)((uint64_t)ticks * 1000000u / TICKS_PER_SECOND);
}

static const char *timing_name(uint32_t id)
{
    switch (id) {
    case TIMING_PI_STATUS:
        return "PI_STAT";
    case TIMING_X7_USBCFG:
        return "USBCFG";
    case TIMING_SC64_SR_CMD:
        return "SR_CMD";
    case TIMING_PRO_SYSSTAT:
        return "SYSSTAT";
    case TIMING_PRO_FIFOSTAT:
        return "FIFOST";
    default:
        return "?";
    }
}

/* "1.21 us x 20000 = 24.2 ms | 1.80 us": per read, the longest the driver's wait can last at that
   rate, and per read under moderate load. */
static void print_timing(const struct timing *t, const struct timing *loaded)
{
    uint64_t ns = (uint64_t)t->ticks_total * 1000000000u / ((uint64_t)TIMING_READS * TICKS_PER_SECOND);
    uint64_t wait_us = ns * t->spins / 1000u;
    uint64_t ns_l =
        (uint64_t)loaded->ticks_total * 1000000000u / ((uint64_t)TIMING_READS * TICKS_PER_SECOND);

    printf("         %-8s %lu.%02lu us x %lu = %lu.%lu ms | %lu.%02lu us\n", timing_name(t->id),
           (unsigned long)(ns / 1000u),
           (unsigned long)(ns % 1000u / 10u), (unsigned long)t->spins, (unsigned long)(wait_us / 1000u),
           (unsigned long)(wait_us % 1000u / 100u), (unsigned long)(ns_l / 1000u),
           (unsigned long)(ns_l % 1000u / 10u));
}

static void buffer_cell(char *out, size_t n, const struct buffer_test *b)
{
    switch (b->result) {
    case BUF_MATCH:
        snprintf(out, n, "ok");
        break;
    case BUF_MISMATCH:
        snprintf(out, n, "BAD@%lu/%lu", (unsigned long)b->first_bad, (unsigned long)b->bad_bytes);
        break;
    case BUF_WRITE_FAILED:
        snprintf(out, n, "wbusy");
        break;
    case BUF_READ_FAILED:
        snprintf(out, n, "rbusy");
        break;
    case BUF_NOT_APPLICABLE:
        snprintf(out, n, "n/a");
        break;
    default:
        snprintf(out, n, "--");
        break;
    }
}

static char init_mark(uint32_t variant)
{
    const struct report *r = &g_report;
    if (!(r->init_tried & (1u << variant))) {
        return '.';
    }
    return (r->init_ok & (1u << variant)) ? '+' : '-';
}

/* The console is 64 columns by 28 rows. Keep to about 20, so nothing scrolls off the top, and put
   the raw values a person would read out of a photo first. */
static void draw(void)
{
    static const char *const combo[BUF_COMBOS] = { "IO>IO", "IO>DMA", "DMA>IO", "DMA>DMA" };
    const struct report *r = &g_report;
    const struct link_stats *l = s_variant ? &r->link[s_variant - 1u] : 0;
    char title[21];
    unsigned i;

    memcpy(title, (const uint8_t *)r->rom_header + 0x20, 20);
    title[20] = '\0';
    for (i = 0u; i < 20u; i++) {
        if (title[i] < 0x20 || title[i] > 0x7E) {
            title[i] = '.';
        }
    }

    console_clear();
    printf("multi64 bring-up %lu.%lu  cart %s  link %s  load %s  f%lu\n",
           (unsigned long)(BRINGUP_ROM_VERSION >> 16), (unsigned long)(BRINGUP_ROM_VERSION & 0xFFFFu),
           cart_name(r->cart), s_variant ? g_variants[s_variant - 1u].name : "none", load_name(r->load),
           (unsigned long)r->frame);
    printf("identify ed14 %08lX>%08lX  ed04 %08lX>%08lX\n", (unsigned long)r->ed_reg14_locked,
           (unsigned long)r->ed_reg14_unlocked, (unsigned long)r->ed_reg04_locked,
           (unsigned long)r->ed_usbcfg_after);
    printf("         sc64 %08lX>%08lX  sys %08lX %08lX\n", (unsigned long)r->sc64_ident_locked,
           (unsigned long)r->sc64_ident_unlocked, (unsigned long)r->pro_sysstat[0],
           (unsigned long)r->pro_sysstat[1]);
    printf("         init PRO%c X7%c SC64%c  d64 %08lX  probes %02lX\n", init_mark(VARIANT_PRO),
           init_mark(VARIANT_X7_IO), init_mark(VARIANT_SC64), (unsigned long)r->d64_magic,
           (unsigned long)r->probe_ok);
    printf("         rom %08lX %s\n", (unsigned long)r->rom_header[0], title);

    printf("blocks   per read x spins = longest wait | per read, moderate load\n");
    for (i = 0u; i < TIMING_SLOTS; i++) {
        if (r->timing[0][i].id != 0u) {
            print_timing(&r->timing[0][i], &r->timing[1][i]);
        }
    }
    printf("buffer   %08lX      no load       moderate load\n", (unsigned long)r->buf_addr);
    for (i = 0u; i < BUF_COMBOS; i++) {
        char a[24];
        char b[24];
        buffer_cell(a, sizeof a, &r->buffer[0][i]);
        buffer_cell(b, sizeof b, &r->buffer[1][i]);
        printf("         %-8s %-13s %s\n", combo[i], a, b);
    }

    if (l != 0) {
        printf("link     init %lu/%lu ready %lu  ticks %lu  frames %lu  req %lu err %lu last %02lX\n",
               (unsigned long)l->init_ok, (unsigned long)l->init_calls, (unsigned long)l->agent_ready,
               (unsigned long)l->agent_ticks, (unsigned long)l->agent_frames,
               (unsigned long)l->m64p_requests, (unsigned long)l->m64p_errors,
               (unsigned long)l->m64p_last_error);
        printf("         rx %lu/%lu %luB lost %lu  tx %lu/%lu %luB  rd %lu/%lu\n",
               (unsigned long)l->recv_data, (unsigned long)l->recv_calls, (unsigned long)l->recv_bytes,
               (unsigned long)l->recv_lost, (unsigned long)l->send_ok, (unsigned long)l->send_calls,
               (unsigned long)l->send_bytes, (unsigned long)(l->read_calls - l->read_failed),
               (unsigned long)l->read_calls);
        printf("         pi_io %lu busy %lu  max us: tick %lu rx %lu tx %lu\n",
               (unsigned long)l->pio_calls, (unsigned long)l->pio_failed,
               (unsigned long)ticks_us(l->tick_ticks_max), (unsigned long)ticks_us(l->recv_ticks_max),
               (unsigned long)ticks_us(l->send_ticks_max));
    } else {
        printf("link     no agent driver for this cart\n");
    }
    printf("load     irq gap max %luus  DMAs %lu  skipped %lu\n", (unsigned long)ticks_us(r->irq_gap_max),
           (unsigned long)r->load_started, (unsigned long)r->load_skipped);
    printf("\nR: X7 IO/DMA build   Z: load off/moderate/heavy   A: rerun blocks\n");
    console_render();
}

int main(void)
{
    /* Until the main loop, every line reaches the screen as it is printed (RENDER_AUTOMATIC), so a
       ROM that hangs on the way up shows the step it hung in as its last line. */
    console_init();
    console_set_render_mode(RENDER_AUTOMATIC);
    printf("multi64 bring-up %lu.%lu\n", (unsigned long)(BRINGUP_ROM_VERSION >> 16),
           (unsigned long)(BRINGUP_ROM_VERSION & 0xFFFFu));
    printf("boot: timer\n");
    timer_init();
    printf("boot: joypad\n");
    joypad_init();
    printf("boot: report\n");
    report_init();
    printf("boot: load timer\n");
    load_init();

    stage_identify();
    stage_blocks(0u);
    stage_conditions();
    printf("boot: link\n");
    console_set_render_mode(RENDER_MANUAL);

    s_variant = default_variant(g_report.cart);
    g_report.variant = s_variant;
    if (s_variant != VARIANT_NONE) {
        g_report.stages |= STAGE_LINK;
    }

    for (;;) {
        joypad_poll();
        controls(joypad_get_buttons_pressed(JOYPAD_PORT_1));
        tick_variant();
        g_report.frame++;
        draw();
    }
}
