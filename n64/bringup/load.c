#include "load.h"

#include <libdragon.h>

#include "report.h"

#define BR_PI_DRAM_ADDR (*(volatile uint32_t *)0xA4600000u)
#define BR_PI_CART_ADDR (*(volatile uint32_t *)0xA4600004u)
#define BR_PI_WR_LEN (*(volatile uint32_t *)0xA460000Cu) /* cart to RDRAM */
#define BR_PI_STATUS (*(volatile uint32_t *)0xA4600010u)

#define LOAD_PERIOD_US 2000
#define LOAD_MAX_BYTES 16384u
/* Where the DMAs read: a window of this ROM's own image, so nothing beyond it is touched. */
#define LOAD_ROM_BASE 0x10001000u
#define LOAD_ROM_SPAN 0x00020000u

static timer_link_t *s_timer;
static void *s_target;
static uint32_t s_level;
static uint32_t s_bytes;
static uint32_t s_off;
static uint32_t s_last;

static void load_tick(int ovfl)
{
    struct report *r = &g_report;
    uint32_t t = C0_COUNT();

    (void)ovfl;
    if (s_last != 0u && t - s_last > r->irq_gap_max) {
        r->irq_gap_max = t - s_last;
    }
    s_last = t;

    if (s_bytes == 0u) {
        return;
    }
    /* Busy means the last one is still running, or the code under test holds the bus: a game's PI
       manager would queue, and this just skips a turn. */
    if (BR_PI_STATUS & 3u) {
        r->load_skipped++;
        return;
    }
    BR_PI_DRAM_ADDR = (uint32_t)s_target & 0x1FFFFFFFu;
    BR_PI_CART_ADDR = LOAD_ROM_BASE + s_off;
    BR_PI_WR_LEN = s_bytes - 1u;
    s_off = (s_off + s_bytes) % LOAD_ROM_SPAN;
    r->load_started++;
}

void load_init(void)
{
    /* Uncached, and never read: the DMA only has to land somewhere harmless. */
    s_target = malloc_uncached(LOAD_MAX_BYTES);
    s_timer = new_timer(TIMER_TICKS(LOAD_PERIOD_US), TF_CONTINUOUS, load_tick);
    load_set(LOAD_OFF);
}

void load_set(uint32_t level)
{
    struct report *r = &g_report;
    uint32_t bytes = level == LOAD_HEAVY ? LOAD_MAX_BYTES : level == LOAD_MODERATE ? 4096u : 0u;

    disable_interrupts();
    s_level = level < LOAD_LEVELS ? level : LOAD_OFF;
    s_bytes = bytes;
    s_last = 0u;
    r->load = s_level;
    r->load_bytes = bytes;
    r->load_period_ticks = TIMER_TICKS(LOAD_PERIOD_US);
    r->load_started = 0u;
    r->load_skipped = 0u;
    r->irq_gap_max = 0u;
    enable_interrupts();
}

uint32_t load_level(void)
{
    return s_level;
}
