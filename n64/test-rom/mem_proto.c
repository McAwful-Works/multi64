/**
 * M64P — RDRAM peek/poke over L3 APPLICATION.
 * See docs/spec/memory-l3-application-v0.md.
 *
 * No libdragon here on purpose (see mem_proto.h). Nothing in this file knows what
 * game is running or what any address means.
 */
#include "mem_proto.h"

#include <string.h>

static uint32_t s_requests;
static uint32_t s_bytes_read;
static uint32_t s_bytes_written;
static uint32_t s_errors;
static uint8_t s_last_error;

/* One request in, one response out; both bounded by M64P_APP_CAP. Static rather
   than stack because a game-resident agent runs on whatever thread stack the hook
   site happens to have, which may be small. */
static uint8_t s_out[M64P_APP_CAP];

/*
 * RDRAM access.
 *
 * Cached KSEG0, not KSEG1: the game manipulates its structures with the CPU, so
 * cached access is what stays coherent. Uncached reads can miss data the CPU has
 * not written back yet. Spec §4.1.
 */
static volatile uint8_t *rdram_at(uint32_t addr)
{
    return m64p_rdram_base() + addr;
}

static uint16_t read_be16(const uint8_t *p)
{
    return (uint16_t)(((uint16_t)p[0] << 8) | (uint16_t)p[1]);
}

static uint32_t read_be32(const uint8_t *p)
{
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | (uint32_t)p[3];
}

static void put_be16(uint8_t *p, uint16_t v)
{
    p[0] = (uint8_t)(v >> 8);
    p[1] = (uint8_t)(v & 0xFFU);
}

static int app_header(uint8_t *out, uint8_t msg)
{
    out[0] = M64P_MAGIC0;
    out[1] = M64P_MAGIC1;
    out[2] = M64P_MAGIC2;
    out[3] = M64P_MAGIC3;
    out[4] = msg;
    return 5;
}

static void send_err(uint16_t rid, uint8_t code)
{
    uint8_t app[5 + 3];
    int n = app_header(app, M64P_MSG_ERR);
    put_be16(app + n, rid);
    app[n + 2] = code;
    s_errors++;
    s_last_error = code;
    m64p_transport_send(app, n + 3);
}

static void handle_hello(void)
{
    uint8_t app[5 + 8];
    int n = app_header(app, M64P_MSG_HELLO_ACK);
    app[n] = (uint8_t)M64P_PROTO_VERSION;
    put_be16(app + n + 1, 0x0100U); /* agent_ver 1.0 */
    uint32_t sz = m64p_rdram_size();
    app[n + 3] = (uint8_t)(sz >> 24);
    app[n + 4] = (uint8_t)(sz >> 16);
    app[n + 5] = (uint8_t)(sz >> 8);
    app[n + 6] = (uint8_t)sz;
    app[n + 7] = (uint8_t)M64P_FLAG_WRITABLE;
    m64p_transport_send(app, n + 8);
}

/**
 * Walk the region list of a PEEKV or POKEV, validating as we go.
 *
 * `stride_includes_data` distinguishes the two: POKEV carries `len` bytes inline
 * after each header, PEEKV does not. Returns 0 on success, or an M64P_ERR_* code.
 * On success `*total_out` is the summed region length.
 */
static uint8_t validate_regions(const uint8_t *body, size_t body_len, uint8_t n, int stride_includes_data,
                                uint32_t *total_out)
{
    uint32_t total = 0;
    size_t off = 3; /* rid:u16 + n:u8 */
    uint32_t ram = m64p_rdram_size();

    if (n > (uint8_t)M64P_MAX_REGIONS) {
        return M64P_ERR_TOO_MANY;
    }

    for (uint8_t i = 0; i < n; i++) {
        if (off + 6U > body_len) {
            return M64P_ERR_MALFORMED;
        }
        uint32_t addr = read_be32(body + off);
        uint16_t len = read_be16(body + off + 4);
        off += 6U;

        if (len > (uint16_t)M64P_MAX_REGION_BYTES) {
            return M64P_ERR_TOO_LARGE;
        }
        total += (uint32_t)len;
        if (total > (uint32_t)M64P_MAX_TOTAL_BYTES) {
            return M64P_ERR_TOO_LARGE;
        }
        /* Range-check rather than fault: a bad address from the host would
           otherwise bus-error the console. Written to avoid overflow. */
        if (addr > ram || (uint32_t)len > ram - addr) {
            return M64P_ERR_RANGE;
        }
        if (stride_includes_data) {
            if (off + (size_t)len > body_len) {
                return M64P_ERR_MALFORMED;
            }
            off += (size_t)len;
        }
    }

    *total_out = total;
    return 0U;
}

