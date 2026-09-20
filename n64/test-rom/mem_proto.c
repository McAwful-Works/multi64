/**
 * M64P — RDRAM peek/poke over L3 APPLICATION.
 * See docs/spec/memory-l3-application-v0.md.
 *
 * No libdragon here on purpose (see mem_proto.h). Nothing in this file knows what
 * game is running or what any address means.
 */
#include "mem_proto.h"


/*
 * Watched slots (spec 4.3).
 *
 * A game that records an event in one place the next event overwrites gives a host
 * polling over USB no way to see them all: it reads the slot once per exchange at best.
 * This runs every frame, where the game writes them. What it sees goes in one queue and
 * rides back on responses that were already being sent.
 *
 * Nothing here knows what a slot means. The address, the length and the filter are the
 * host's; the agent only reports that the bytes changed.
 */
struct watch_slot {
    uint32_t addr;
    uint8_t len;
    uint8_t at;      /* filter: the byte to test... */
    uint8_t nvalues; /* ...against these, or none to keep every change */
    uint8_t values[M64P_WATCH_MAX_VALUES];
    uint8_t last[M64P_WATCH_MAX_LEN];
    uint8_t have_last; /* the first read is a baseline, not an event */
};

struct watch_event {
    uint8_t slot;
    uint8_t len;
    uint8_t bytes[M64P_WATCH_MAX_LEN];
};

static struct watch_slot s_watch[M64P_WATCH_SLOTS];
static uint8_t s_watching;
static struct watch_event s_events[M64P_WATCH_QUEUE];
static uint8_t s_events_first;
static uint8_t s_events_count;
static uint16_t s_events_dropped;

static uint32_t s_requests;
static uint32_t s_bytes_read;
static uint32_t s_bytes_written;
static uint32_t s_errors;
static uint8_t s_last_error;

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

static void put_be32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)(v >> 16);
    p[2] = (uint8_t)(v >> 8);
    p[3] = (uint8_t)v;
}

static void watch_clear(void)
{
    s_watching = 0;
    s_events_first = 0;
    s_events_count = 0;
    s_events_dropped = 0;
}

static void handle_hello(void)
{
    uint8_t app[5 + 13];
    int n;
    int len = 8;
    uint32_t rom = m64p_cart_rom_size();
    uint8_t flags = (uint8_t)M64P_FLAG_WRITABLE;

    n = app_header(app, M64P_MSG_HELLO_ACK);
    app[n] = (uint8_t)M64P_PROTO_VERSION;
    put_be16(app + n + 1, 0x0100U); /* agent_ver 1.0 */
    put_be32(app + n + 3, m64p_rdram_size());
    if (rom != 0U) {
        /* rom_bytes follows the flags only when bit 1 says so, so a host that predates it
           still finds every field where it expects. */
        flags |= (uint8_t)M64P_FLAG_CART_ROM;
        put_be32(app + n + 8, rom);
        len = 12;
    }
    /* Then watch_slots, in bit order: a host reads the appended fields in the order of
       the bits that announce them. */
    flags |= (uint8_t)M64P_FLAG_WATCH;
    app[n + len] = (uint8_t)M64P_WATCH_SLOTS;
    len++;
    app[n + 7] = flags;

    /* A HELLO is a new host, or the same one starting again: it has not said what to
       watch yet, and events from before it arrived are not its to read. */
    watch_clear();
    m64p_transport_send(app, n + len);
}

/**
 * Walk the region list of a PEEKV or POKEV, validating as we go.
 *
 * `stride_includes_data` distinguishes the two: POKEV carries `len` bytes inline
 * after each header, PEEKV and PEEKROM do not. `space` is the size of the address space
 * the regions index (RDRAM, or the cart ROM window). Returns 0 on success, or an
 * M64P_ERR_* code.
 * On success `*total_out` is the summed region length.
 */
