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
    /* A DMA may have started between the check and the mask. */
    if (!wait_bits(PI_STATUS_DMA_BUSY | PI_STATUS_IO_BUSY)) {
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
    uint32_t sr;
    int ok;
    if (!begin(&sr)) {
        return 0;
    }
    *io(addr) = value;
    /* Stores are posted: the next access must not start until this one has landed. */
    ok = wait_bits(PI_STATUS_IO_BUSY);
    int_restore(sr);
    return ok;
}

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
