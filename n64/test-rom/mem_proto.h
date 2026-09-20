/**
 * L3 APPLICATION RDRAM peek/poke (M64P) — see docs/spec/memory-l3-application-v0.md
 *
 * Deliberately free of libdragon: the only outside world this module knows is the
 * hooks below and m64p_types.h. That is what lets the same source be lifted into a
 * game-resident agent (libultra, no libdragon) without editing the protocol handling.
 */
#ifndef MULTI64_MEM_PROTO_H
#define MULTI64_MEM_PROTO_H

/* Fixed-width types come from m64p_types.h, never the C library directly, so a build
   without <stdint.h> replaces that one file. See its header comment. */
#include "m64p_types.h"

#define M64P_MAGIC0 0x4DU
#define M64P_MAGIC1 0x36U
#define M64P_MAGIC2 0x34U
#define M64P_MAGIC3 0x50U

#define M64P_MSG_HELLO 0x01U
#define M64P_MSG_PEEKV 0x02U
#define M64P_MSG_POKEV 0x03U
#define M64P_MSG_PEEKROM 0x04U
#define M64P_MSG_WATCH 0x05U

#define M64P_MSG_HELLO_ACK 0x81U
#define M64P_MSG_PEEKV_RESP 0x82U
#define M64P_MSG_POKE_ACK 0x83U
#define M64P_MSG_PEEKROM_RESP 0x84U
#define M64P_MSG_WATCH_ACK 0x85U
#define M64P_MSG_ERR 0xE0U

#define M64P_ERR_MALFORMED 0x01U
#define M64P_ERR_TOO_MANY 0x02U
#define M64P_ERR_TOO_LARGE 0x03U
#define M64P_ERR_RANGE 0x04U
#define M64P_ERR_READONLY 0x05U
#define M64P_ERR_UNSUPPORTED 0x06U
#define M64P_ERR_BUSY 0x07U

/** Protocol revision reported in `HELLO_ACK`. */
#define M64P_PROTO_VERSION 0U
/** `HELLO_ACK` flags bit 0: this agent accepts `POKEV`. */
#define M64P_FLAG_WRITABLE 0x01U
/** `HELLO_ACK` flags bit 1: this agent answers `PEEKROM`, and `rom_bytes` follows the flags. */
#define M64P_FLAG_CART_ROM 0x02U
/** `HELLO_ACK` flags bit 2: this agent watches slots, and `watch_slots` follows rom_bytes. */
#define M64P_FLAG_WATCH 0x04U

/** Spec §4 limits. Exceeding any of them is an error, never a truncation. */
#define M64P_MAX_REGIONS 32
#define M64P_MAX_REGION_BYTES 4096
/* Worst case on the wire is a POKEV *request* (6-byte region header vs 2 in a
   PEEKV response): 8 + 6n + total. At n = 32 that leaves 7992 under an 8192-byte
   L3 payload, so one usb_write still carries the whole frame. */
#define M64P_MAX_TOTAL_BYTES 7936

/*
 * Spec §4.3 limits. A slot is small by construction: this exists for the one place a game
 * records an event, not for watching data structures.
 *
 * The cost is all per frame and all static: M64P_WATCH_SLOTS slots are read and compared
 * every frame, and the queue is the only memory an event ever occupies.
 */
#define M64P_WATCH_SLOTS 4
#define M64P_WATCH_MAX_LEN 8
#define M64P_WATCH_MAX_VALUES 8
#define M64P_WATCH_QUEUE 16

/** Largest M64P application payload this module will build (magic + msg + body). */
#define M64P_APP_CAP (5 + 3 + (M64P_MAX_REGIONS * 2) + M64P_MAX_TOTAL_BYTES)

/*
 * Hooks the host program must provide.
 */

/**
 * Send one APPLICATION payload (magic-first) as an L3 DATA frame.
 *
 * `app` may already be the buffer m64p_reply_buffer() returned -- that is how every
 * PEEKV response arrives -- in which case the payload is in place and must not be
 * copied onto itself. Other replies are small and built on the stack.
 */
void m64p_transport_send(const uint8_t *app, int app_len);

/**
 * Where to build a reply, so it can be sent without a copy: room for at least
 * M64P_APP_CAP bytes, normally just past the host's own frame header in its transmit
 * buffer. It must not overlap the payload passed to m64p_handle(), which is still
 * being read while the reply is written.
 *
 * A hook rather than a buffer of this module's own because a game-resident agent
 * already owns a transmit buffer that size. Keeping a second one cost ~8 KB of static
 * RAM, plus a copy of up to 8 KB per response, in games that may have no RAM to spare.
 */
uint8_t *m64p_reply_buffer(void);

/** RDRAM size in bytes, for range checks. */
uint32_t m64p_rdram_size(void);

/**
 * Base of RDRAM as the CPU should see it — cached KSEG0 on hardware (spec §4.1).
 * A hook rather than a constant so the protocol logic can be exercised off-target
 * against an ordinary buffer.
 */
volatile uint8_t *m64p_rdram_base(void);

/**
 * Bytes of cartridge ROM that PEEKROM may address, counted from ROM offset 0 (PI address
 * 0x10000000). Return 0 when this host cannot read the cart: HELLO_ACK then leaves
 * M64P_FLAG_CART_ROM clear and PEEKROM is answered with E_UNSUPPORTED.
 *
 * An upper bound on what can be read, not the size of the image the console booted:
 * nothing on the console records that.
 */
uint32_t m64p_cart_rom_size(void);

/**
 * Copy `len` bytes of cartridge ROM, from ROM offset `off`, to `dst`. `off` and `len` need
 * not be aligned, and are already range-checked against m64p_cart_rom_size().
 *
 * Called from the same per-frame hook as everything else here, so it runs between the
 * game's own PI transfers and must share the bus with them the way the cart driver does.
 * Return 1 on success; 0 if the PI stayed busy, in which case `dst` is incomplete and the
 * host is told E_BUSY.
 */
int m64p_cart_rom_read(uint32_t off, uint8_t *dst, uint32_t len);

/**
 * Handle one APPLICATION payload whose magic is `M64P`. Returns 1 if the payload
 * was an M64P message (handled or answered with `ERR`), 0 if it was not ours.
 */
int m64p_handle(const uint8_t *p, size_t plen);

/**
 * Read every watched slot and queue what changed (spec §4.3).
 *
 * **Call this once per frame, from the same hook as m64p_handle(), whether or not a
 * request arrived.** That is the whole feature: a host cannot read a slot often enough to
 * see a value the next frame overwrites, and this can. A host that sets a watch is
 * promised a per-frame sample, so a program that cannot make this call every frame must
 * report no watch support (leave M64P_FLAG_WATCH clear) rather than call it less often --
 * a host cannot tell a slow sampler from a quiet game.
 *
 * Costs nothing worth measuring while no slot is watched, which is the state after HELLO.
 */
void m64p_watch_tick(void);

/** Slots watched now, and events dropped by a full queue since the last WATCH. */
uint8_t m64p_get_watching(void);
uint16_t m64p_get_watch_dropped(void);

/** Requests served since reset — HELLO, PEEKV, POKEV and PEEKROM all count. */
uint32_t m64p_get_requests(void);
/** Total bytes returned by PEEKV and PEEKROM, and consumed by POKEV. */
uint32_t m64p_get_bytes_read(void);
uint32_t m64p_get_bytes_written(void);
/** Requests rejected with ERR. */
uint32_t m64p_get_errors(void);
/** Last error code sent (0 if none), for the HUD. */
uint8_t m64p_get_last_error(void);

void m64p_reset_stats(void);

#endif