static uint8_t validate_regions(const uint8_t *body, size_t body_len, uint8_t n, int stride_includes_data,
                                uint32_t space, uint32_t *total_out)
{
    uint32_t total = 0;
    size_t off = 3; /* rid:u16 + n:u8 */
    uint8_t i;

    if (n > (uint8_t)M64P_MAX_REGIONS) {
        return M64P_ERR_TOO_MANY;
    }

    for (i = 0; i < n; i++) {
        uint32_t addr;
        uint16_t len;

        if (off + 6U > body_len) {
            return M64P_ERR_MALFORMED;
        }
        addr = read_be32(body + off);
        len = read_be16(body + off + 4);
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
        if (addr > space || (uint32_t)len > space - addr) {
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

/** Queue one event, dropping the oldest when the queue is full. */
static void watch_push(uint8_t slot, const uint8_t *bytes, uint8_t len)
{
    uint8_t at;
    uint8_t i;

    if (s_events_count == (uint8_t)M64P_WATCH_QUEUE) {
        s_events_first = (uint8_t)((s_events_first + 1U) % (uint8_t)M64P_WATCH_QUEUE);
        s_events_count--;
        if (s_events_dropped != 0xFFFFU) {
            s_events_dropped++;
        }
    }
    at = (uint8_t)((s_events_first + s_events_count) % (uint8_t)M64P_WATCH_QUEUE);
    s_events[at].slot = slot;
    s_events[at].len = len;
    for (i = 0; i < len; i++) {
        s_events[at].bytes[i] = bytes[i];
    }
    s_events_count++;
}

void m64p_watch_tick(void)
{
    uint8_t i;

    for (i = 0; i < s_watching; i++) {
        struct watch_slot *w = &s_watch[i];
        uint8_t now[M64P_WATCH_MAX_LEN];
        volatile uint8_t *src = rdram_at(w->addr);
        int changed = 0;
        int zero = 1;
        uint8_t keep;
        uint8_t j;

        for (j = 0; j < w->len; j++) {
            now[j] = src[j];
            if (now[j] != w->last[j]) {
                changed = 1;
            }
            if (now[j] != 0U) {
                zero = 0;
            }
            w->last[j] = now[j];
        }
        if (!w->have_last) {
            /* Whatever was in the slot before the host asked is not an event. */
            w->have_last = 1;
            continue;
        }
        /* A slot going to zero is the game finishing with it, not something happening. */
        if (!changed || zero) {
            continue;
        }
        keep = (uint8_t)(w->nvalues == 0U);
        for (j = 0; j < w->nvalues; j++) {
            if (now[w->at] == w->values[j]) {
                keep = 1;
            }
        }
        if (keep) {
            watch_push(i, now, w->len);
        }
    }
}

/**
 * Write as many queued events as `room` allows, oldest first, and remove them.
 *
 * Returns the bytes written, or 0 when the trailer does not fit at all -- the events stay
 * queued and ride the next response. Only called while a slot is watched, so a host that
 * asked for nothing never sees these bytes.
 */
static int watch_trailer(uint8_t *out, int room)
{
    int at = 3; /* n:u8 + dropped:u16 */
    uint8_t n = 0;

    if (room < at) {
        return 0;
    }
    while (n < s_events_count) {
        const struct watch_event *e =
            &s_events[(s_events_first + n) % (uint8_t)M64P_WATCH_QUEUE];
        uint8_t j;

        if (at + 2 + (int)e->len > room) {
            break;
        }
        out[at] = e->slot;
        out[at + 1] = e->len;
        for (j = 0; j < e->len; j++) {
            out[at + 2 + j] = e->bytes[j];
        }
        at += 2 + (int)e->len;
        n++;
    }
    out[0] = n;
    put_be16(out + 1, s_events_dropped);
    s_events_first = (uint8_t)((s_events_first + n) % (uint8_t)M64P_WATCH_QUEUE);
    s_events_count = (uint8_t)(s_events_count - n);
    return at;
}

static void handle_watch(const uint8_t *body, size_t body_len)
{
    uint8_t app[5 + 3];
    struct watch_slot next[M64P_WATCH_SLOTS];
    uint16_t rid;
    uint8_t n;
    size_t off = 3;
    uint8_t i;
    int an;

    if (body_len < 3U) {
        send_err(0U, M64P_ERR_MALFORMED);
        return;
    }
    rid = read_be16(body);
    n = body[2];
    if (n > (uint8_t)M64P_WATCH_SLOTS) {
        send_err(rid, M64P_ERR_TOO_MANY);
        return;
    }

    /* Built aside and only then installed, so a request rejected part way through leaves
       the slots the host set last time exactly as they were. */
    for (i = 0; i < n; i++) {
        uint8_t len;
        uint8_t nvalues;
        uint8_t j;

        if (body_len < off + 7U) {
            send_err(rid, M64P_ERR_MALFORMED);
            return;
        }
        len = body[off + 4];
        nvalues = body[off + 6];
        if (len == 0U || len > (uint8_t)M64P_WATCH_MAX_LEN ||
            nvalues > (uint8_t)M64P_WATCH_MAX_VALUES) {
            send_err(rid, M64P_ERR_TOO_LARGE);
            return;
        }
        if (body_len < off + 7U + (size_t)nvalues) {
            send_err(rid, M64P_ERR_MALFORMED);
            return;
        }
        next[i].addr = read_be32(body + off);
        next[i].len = len;
        next[i].at = body[off + 5];
        next[i].nvalues = nvalues;
        /* A filter on a byte outside the slot could never match. */
        if (nvalues != 0U && next[i].at >= len) {
            send_err(rid, M64P_ERR_RANGE);
            return;
        }
        if ((uint32_t)(next[i].addr + (uint32_t)len) > m64p_rdram_size() ||
            next[i].addr > m64p_rdram_size()) {
            send_err(rid, M64P_ERR_RANGE);
            return;
        }
        for (j = 0; j < nvalues; j++) {
            next[i].values[j] = body[off + 7U + j];
        }
        for (j = 0; j < (uint8_t)M64P_WATCH_MAX_LEN; j++) {
            next[i].last[j] = 0;
        }
        next[i].have_last = 0;
        off += 7U + (size_t)nvalues;
    }

    /* A new list starts a new history: events queued for slots that may no longer exist,
       or now mean something else, are not an answer to the question just asked. */
    watch_clear();
    for (i = 0; i < n; i++) {
        s_watch[i] = next[i];
    }
    s_watching = n;

    s_requests++;
    an = app_header(app, M64P_MSG_WATCH_ACK);
    put_be16(app + an, rid);
    app[an + 2] = s_watching;
    m64p_transport_send(app, an + 3);
}

static void handle_peekv(const uint8_t *body, size_t body_len)
{
    uint16_t rid;
    uint8_t n;
    uint32_t total = 0;
    uint8_t err;
    uint8_t *reply;
    int out;
    size_t off;
    uint8_t i;

    if (body_len < 3U) {
        send_err(0U, M64P_ERR_MALFORMED);
        return;
    }
    rid = read_be16(body);
    n = body[2];

    err = validate_regions(body, body_len, n, 0, m64p_rdram_size(), &total);
    if (err != 0U) {
        send_err(rid, err);
        return;
    }

    /* Straight into the host's transmit buffer (see m64p_reply_buffer). Validation
       above bounds the response by M64P_APP_CAP, which is the room the hook
       promises; and the request is still being read below, which is why the hook
       must not overlap it. */
    reply = m64p_reply_buffer();
    out = app_header(reply, M64P_MSG_PEEKV_RESP);
    put_be16(reply + out, rid);
    reply[out + 2] = n;
    out += 3;

    off = 3;
    for (i = 0; i < n; i++) {
        uint32_t addr = read_be32(body + off);
        uint16_t len = read_be16(body + off + 4);
        volatile uint8_t *src;
        uint16_t j;

        off += 6U;

        put_be16(reply + out, len);
        out += 2;

        src = rdram_at(addr);
        for (j = 0; j < len; j++) {
            reply[out + j] = src[j];
        }
        out += (int)len;
    }

    if (s_watching != 0U) {
        out += watch_trailer(reply + out, M64P_APP_CAP - out);
    }

    s_requests++;
    s_bytes_read += total;
    m64p_transport_send(reply, out);
}

static void handle_pokev(const uint8_t *body, size_t body_len)
{
    uint16_t rid;
    uint8_t n;
    uint32_t total = 0;
    uint8_t err;
    size_t off;
    uint8_t i;
    uint8_t app[5 + 3];
    int an;

    if (body_len < 3U) {
        send_err(0U, M64P_ERR_MALFORMED);
        return;
    }
    rid = read_be16(body);
    n = body[2];

    err = validate_regions(body, body_len, n, 1, m64p_rdram_size(), &total);
    if (err != 0U) {
        send_err(rid, err);
        return;
    }

    /* Validated in full before the first byte lands, so a malformed tail cannot
       leave RDRAM half-written. */
    off = 3;
    for (i = 0; i < n; i++) {
        uint32_t addr = read_be32(body + off);
        uint16_t len = read_be16(body + off + 4);
        volatile uint8_t *dst;
        uint16_t j;

        off += 6U;

        dst = rdram_at(addr);
        for (j = 0; j < len; j++) {
            dst[j] = body[off + j];
        }
        off += (size_t)len;
    }

    an = app_header(app, M64P_MSG_POKE_ACK);
    put_be16(app + an, rid);
    app[an + 2] = n;

    s_requests++;
    s_bytes_written += total;
    m64p_transport_send(app, an + 3);
}

/**
 * PEEKROM: PEEKV's shape, over cartridge ROM instead of RDRAM. The ROM does not change
 * while the game runs, so there is no consistency question here, only the bus: every
 * byte comes through m64p_cart_rom_read(), which shares the PI with the game.
 */
static void handle_peekrom(const uint8_t *body, size_t body_len)
{
    uint16_t rid;
    uint8_t n;
    uint32_t total = 0;
    uint32_t space = m64p_cart_rom_size();
    uint8_t err;
    uint8_t *reply;
    int out;
    size_t off;
    uint8_t i;

    if (body_len < 3U) {
        send_err(0U, M64P_ERR_MALFORMED);
        return;
    }
    rid = read_be16(body);
    n = body[2];

    if (space == 0U) {
        send_err(rid, M64P_ERR_UNSUPPORTED);
        return;
    }
    err = validate_regions(body, body_len, n, 0, space, &total);
    if (err != 0U) {
        send_err(rid, err);
        return;
    }

    reply = m64p_reply_buffer();
    out = app_header(reply, M64P_MSG_PEEKROM_RESP);
    put_be16(reply + out, rid);
    reply[out + 2] = n;
    out += 3;

    off = 3;
    for (i = 0; i < n; i++) {
        uint32_t addr = read_be32(body + off);
        uint16_t len = read_be16(body + off + 4);

        off += 6U;
        put_be16(reply + out, len);
        out += 2;
        if (len != 0U && !m64p_cart_rom_read(addr, reply + out, (uint32_t)len)) {
            /* The half-built reply is abandoned; ERR is built on the stack. */
            send_err(rid, M64P_ERR_BUSY);
            return;
        }
        out += (int)len;
    }

    s_requests++;
    s_bytes_read += total;
    m64p_transport_send(reply, out);
}

int m64p_handle(const uint8_t *p, size_t plen)
{
    uint8_t msg;
    const uint8_t *body;
    size_t body_len;

    if (plen < 5U) {
        return 0;
    }
    if (p[0] != M64P_MAGIC0 || p[1] != M64P_MAGIC1 || p[2] != M64P_MAGIC2 || p[3] != M64P_MAGIC3) {
        return 0;
    }

    msg = p[4];
    body = p + 5;
    body_len = plen - 5U;

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
    case M64P_MSG_PEEKROM:
        handle_peekrom(body, body_len);
        break;
    case M64P_MSG_WATCH:
        handle_watch(body, body_len);
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

uint8_t m64p_get_watching(void)
{
    return s_watching;
}

uint16_t m64p_get_watch_dropped(void)
{
    return s_events_dropped;
}

void m64p_reset_stats(void)
{
    s_requests = 0;
    s_bytes_read = 0;
    s_bytes_written = 0;
    s_errors = 0;
    s_last_error = 0;
}
