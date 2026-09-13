/**
 * Game-resident M64P agent.
 *
 * agent_tick() is called once per frame from a hook the integration adds to the
 * host ROM. It polls the cart for an L3 frame, hands any APPLICATION payload to
 * mem_proto.c, and sends the reply. Everything specific to a game lives in the hook
 * site and the placement, not here; this file knows only about frames and bytes.
 *
 * See docs/integration/cart-agent.md for the contract, and sc64.c for the two
 * hardware questions this cannot answer on its own: where packets are staged, and
 * how PI access coexists with the game's own ROM traffic.
 */
#include "m64p_types.h"
#include "mem_proto.h"
#include "agent.h"

/*
 * The cart driver is chosen at build time (Makefile CART=). SummerCart64 is the default and the
 * only driver that has run on hardware; its build compiles none of the EverDrive code below.
 * The EverDrive drivers are experimental and have never run on a cart.
 */
#if defined(AGENT_CART_ED64) && defined(AGENT_CART_ED64PRO)
#error "define at most one of AGENT_CART_ED64 and AGENT_CART_ED64PRO"
#elif defined(AGENT_CART_ED64)
#include "ed64.h"
#define AGENT_STREAM_CART 1
#define CART_INIT() ed64_init()
#define CART_RECEIVE(dst, cap) ed64_receive((dst), (cap))
#define CART_SEND(data, len) ed64_send((data), (len))
#elif defined(AGENT_CART_ED64PRO)
#include "ed64pro.h"
#define AGENT_STREAM_CART 1
#define CART_INIT() ed64pro_init()
#define CART_RECEIVE(dst, cap) ed64pro_receive((dst), (cap))
#define CART_SEND(data, len) ed64pro_send((data), (len))
#else
#include "sc64.h"
#endif

/* ---- L3 framing (l3-bridge-protocol-v1.md) ------------------------------ */

#define L3_MAGIC0 0x4Du /* 'M' */
#define L3_MAGIC1 0x36u /* '6' */
#define L3_MAGIC2 0x34u /* '4' */
#define L3_MAGIC3 0x42u /* 'B' */

#define L3_HEADER_LEN 16
#define L3_TYPE_DATA 0x10u
#define L3_CH_APPLICATION 0x00u
#define L3_FLAG_FINAL 0x0001u

/*
 * Buffers.
 *
 * Sized for one M64P exchange: the protocol caps a request at 7936 bytes plus
 * headers, and both directions fit inside a single L3 frame by construction.
 * Static rather than stack -- this runs on whatever thread stack the hook site
 * happens to have, which may be small.
 *
 * The +16 slack absorbs the even-length rounding PI DMA requires.
 */
#define AGENT_RX_CAP (L3_HEADER_LEN + M64P_APP_CAP + 256 + 16)
#define AGENT_TX_CAP (L3_HEADER_LEN + M64P_APP_CAP + 16)

static uint8_t s_rx[AGENT_RX_CAP];
static uint8_t s_tx[AGENT_TX_CAP];

static uint32_t s_ticks;
static uint32_t s_frames_handled;
static int s_ready;

static uint32_t be32(const uint8_t *p)
{
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | (uint32_t)p[3];
}

static void put_be32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)(v >> 16);
    p[2] = (uint8_t)(v >> 8);
    p[3] = (uint8_t)v;
}

static void put_be16(uint8_t *p, uint16_t v)
{
    p[0] = (uint8_t)(v >> 8);
    p[1] = (uint8_t)v;
}

/* ---- hooks mem_proto.c declares ----------------------------------------- */

void m64p_transport_send(const uint8_t *app, int app_len)
{
    int i;
    int total;

    if (app_len <= 0 || (int)(L3_HEADER_LEN + app_len) > (int)AGENT_TX_CAP) {
        return;
    }

    s_tx[0] = L3_MAGIC0;
    s_tx[1] = L3_MAGIC1;
    s_tx[2] = L3_MAGIC2;
    s_tx[3] = L3_MAGIC3;
    s_tx[4] = L3_TYPE_DATA;
    s_tx[5] = L3_CH_APPLICATION;
    put_be16(&s_tx[6], L3_FLAG_FINAL);
    put_be32(&s_tx[8], 0u); /* request_id: correlation is M64P's rid */
    put_be32(&s_tx[12], (uint32_t)app_len);

    /* PEEKV responses are built in place (m64p_reply_buffer); only the small
       replies mem_proto builds on its stack need copying in behind the header. */
    if (app != &s_tx[L3_HEADER_LEN]) {
        for (i = 0; i < app_len; i++) {
            s_tx[L3_HEADER_LEN + i] = app[i];
        }
    }

    total = L3_HEADER_LEN + app_len;
#ifdef AGENT_STREAM_CART
    (void)CART_SEND(s_tx, (uint32_t)total);
#else
    (void)sc64_write(SC64_DATATYPE_L3, s_tx, (uint32_t)total);
#endif
}

