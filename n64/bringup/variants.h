/**
 * The agent builds this ROM links side by side, and the two stand-alone copies of pi_io.c.
 *
 * Each variant is the agent's own sources (n64/agent: agent.c, a cart driver, pi_io.c, cart_rom.c,
 * and n64/test-rom/mem_proto.c) compiled once with the agent's flags, linked into one object, and
 * every symbol given the variant's prefix, so four agents can share one image. Before that link the
 * Makefile renames the agent's calls into its cart driver, and the driver's and PEEKROM's calls into
 * pi_io, to wrap_* names: wrap.c defines those, counts and times each call into the report, and
 * calls through to the real function. The agent's own code is not changed.
 *
 * Prefixes: bsc_ (sc64.c), bx7_ (ed64.c), bx7d_ (ed64.c over PI_IO_DMA), bpro_ (ed64pro.c).
 * The stand-alone pi_io copies, for the harness's own probes: pio_ (CPU load and store) and pdma_
 * (PI_IO_DMA).
 */
#ifndef MULTI64_BRINGUP_VARIANTS_H
#define MULTI64_BRINGUP_VARIANTS_H

#include <stdint.h>

#define DECLARE_PI_IO(P)                                                                         \
    int P##pi_io_read(uint32_t addr, uint32_t *value);                                         \
    int P##pi_io_write(uint32_t addr, uint32_t value);                                         \
    int P##pi_io_write_stored(uint32_t addr, uint32_t value, int *stored);                     \
    int P##pi_io_load_words(uint32_t *dst, uint32_t addr, uint32_t words);                     \
    int P##pi_io_store_words(const uint32_t *src, uint32_t addr, uint32_t words);              \
    int P##pi_io_load_port(uint8_t *dst, uint32_t addr, uint32_t len);                         \
    int P##pi_io_store_port(const uint8_t *src, uint32_t addr, uint32_t len);

#define DECLARE_AGENT(P)                                                                         \
    void P##agent_tick(void);                                                                  \
    uint32_t P##agent_get_ticks(void);                                                         \
    uint32_t P##agent_get_frames_handled(void);                                                \
    int P##agent_is_ready(void);                                                               \
    uint32_t P##m64p_get_requests(void);                                                       \
    uint32_t P##m64p_get_errors(void);                                                         \
    uint8_t P##m64p_get_last_error(void);

DECLARE_PI_IO(pio_)
DECLARE_PI_IO(pdma_)

DECLARE_AGENT(bsc_)
int bsc_sc64_init(void);
uint32_t bsc_sc64_poll(uint8_t *datatype);
int bsc_sc64_read(void *dst, uint32_t offset, uint32_t len);
int bsc_sc64_write(uint8_t datatype, const void *data, uint32_t len);

DECLARE_AGENT(bx7_)
DECLARE_PI_IO(bx7_)
int bx7_ed64_init(void);
uint32_t bx7_ed64_receive(uint8_t *dst, uint32_t cap);
int bx7_ed64_send(const uint8_t *data, uint32_t len);

DECLARE_AGENT(bx7d_)
DECLARE_PI_IO(bx7d_)
int bx7d_ed64_init(void);
uint32_t bx7d_ed64_receive(uint8_t *dst, uint32_t cap);
int bx7d_ed64_send(const uint8_t *data, uint32_t len);

DECLARE_AGENT(bpro_)
DECLARE_PI_IO(bpro_)
int bpro_ed64pro_init(void);
uint32_t bpro_ed64pro_receive(uint8_t *dst, uint32_t cap);
int bpro_ed64pro_send(const uint8_t *data, uint32_t len);

/* The sc64 variant's PEEKROM reads through its own pi_io copy too. */
DECLARE_PI_IO(bsc_)

/** One variant's entry points, so the main loop can drive whichever is selected. */
struct variant_ops {
    const char *name;
    void (*tick)(void);
    uint32_t (*ticks)(void);
    uint32_t (*frames)(void);
    int (*ready)(void);
    uint32_t (*requests)(void);
    uint32_t (*errors)(void);
    uint8_t (*last_error)(void);
};

/** Index VARIANT_* - 1. */
extern const struct variant_ops g_variants[];

#endif
