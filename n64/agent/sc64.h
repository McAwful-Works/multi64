/**
 * Minimal SummerCart64 USB driver for a game-resident agent.
 *
 * Written against the SC64 register interface as documented by libdragon's
 * src/usb.c (itself derived from UNFLoader), not copied from it: the agent has to
 * be free of libdragon *and* of libultra, because it is injected into a ROM whose
 * SDK entry points we do not control.
 *
 * Register map, PI bus at 0x1FFF0000:
 *   +0x00 SR_CMD   command / status
 *   +0x04 DATA_0   argument and result word 0
 *   +0x08 DATA_1   argument and result word 1
 *   +0x0C IDENT    reads 'SCv2' on a SummerCart64 v2
 *   +0x10 KEY      unlock sequence
 */
#ifndef MULTI64_AGENT_SC64_H
#define MULTI64_AGENT_SC64_H

/* The M64P types header rather than <stdint.h>, so one file adapts the whole agent
   to a toolchain without the C library headers. */
#include "m64p_types.h"

/** Datatype tag for the Multi64 L3 stream (l3-over-sc64.md §1). */
#define SC64_DATATYPE_L3 0x01

/**
 * Unlock the cart and confirm it is an SC64 v2.
 * Returns 1 on success, 0 if this is not an SC64 (or it did not answer).
 */
int sc64_init(void);

/**
 * Non-blocking check for an inbound USB packet.
 *
 * On data, stages it into the cart's buffer and returns the byte count, writing the
 * datatype tag through `datatype`. Returns 0 when nothing is waiting — the
 * common case, and the only path taken on a frame with no host traffic.
 */
uint32_t sc64_poll(uint8_t *datatype);

/**
 * Copy `len` bytes of a staged packet (see sc64_poll) out of the cart's buffer into
 * `dst`. `offset` is relative to the start of the packet.
 */
void sc64_read(void *dst, uint32_t offset, uint32_t len);

/**
 * Send one packet to the host. Returns 1 on success, 0 on timeout.
 *
 * The SC64 sends from its own memory, not RDRAM, so this stages `data` across the
 * PI bus first. That is the expensive part, and the reason callers should send
 * one whole L3 frame rather than dribbling it out.
 */
int sc64_write(uint8_t datatype, const void *data, uint32_t len);

#endif
