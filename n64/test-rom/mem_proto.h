/**
 * L3 APPLICATION RDRAM peek/poke (M64P) — see docs/spec/memory-l3-application-v0.md
 *
 * Deliberately free of libdragon: the only outside world this module knows is the
 * two hooks below. That is what lets the same source be lifted into a game-resident
 * agent (libultra, no libdragon) without editing the protocol handling.
 */
#ifndef MULTI64_MEM_PROTO_H
#define MULTI64_MEM_PROTO_H

#include <stddef.h>
#include <stdint.h>

#define M64P_MAGIC0 0x4DU
#define M64P_MAGIC1 0x36U
#define M64P_MAGIC2 0x34U
#define M64P_MAGIC3 0x50U

#define M64P_MSG_HELLO 0x01U
#define M64P_MSG_PEEKV 0x02U
#define M64P_MSG_POKEV 0x03U

#define M64P_MSG_HELLO_ACK 0x81U
#define M64P_MSG_PEEKV_RESP 0x82U
#define M64P_MSG_POKE_ACK 0x83U
#define M64P_MSG_ERR 0xE0U

#define M64P_ERR_MALFORMED 0x01U
#define M64P_ERR_TOO_MANY 0x02U
#define M64P_ERR_TOO_LARGE 0x03U
#define M64P_ERR_RANGE 0x04U
#define M64P_ERR_READONLY 0x05U

/** Protocol revision reported in `HELLO_ACK`. */
#define M64P_PROTO_VERSION 0U
/** `HELLO_ACK` flags bit 0: this agent accepts `POKEV`. */
#define M64P_FLAG_WRITABLE 0x01U

/** Spec §4 limits. Exceeding any of them is an error, never a truncation. */
#define M64P_MAX_REGIONS 32
#define M64P_MAX_REGION_BYTES 4096
/* Worst case on the wire is a POKEV *request* (6-byte region header vs 2 in a
   PEEKV response): 8 + 6n + total. At n = 32 that leaves 7992 under an 8192-byte
   L3 payload, so one usb_write still carries the whole frame. */
#define M64P_MAX_TOTAL_BYTES 7936

/** Largest M64P application payload this module will build (magic + msg + body). */
#define M64P_APP_CAP (5 + 3 + (M64P_MAX_REGIONS * 2) + M64P_MAX_TOTAL_BYTES)

/*
 * Hooks the host program must provide.
 */

/** Send one APPLICATION payload (magic-first) as an L3 DATA frame. */
void m64p_transport_send(const uint8_t *app, int app_len);

/** RDRAM size in bytes, for range checks. */
uint32_t m64p_rdram_size(void);

/**
 * Base of RDRAM as the CPU should see it — cached KSEG0 on hardware (spec §4.1).
 * A hook rather than a constant so the protocol logic can be exercised off-target
 * against an ordinary buffer.
 */
volatile uint8_t *m64p_rdram_base(void);

/**
 * Handle one APPLICATION payload whose magic is `M64P`. Returns 1 if the payload
 * was an M64P message (handled or answered with `ERR`), 0 if it was not ours.
 */
int m64p_handle(const uint8_t *p, size_t plen);

/** Requests served since reset — HELLO, PEEKV and POKEV all count. */
uint32_t m64p_get_requests(void);
/** Total bytes returned by PEEKV and consumed by POKEV. */
uint32_t m64p_get_bytes_read(void);
uint32_t m64p_get_bytes_written(void);
/** Requests rejected with ERR. */
uint32_t m64p_get_errors(void);
/** Last error code sent (0 if none), for the HUD. */
uint8_t m64p_get_last_error(void);

void m64p_reset_stats(void);

#endif
