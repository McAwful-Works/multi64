/**
 * EverDrive-64 PRO USB link for the test ROM: detection, receive from the host, send to it.
 *
 * libdragon's <usb.h> predates the PRO and does not recognize it, so this talks to the cart's
 * EDIO registers directly. Written against Krikzz's ed64-pro-pub `edio/everdrive.c` (MIT,
 * 5d7e96905331a841f97f8c51e0b0cba878e72fe3), not copied from it.
 *
 * NEVER RUN ON A CART. See docs/spec/l3-over-everdrive-pro.md, section 6 for the registers and
 * section 8 for what is unverified.
 *
 * EDIO registers, PI bus at 0x1F800000, one 32-bit word each:
 *   +0x00 FIFODATA  R/W  one byte per word: reads drain host bytes, writes go to the cart MCU
 *   +0x04 FIFOSTAT  R    low 16 bits: host bytes waiting
 *   +0x08 SYSSTAT   R    bit 0 MCU busy; bit 3 inverts on every read; bits 7..4 read 0xA
 *   +0x14 EDID      R    0xED64xxxx
 */
#ifndef MULTI64_ED64PRO_H
#define MULTI64_ED64PRO_H

#include <stdint.h>

/**
 * Returns 1 if the cart is an EverDrive-64 PRO, 0 otherwise.
 *
 * Call it before libdragon's usb_initialize(), whose EverDrive probe writes X-series registers at
 * addresses the PRO uses for other things. Only reads happen until the ID and status registers
 * look like a PRO's; then it sends one status command and checks the reply.
 */
int ed64pro_detect(void);

/** Host bytes waiting in the FIFO. */
uint32_t ed64pro_rx_available(void);

/** Read up to `len` waiting host bytes into `dst`. Never blocks; returns the count read. */
uint32_t ed64pro_rx_read(uint8_t *dst, uint32_t len);

/** Send `len` bytes to the host. Returns 1 on success, 0 if the cart stayed busy too long. */
int ed64pro_tx_write(const uint8_t *src, uint32_t len);

#endif
