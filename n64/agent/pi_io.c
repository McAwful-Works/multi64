#include "pi_io.h"

#define PI_STATUS 0xA4600010u

#define PI_STATUS_DMA_BUSY (1u << 0)
#define PI_STATUS_IO_BUSY (1u << 1)

/** Spins before a busy PI is reported as a failure. A legitimate wait is microseconds. */
#define PI_WAIT_SPINS 100000u

/** Accesses per masked span: a few microseconds of latency at most. */
#define PI_IO_BURST 64u

/* Uncached (KSEG1) view: cart and register access must never be cached. */
static volatile uint32_t *io(uint32_t phys)
{
    return (volatile uint32_t *)(0xA0000000u | (phys & 0x1FFFFFFFu));
}

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

static int wait_bits(uint32_t bits)
{
    uint32_t spins = PI_WAIT_SPINS;
    while (*io(PI_STATUS) & bits) {
        if (--spins == 0u) {
            return 0;
        }
    }
    return 1;
}

/** Mask interrupts with the PI idle, or return 0 with interrupts as they were. */
static int begin(uint32_t *sr)
{
    if (!wait_bits(PI_STATUS_DMA_BUSY | PI_STATUS_IO_BUSY)) {
        return 0;
    }
    *sr = int_mask();
    /*
     * A DMA may have started between the check and the mask -- so look, and give up if one
     * has. Do NOT wait for it here. The wait above is harmless because interrupts are on
     * and the game keeps running; waiting in this masked span instead holds VI, AI and SI
     * off for the length of the game's own transfer, every frame, until the threads blocked
     * on those interrupts stop. See pi_idle_now() in sc64.c, where that froze Banjo-Tooie
     * seconds into its opening cutscene with the music still playing.
     *
     * Untested on hardware: no X7 or PRO here. It is the same defect the SC64 driver had,
     * in the same shape, and less masked time cannot be worse.
     */
    if ((*io(PI_STATUS) & (PI_STATUS_DMA_BUSY | PI_STATUS_IO_BUSY)) != 0u) {
        int_restore(*sr);
        return 0;
    }
    return 1;
}

int pi_io_read(uint32_t addr, uint32_t *value)
{
    uint32_t sr;
    if (!begin(&sr)) {
        return 0;
    }
    *value = *io(addr);
    int_restore(sr);
    return 1;
}

int pi_io_write(uint32_t addr, uint32_t value)
{
    int stored;
    return pi_io_write_stored(addr, value, &stored);
}

int pi_io_write_stored(uint32_t addr, uint32_t value, int *stored)
{
    uint32_t sr;
    int ok;
    *stored = 0;
    if (!begin(&sr)) {
        return 0;
    }
    *io(addr) = value;
    *stored = 1;
    /* Stores are posted: the next access must not start until this one has landed. */
    ok = wait_bits(PI_STATUS_IO_BUSY);
    int_restore(sr);
    return ok;
}

#ifdef PI_IO_DMA
/*
 * PI_IO_DMA (make PI_IO=dma, and n64/bringup's DMA variant): pi_io_load_words and pi_io_store_words
 * move a run of words by PI DMA through a bounce buffer, the way libdragon and Krikzz's own code move
 * an EverDrive's USB window, instead of by CPU load and store.
 *
 * It breaks the agent's first rule (docs/integration/cart-agent.md section 3): it writes the PI's
 * DMA registers, which belong to whatever transfer the game has in flight, and the DMA it runs ends
 * in a PI interrupt that a game's PI manager does not expect. It exists to find out on a cart
 * whether an X7 needs DMA, not to put in a game. Each transfer starts with the PI idle and
 * interrupts masked, waits for itself inside that span (a 512-byte transfer is about 0.1 ms), clears
 * the interrupt it raised, and gives up after PI_WAIT_SPINS like every other wait here.
 */
#define PI_DRAM_ADDR 0xA4600000u
#define PI_CART_ADDR 0xA4600004u
#define PI_RD_LEN 0xA4600008u /* RDRAM to cart */
#define PI_WR_LEN 0xA460000Cu /* cart to RDRAM */
#define PI_STATUS_CLEAR_INTR (1u << 1)

/** Bytes per transfer: one EverDrive USB window. */
#define PI_DMA_CHUNK 512u

static uint32_t s_bounce[PI_DMA_CHUNK / 4u] __attribute__((aligned(16)));

/** s_bounce through KSEG1. The CPU touches it only there, so the DMA never races a cache line. */
static volatile uint32_t *bounce(void)
{
    return (volatile uint32_t *)((uint32_t)s_bounce | 0xA0000000u);
}

/* Write back and drop every cache line of s_bounce. Nothing here reads it through KSEG0, but its
   BSS was zeroed that way at boot, and a dirty line written back later would land on a transfer. */
static void bounce_flush(void)
{
    uint32_t a;
    for (a = (uint32_t)s_bounce; a < (uint32_t)s_bounce + sizeof s_bounce; a += 16u) {
        __asm__ __volatile__("cache 0x15, 0(%0)" : : "r"(a)); /* hit writeback invalidate, D */
    }
}