static void handle_peekv(const uint8_t *body, size_t body_len)
{
    if (body_len < 3U) {
        send_err(0U, M64P_ERR_MALFORMED);
        return;
    }
    uint16_t rid = read_be16(body);
    uint8_t n = body[2];

    uint32_t total = 0;
    uint8_t err = validate_regions(body, body_len, n, 0, &total);
    if (err != 0U) {
        send_err(rid, err);
        return;
    }

    int out = app_header(s_out, M64P_MSG_PEEKV_RESP);
    put_be16(s_out + out, rid);
    s_out[out + 2] = n;
    out += 3;

    size_t off = 3;
    for (uint8_t i = 0; i < n; i++) {
        uint32_t addr = read_be32(body + off);
        uint16_t len = read_be16(body + off + 4);
        off += 6U;

        put_be16(s_out + out, len);
        out += 2;

        volatile uint8_t *src = rdram_at(addr);
        for (uint16_t j = 0; j < len; j++) {
            s_out[out + j] = src[j];
        }
        out += (int)len;
    }

    s_requests++;
    s_bytes_read += total;
    m64p_transport_send(s_out, out);
}

static void handle_pokev(const uint8_t *body, size_t body_len)
{
    if (body_len < 3U) {
        send_err(0U, M64P_ERR_MALFORMED);
        return;
    }
    uint16_t rid = read_be16(body);
    uint8_t n = body[2];

    uint32_t total = 0;
    uint8_t err = validate_regions(body, body_len, n, 1, &total);
    if (err != 0U) {
        send_err(rid, err);
        return;
    }

    /* Validated in full before the first byte lands, so a malformed tail cannot
       leave RDRAM half-written. */
    size_t off = 3;
    for (uint8_t i = 0; i < n; i++) {
        uint32_t addr = read_be32(body + off);
        uint16_t len = read_be16(body + off + 4);
        off += 6U;

        volatile uint8_t *dst = rdram_at(addr);
        for (uint16_t j = 0; j < len; j++) {
            dst[j] = body[off + j];
        }
        off += (size_t)len;
    }

    uint8_t app[5 + 3];
    int an = app_header(app, M64P_MSG_POKE_ACK);
    put_be16(app + an, rid);
    app[an + 2] = n;

    s_requests++;
    s_bytes_written += total;
    m64p_transport_send(app, an + 3);
}

int m64p_handle(const uint8_t *p, size_t plen)
{
    if (plen < 5U) {
        return 0;
    }
    if (p[0] != M64P_MAGIC0 || p[1] != M64P_MAGIC1 || p[2] != M64P_MAGIC2 || p[3] != M64P_MAGIC3) {
        return 0;
    }

    uint8_t msg = p[4];
    const uint8_t *body = p + 5;
    size_t body_len = plen - 5U;

    switch (msg) {
    case M64P_MSG_HELLO:
        s_requests++;
        handle_hello();
        break;
    case M64P_MSG_PEEKV:
        handle_peekv(body, body_len);
        break;
    case M64P_MSG_POKEV:
        handle_pokev(body, body_len);
        break;
    default:
        /* Unknown request: answer rather than go quiet, so a host driving a newer
           protocol sees a refusal instead of a timeout. */
        send_err(body_len >= 2U ? read_be16(body) : 0U, M64P_ERR_MALFORMED);
        break;
    }
    return 1;
}

uint32_t m64p_get_requests(void)
{
    return s_requests;
}

uint32_t m64p_get_bytes_read(void)
{
    return s_bytes_read;
}

uint32_t m64p_get_bytes_written(void)
{
    return s_bytes_written;
}

uint32_t m64p_get_errors(void)
{
    return s_errors;
}

uint8_t m64p_get_last_error(void)
{
    return s_last_error;
}

void m64p_reset_stats(void)
{
    s_requests = 0;
    s_bytes_read = 0;
    s_bytes_written = 0;
    s_errors = 0;
    s_last_error = 0;
}