uint8_t *m64p_reply_buffer(void)
{
    /* Just past the L3 header, so a PEEKV response is already framed for sending.
       s_tx never overlaps s_rx, where the request is still being read. */
    return &s_tx[L3_HEADER_LEN];
}

uint32_t m64p_rdram_size(void)
{
    /*
     * Read from the boot-time memory size word rather than hardcoding, so a
     * 4 MiB console reports honestly and every M64P request is range-checked
     * against the truth.
     */
    uint32_t sz = *(volatile uint32_t *)0xA0000318u;
    if (sz == 0u || sz > 0x00800000u) {
        return 0x00800000u;
    }
    return sz;
}

volatile uint8_t *m64p_rdram_base(void)
{
    /* Cached KSEG0: the game touches its structures with the CPU, so cached
       access is what stays coherent. See memory-l3-application-v0.md 4.1. */
    return (volatile uint8_t *)0x80000000u;
}

#ifdef AGENT_STREAM_CART
/* ---- L3 reassembly (EverDrive builds only) -------------------------------
 *
 * An SC64 packet carries a whole frame. The EverDrives do not: the X7 host sends
 * 512-byte USB messages and the PRO host 1024-byte FIFO writes a few frames apart
 * (l3-over-everdrive-x7.md 4, l3-over-everdrive-pro.md 5), so one M64P request
 * arrives over several receives and often several ticks.
 *
 * s_rx holds what has arrived. stream_frame() returns a complete frame at its
 * start, and the next call drops it -- after agent_tick() has handled it, or
 * decided not to.
 */

/** Receives per tick: a whole request from the X7 (16 x 512 bytes) fits in one. */
#define AGENT_STREAM_PULLS 32u

/** Ticks a partial frame may wait for more bytes before it is thrown away. */
#define AGENT_STREAM_STALE_TICKS 60u

static uint32_t s_rx_len;
static uint32_t s_rx_done;
static uint32_t s_rx_idle_ticks;

static void rx_drop(uint32_t n)
{
    uint32_t i;

    if (n >= s_rx_len) {
        s_rx_len = 0u;
        return;
    }
    for (i = n; i < s_rx_len; i++) {
        s_rx[i - n] = s_rx[i];
    }
    s_rx_len -= n;
}

/** 1 if a frame magic, or the start of one cut off by the end of the data, begins at `i`. */
static int rx_magic_at(uint32_t i)
{
    static const uint8_t magic[4] = { L3_MAGIC0, L3_MAGIC1, L3_MAGIC2, L3_MAGIC3 };
    uint32_t k;

    for (k = 0u; k < 4u && i + k < s_rx_len; k++) {
        if (s_rx[i + k] != magic[k]) {
            return 0;
        }
    }
    return 1;
}

/** Length of the complete frame at the start of s_rx, dropping anything before it; 0 if none yet. */
static uint32_t rx_frame_len(void)
{
    for (;;) {
        uint32_t i = 0u;
        uint32_t payload_len;

        while (i < s_rx_len && !rx_magic_at(i)) {
            i++;
        }
        rx_drop(i);
        if (s_rx_len < L3_HEADER_LEN) {
            return 0u;
        }
        payload_len = be32(&s_rx[12]);
        if (payload_len > AGENT_RX_CAP - L3_HEADER_LEN) {
            /* Not a frame this agent could hold: look past this magic for the next one. */
            rx_drop(1u);
            continue;
        }
        if (s_rx_len < L3_HEADER_LEN + payload_len) {
            return 0u;
        }
        return L3_HEADER_LEN + payload_len;
    }
}

