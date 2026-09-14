/**
 * PI-bus access for the EverDrive agent drivers.
 *
 * The same rules sc64.c established, and docs/integration/cart-agent.md section 3 explains:
 * never touch the PI control registers; move data by CPU load and store through the uncached
 * window; mask interrupts around each access, having checked the PI is idle; wait on IO_BUSY after
 * every store; never mask for more than a short burst; and bound every wait.
 *
 * sc64.c keeps its own private copy of these helpers on purpose. It is the only driver that has
 * run on hardware, and moving its code here would change that object. Fold the two together once
 * the SC64 build has been re-tested.
 *
 * Every function returns 1 on success and 0 when the PI stayed busy; on 0 the caller's data is
 * incomplete and must not be trusted.
 */
#ifndef MULTI64_AGENT_PI_IO_H
#define MULTI64_AGENT_PI_IO_H

#include "m64p_types.h"

/** Read one 32-bit register. */
int pi_io_read(uint32_t addr, uint32_t *value);

/** Write one 32-bit register. */
int pi_io_write(uint32_t addr, uint32_t value);

/** Load `words` consecutive words starting at `addr`. */
int pi_io_load_words(uint32_t *dst, uint32_t addr, uint32_t words);

/** Store `words` consecutive words starting at `addr`. */
int pi_io_store_words(const uint32_t *src, uint32_t addr, uint32_t words);

/** Read `len` words from one register, keeping the low byte of each: a byte-wide read port. */
int pi_io_load_port(uint8_t *dst, uint32_t addr, uint32_t len);

/** Write `len` bytes to one register, one word each: a byte-wide write port. */
int pi_io_store_port(const uint8_t *src, uint32_t addr, uint32_t len);

#endif
