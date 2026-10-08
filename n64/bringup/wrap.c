/**
 * The wrap_* functions each variant's agent and driver call in place of the real ones (variants.h):
 * each counts and times the call into g_report.link[], then calls through.
 */
#include <libdragon.h>

#include "report.h"
#include "variants.h"

#define LOST 0xFFFFFFFFu

static inline uint32_t now(void)
{
    return C0_COUNT();
}

static inline void keep_max(uint32_t *max, uint32_t v)
{
    if (v > *max) {
        *max = v;
    }
}

static inline int pio_count(unsigned idx, int ok)
{
    struct link_stats *l = &g_report.link[idx];
    l->pio_calls++;
    if (!ok) {
        l->pio_failed++;
    }
    return ok;
}

/* The driver's and PEEKROM's calls into pi_io, for one variant. */
#define PI_IO_WRAPS(P, IDX)                                                                      \
    int P##wrap_pi_io_read(uint32_t addr, uint32_t *value)                                     \
    {                                                                                          \
        return pio_count(IDX, P##pi_io_read(addr, value));                                     \
    }                                                                                          \
    int P##wrap_pi_io_write(uint32_t addr, uint32_t value)                                     \
    {                                                                                          \
        return pio_count(IDX, P##pi_io_write(addr, value));                                    \
    }                                                                                          \
    int P##wrap_pi_io_write_stored(uint32_t addr, uint32_t value, int *stored)                 \
    {                                                                                          \
        return pio_count(IDX, P##pi_io_write_stored(addr, value, stored));                     \
    }                                                                                          \
    int P##wrap_pi_io_load_words(uint32_t *dst, uint32_t addr, uint32_t words)                 \
    {                                                                                          \
        return pio_count(IDX, P##pi_io_load_words(dst, addr, words));                          \
    }                                                                                          \
    int P##wrap_pi_io_store_words(const uint32_t *src, uint32_t addr, uint32_t words)          \
    {                                                                                          \
        return pio_count(IDX, P##pi_io_store_words(src, addr, words));                         \
    }                                                                                          \
    int P##wrap_pi_io_load_port(uint8_t *dst, uint32_t addr, uint32_t len)                     \
    {                                                                                          \
        return pio_count(IDX, P##pi_io_load_port(dst, addr, len));                             \
    }                                                                                          \
    int P##wrap_pi_io_store_port(const uint8_t *src, uint32_t addr, uint32_t len)              \
    {                                                                                          \
        return pio_count(IDX, P##pi_io_store_port(src, addr, len));                            \
    }

/* The agent's calls into an EverDrive driver, which share one shape. */
#define STREAM_WRAPS(P, DRV, IDX)                                                                \
    int P##wrap_##DRV##_init(void)                                                             \
    {                                                                                          \
        struct link_stats *l = &g_report.link[IDX];                                            \
        int ok = P##DRV##_init();                                                              \
        l->init_calls++;                                                                       \
        if (ok) {                                                                              \
            l->init_ok++;                                                                      \
        }                                                                                      \
        return ok;                                                                             \
    }                                                                                          \
    uint32_t P##wrap_##DRV##_receive(uint8_t *dst, uint32_t cap)                               \
    {                                                                                          \
        struct link_stats *l = &g_report.link[IDX];                                            \
        uint32_t t0 = now();                                                                   \
        uint32_t n = P##DRV##_receive(dst, cap);                                               \
        keep_max(&l->recv_ticks_max, now() - t0);                                              \
        l->recv_calls++;                                                                       \
        if (n == LOST) {                                                                       \
            l->recv_lost++;                                                                    \
        } else if (n != 0u) {                                                                  \
            l->recv_data++;                                                                    \
            l->recv_bytes += n;                                                                \
        }                                                                                      \
        return n;                                                                              \
    }                                                                                          \
    int P##wrap_##DRV##_send(const uint8_t *data, uint32_t len)                                \
    {                                                                                          \
        struct link_stats *l = &g_report.link[IDX];                                            \
        uint32_t t0 = now();                                                                   \
        int ok = P##DRV##_send(data, len);                                                     \
        keep_max(&l->send_ticks_max, now() - t0);                                              \
        l->send_calls++;                                                                       \
        if (ok) {                                                                              \
            l->send_ok++;                                                                      \
            l->send_bytes += len;                                                              \
        }                                                                                      \
        return ok;                                                                             \
    }

PI_IO_WRAPS(bsc_, VARIANT_SC64 - 1u)
PI_IO_WRAPS(bx7_, VARIANT_X7_IO - 1u)
PI_IO_WRAPS(bx7d_, VARIANT_X7_DMA - 1u)
PI_IO_WRAPS(bpro_, VARIANT_PRO - 1u)

STREAM_WRAPS(bx7_, ed64, VARIANT_X7_IO - 1u)
STREAM_WRAPS(bx7d_, ed64, VARIANT_X7_DMA - 1u)
STREAM_WRAPS(bpro_, ed64pro, VARIANT_PRO - 1u)

/* The SC64 driver has a packet interface: poll, then read what it staged. */

int bsc_wrap_sc64_init(void)
{
    struct link_stats *l = &g_report.link[VARIANT_SC64 - 1u];
    int ok = bsc_sc64_init();
    l->init_calls++;
    if (ok) {
        l->init_ok++;
    }
    return ok;
}

uint32_t bsc_wrap_sc64_poll(uint8_t *datatype)
{
    struct link_stats *l = &g_report.link[VARIANT_SC64 - 1u];
    uint32_t t0 = now();
    uint32_t n = bsc_sc64_poll(datatype);
    keep_max(&l->recv_ticks_max, now() - t0);
    l->recv_calls++;
    if (n != 0u) {
        l->recv_data++;
        l->recv_bytes += n;
    }
    return n;
}

int bsc_wrap_sc64_read(void *dst, uint32_t offset, uint32_t len)
{
    struct link_stats *l = &g_report.link[VARIANT_SC64 - 1u];
    int ok = bsc_sc64_read(dst, offset, len);
    l->read_calls++;
    if (!ok) {
        l->read_failed++;
    }
    return ok;
}

int bsc_wrap_sc64_write(uint8_t datatype, const void *data, uint32_t len)
{
    struct link_stats *l = &g_report.link[VARIANT_SC64 - 1u];
    uint32_t t0 = now();
    int ok = bsc_sc64_write(datatype, data, len);
    keep_max(&l->send_ticks_max, now() - t0);
    l->send_calls++;
    if (ok) {
        l->send_ok++;
        l->send_bytes += len;
    }
    return ok;
}

#define OPS(P, NAME)                                                                             \
    {                                                                                          \
        NAME, P##agent_tick, P##agent_get_ticks, P##agent_get_frames_handled, P##agent_is_ready, \
            P##m64p_get_requests, P##m64p_get_errors, P##m64p_get_last_error                   \
    }

const struct variant_ops g_variants[VARIANT_COUNT] = {
    OPS(bsc_, "SC64"),
    OPS(bx7_, "X7 IO"),
    OPS(bx7d_, "X7 DMA"),
    OPS(bpro_, "PRO"),
};
