/**
 * EverDrive-64 X7 USB driver for the game-resident agent.
 *
 * NEVER RUN ON A CART. The console half of the DMA@ framing in
 * docs/spec/l3-over-everdrive-x7.md section 4, written against libdragon's src/usb.c
 * (trunk c4a7e119), not copied from it. libdragon moves data by DMA and stages received packets
 * at the top of the ROM window; this driver does neither. Bytes go through the cart's 512-byte
 * USB window by CPU load and store (pi_io.c), so it owns no PI state and cannot land inside a
 * running game's ROM.
 *
 * Registers, PI bus:
 *   0x1F800004 USBCFG   USB mode in, status out
 *   0x1F800014 VERSION  0xED640013 (X7, X5) or 0xED640008 (3.0)
 *   0x1F800400 USBDAT   512-byte data window; a transfer of n bytes uses its last n bytes
 *   0x1F808000 SYSCFG
 *   0x1F808004 KEY      0xAA55 unlocks the registers
 */
#ifndef MULTI64_AGENT_ED64_H
#define MULTI64_AGENT_ED64_H

#include "m64p_types.h"

/**
 * Unlock the registers and confirm an X7 or 3.0 with its USB unit powered.
 * Returns 1 on success, 0 if this is not such a cart (an X5 has no USB).
 */
int ed64_init(void);

/**
 * Receive at most one USB message. If it is a well-formed DMA@ message carrying the L3 datatype
 * and fits in `cap`, copy its payload to `dst` and return the byte count. Returns 0 when nothing
 * is waiting, and also, having drained it, for any other message.
 */
uint32_t ed64_receive(uint8_t *dst, uint32_t cap);

/** Send `len` bytes as one DMA@ message of the L3 datatype. Returns 1 on success, 0 on timeout. */
int ed64_send(const uint8_t *data, uint32_t len);

#endif
