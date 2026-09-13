/**
 * EverDrive-64 PRO USB driver for the game-resident agent.
 *
 * NEVER RUN ON A CART. The console side of docs/spec/l3-over-everdrive-pro.md section 6: the same
 * mapping as the test ROM's n64/test-rom/ed64pro.c, rewritten under the agent's rules. That file
 * uses libdragon and a millisecond clock; this one uses neither, and reaches the bus only through
 * pi_io.c.
 *
 * EDIO registers, PI bus at 0x1F800000, one 32-bit word each:
 *   +0x00 FIFODATA  R/W  one byte per word: reads drain host bytes, writes go to the cart MCU
 *   +0x04 FIFOSTAT  R    low 16 bits: host bytes waiting
 *   +0x08 SYSSTAT   R    bit 0 MCU busy; bit 3 inverts on every read; bits 7..4 read 0xA
 *   +0x14 EDID      R    0xED64xxxx
 */
#ifndef MULTI64_AGENT_ED64PRO_H
#define MULTI64_AGENT_ED64PRO_H

#include "m64p_types.h"

/**
 * Confirm an EverDrive-64 PRO (spec 6.2). Only reads until the ID and status registers look like a
 * PRO's; then one status command. Returns 1 on success, 0 otherwise.
 */
int ed64pro_init(void);

/** Copy up to `cap` waiting host bytes to `dst`; returns the count, 0 if none. Never blocks. */
uint32_t ed64pro_receive(uint8_t *dst, uint32_t cap);

/** Send `len` bytes to the host. Returns 1 on success, 0 if the cart stayed busy. */
int ed64pro_send(const uint8_t *data, uint32_t len);

#endif
