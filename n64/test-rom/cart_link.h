/**
 * One USB surface for the test ROM, whichever cart it boots on.
 *
 * SummerCart64 and the X-series EverDrive go through libdragon's <usb.h>. The EverDrive-64 PRO
 * is not a cart libdragon knows, so it goes through ed64pro.c. The PRO delivers a bare byte
 * stream with no datatype header: everything it receives is the L3 stream.
 */
#ifndef MULTI64_CART_LINK_H
#define MULTI64_CART_LINK_H

#include <stdint.h>

/** Datatype tag of the Multi64 L3 stream (l3-over-sc64.md section 1). */
#define CART_LINK_L3 0x01

enum cart_link_kind {
    /** No cart that libdragon or ed64pro.c recognizes. */
    CART_LINK_NONE = 0,
    CART_LINK_SC64,
    /** X-series (X7, 3.0), through libdragon. */
    CART_LINK_EVERDRIVE,
    CART_LINK_ED64PRO,
    /** Recognized by libdragon, but not a cart this ROM supports (64drive). */
    CART_LINK_OTHER,
};

/** Find the cart and set up its link. Call once, before anything else here. */
enum cart_link_kind cart_link_init(void);

/**
 * Bytes waiting from the host, 0 if none. Writes the datatype through `datatype`; always
 * CART_LINK_L3 on a PRO.
 */
uint32_t cart_link_poll(uint8_t *datatype);

/** Read `len` of the bytes cart_link_poll() reported. */
void cart_link_read(uint8_t *dst, int len);

/** Send L3 bytes to the host. */
void cart_link_write(const uint8_t *data, int len);

/**
 * Writes that gave up before the whole message was sent, since boot. Never reset: the checks that
 * provoke it run in RAW_ECHO, which cannot report it, between two mode changes.
 */
uint32_t cart_link_tx_failures(void);

#endif