/** Move `bytes` (even, at most PI_DMA_CHUNK) between s_bounce and the cart. */
static int dma_chunk(uint32_t addr, uint32_t bytes, int to_cart)
{
    uint32_t sr;
    int ok;

    if (!begin(&sr)) {
        return 0;
    }
    *io(PI_DRAM_ADDR) = (uint32_t)s_bounce & 0x1FFFFFFFu;
    *io(PI_CART_ADDR) = addr & 0x1FFFFFFFu;
    *io(to_cart ? PI_RD_LEN : PI_WR_LEN) = bytes - 1u;
    ok = wait_bits(PI_STATUS_DMA_BUSY | PI_STATUS_IO_BUSY);
    /* Only once the transfer is over: clearing it earlier would not stop a later interrupt. */
    if (ok) {
        *io(PI_STATUS) = PI_STATUS_CLEAR_INTR;
    }
    int_restore(sr);
    return ok;
}

int pi_io_load_words(uint32_t *dst, uint32_t addr, uint32_t words)
{
    uint32_t done = 0u;

    bounce_flush();
    while (done < words) {
        uint32_t n = words - done;
        uint32_t i;

        if (n > PI_DMA_CHUNK / 4u) {
            n = PI_DMA_CHUNK / 4u;
        }
        if (!dma_chunk(addr + done * 4u, n * 4u, 0)) {
            return 0;
        }
        for (i = 0u; i < n; i++) {
            dst[done + i] = bounce()[i];
        }
        done += n;
    }
    return 1;
}

int pi_io_store_words(const uint32_t *src, uint32_t addr, uint32_t words)
{
    uint32_t done = 0u;

    bounce_flush();
    while (done < words) {
        uint32_t n = words - done;
        uint32_t i;

        if (n > PI_DMA_CHUNK / 4u) {
            n = PI_DMA_CHUNK / 4u;
        }
        for (i = 0u; i < n; i++) {
            bounce()[i] = src[done + i];
        }
        if (!dma_chunk(addr + done * 4u, n * 4u, 1)) {
            return 0;
        }
        done += n;
    }
    return 1;
}
#else
int pi_io_load_words(uint32_t *dst, uint32_t addr, uint32_t words)
{
    uint32_t done = 0u;

    while (done < words) {
        uint32_t n = words - done;
        uint32_t sr;
        uint32_t i;

        if (n > PI_IO_BURST) {
            n = PI_IO_BURST;
        }
        if (!begin(&sr)) {
            return 0;
        }
        for (i = 0u; i < n; i++) {
            dst[done + i] = *io(addr + (done + i) * 4u);
        }
        int_restore(sr);
        done += n;
    }
    return 1;
}

int pi_io_store_words(const uint32_t *src, uint32_t addr, uint32_t words)
{
    uint32_t done = 0u;

    while (done < words) {
        uint32_t n = words - done;
        uint32_t sr;
        uint32_t i;

        if (n > PI_IO_BURST) {
            n = PI_IO_BURST;
        }
        if (!begin(&sr)) {
            return 0;
        }
        for (i = 0u; i < n; i++) {
            /* Back-to-back stores overrun the PI's write path and are silently dropped. */
            if (!wait_bits(PI_STATUS_IO_BUSY)) {
                int_restore(sr);
                return 0;
            }
            *io(addr + (done + i) * 4u) = src[done + i];
        }
        if (!wait_bits(PI_STATUS_IO_BUSY)) {
            int_restore(sr);
            return 0;
        }
        int_restore(sr);
        done += n;
    }
    return 1;
}
#endif

int pi_io_load_port(uint8_t *dst, uint32_t addr, uint32_t len)
{
    uint32_t done = 0u;

    while (done < len) {
        uint32_t n = len - done;
        uint32_t sr;
        uint32_t i;

        if (n > PI_IO_BURST) {
            n = PI_IO_BURST;
        }
        if (!begin(&sr)) {
            return 0;
        }
        for (i = 0u; i < n; i++) {
            dst[done + i] = (uint8_t)*io(addr);
        }
        int_restore(sr);
        done += n;
    }
    return 1;
}

int pi_io_store_port(const uint8_t *src, uint32_t addr, uint32_t len)
{
    uint32_t done = 0u;

    while (done < len) {
        uint32_t n = len - done;
        uint32_t sr;
        uint32_t i;

        if (n > PI_IO_BURST) {
            n = PI_IO_BURST;
        }
        if (!begin(&sr)) {
            return 0;
        }
        for (i = 0u; i < n; i++) {
            if (!wait_bits(PI_STATUS_IO_BUSY)) {
                int_restore(sr);
                return 0;
            }
            *io(addr) = (uint32_t)src[done + i];
        }
        if (!wait_bits(PI_STATUS_IO_BUSY)) {
            int_restore(sr);
            return 0;
        }
        int_restore(sr);
        done += n;
    }
    return 1;
}