static uint32_t stream_frame(void)
{
    uint32_t pulls;
    uint32_t len;
    int arrived = 0;

    rx_drop(s_rx_done);
    s_rx_done = 0u;

    for (pulls = 0u; pulls < AGENT_STREAM_PULLS; pulls++) {
        uint32_t n;

        if (s_rx_len >= AGENT_RX_CAP || rx_frame_len() != 0u) {
            break;
        }
        n = CART_RECEIVE(&s_rx[s_rx_len], AGENT_RX_CAP - s_rx_len);
        if (n == 0u) {
            break;
        }
        s_rx_len += n;
        arrived = 1;
    }

    len = rx_frame_len();
    if (len != 0u) {
        s_rx_done = len;
        s_rx_idle_ticks = 0u;
        return len;
    }
    /* A request cut short by a lost piece would otherwise swallow the host's retry. */
    if (arrived || s_rx_len == 0u) {
        s_rx_idle_ticks = 0u;
    } else if (++s_rx_idle_ticks >= AGENT_STREAM_STALE_TICKS) {
        s_rx_len = 0u;
        s_rx_idle_ticks = 0u;
    }
    return 0u;
}
#endif

/* ---- per-frame entry point ---------------------------------------------- */

/**
 * Called once per frame from the host ROM's hook.
 *
 * Deliberately cheap on the common path: one SC64 status command, and a return
 * when the host has sent nothing. Only a frame that actually carries a request
 * does any copying.
 */
/* Initialisation attempts before giving up for the rest of the boot. A cart that
   is there answers on the first one; this only allows for one still waking up. */
#define AGENT_INIT_ATTEMPTS 16

static uint32_t s_init_attempts;

void agent_tick(void)
{
#ifndef AGENT_STREAM_CART
    uint8_t datatype = 0;
#endif
    uint32_t size;
    uint32_t payload_len;

    s_ticks++;

    if (!s_ready) {
        /* Dormancy has to be PERMANENT, not per-frame.
         *
         * This once called sc64_init() again on every frame it failed, which is
         * four PI accesses and eight spins on PI_STATUS, sixty times a second,
         * forever -- on any machine without an SC64 that is every frame the game
         * ever runs. It hard locked a game at room loads, where the game's own DMA
         * holds the bus longest, and it reproduced in an emulator because there
         * the retry can never succeed.
         *
         * A cart that is present answers immediately. A handful of attempts
         * covers a cart still coming up; past that, staying quiet for the rest of
         * the boot is the whole point of being dormant. */
        if (s_init_attempts >= AGENT_INIT_ATTEMPTS) {
            return;
        }
        s_init_attempts++;
#ifdef AGENT_STREAM_CART
        s_ready = CART_INIT();
#else
        s_ready = sc64_init();
#endif
        if (!s_ready) {
            return; /* not an SC64, or not answering: stay dormant */
        }
    }

#ifdef AGENT_STREAM_CART
    size = stream_frame();
    if (size == 0u) {
        return;
    }
#else
    size = sc64_poll(&datatype);
    if (size == 0u) {
        return;
    }
    if (datatype != SC64_DATATYPE_L3 || size < L3_HEADER_LEN || size > AGENT_RX_CAP) {
        return;
    }

    sc64_read(s_rx, 0u, size);
#endif

    if (s_rx[0] != L3_MAGIC0 || s_rx[1] != L3_MAGIC1 || s_rx[2] != L3_MAGIC2 || s_rx[3] != L3_MAGIC3) {
        return;
    }
    if (s_rx[4] != L3_TYPE_DATA || s_rx[5] != L3_CH_APPLICATION) {
        return;
    }

    payload_len = be32(&s_rx[12]);
    if (payload_len == 0u || payload_len > size - L3_HEADER_LEN) {
        return;
    }

    /*
     * One packet, one frame. The test ROM reassembles a byte stream across USB
     * reads because M64T lets the host fragment; M64P requests fit in a single
     * frame by construction, so the agent takes the simpler path and drops
     * anything that does not. If fragmentation ever shows up on hardware this is
     * where it has to be handled. (The EverDrive builds already reassemble: see
     * stream_frame().)
     */
    if (m64p_handle(&s_rx[L3_HEADER_LEN], (size_t)payload_len)) {
        s_frames_handled++;
    }
}

/* Small, stable surface for a debug hook or an on-screen counter. */
uint32_t agent_get_ticks(void)
{
    return s_ticks;
}

uint32_t agent_get_frames_handled(void)
{
    return s_frames_handled;
}

int agent_is_ready(void)
{
    return s_ready;
}
