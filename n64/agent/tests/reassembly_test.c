/*
 * Host-side test of the cart agent's L3 reassembly, which only the EverDrive builds use.
 *
 * `make host-test` compiles the real agent.c and mem_proto.c for the PC, once per EverDrive cart
 * (AGENT_CART_ED64, then AGENT_CART_ED64PRO), with this file standing in for the cart driver: it
 * hands the agent scripted pieces of the byte stream and records what the agent sends back.
 * Requests use an unknown M64P message type, which mem_proto answers with ERR echoing the request
 * id, so nothing touches RDRAM.
 *
 * This checks the byte-stream logic only. It says nothing about whether either driver works on a
 * cart.
 */
#undef NDEBUG
#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "agent.h"

#if defined(AGENT_CART_ED64PRO)
#include "ed64pro.h"
#define FAKE_INIT ed64pro_init
#define FAKE_RECEIVE ed64pro_receive
#define FAKE_SEND ed64pro_send
#define CART_NAME "ed64pro"
/* The PRO host writes the cart FIFO in blocks of up to 1024 bytes. */
#define HOST_PIECE 1024u
#elif defined(AGENT_CART_ED64)
#include "ed64.h"
#define FAKE_INIT ed64_init
#define FAKE_RECEIVE ed64_receive
#define FAKE_SEND ed64_send
#define CART_NAME "ed64"
/* The X7 host sends 512-byte DMA@ messages. */
#define HOST_PIECE 512u
#else
#error "build with -DAGENT_CART_ED64 or -DAGENT_CART_ED64PRO (see make host-test)"
#endif

/* ---- fake driver ------------------------------------------------------------ */

#define MAX_PIECES 64
static struct {
    uint8_t bytes[9000];
    uint32_t len;
} s_piece[MAX_PIECES];
static int s_pieces;
static int s_next;
/* 0: deliver every queued piece as fast as the agent asks; 1: one piece per tick. */
static int s_one_per_tick;
static int s_given_this_tick;

static uint8_t s_sent[16][9000];
static uint32_t s_sent_len[16];
static int s_sends;

int FAKE_INIT(void)
{
    return 1;
}

uint32_t FAKE_RECEIVE(uint8_t *dst, uint32_t cap)
{
    uint32_t n;
    if (s_next >= s_pieces || (s_one_per_tick && s_given_this_tick)) {
        return 0;
    }
    n = s_piece[s_next].len;
    assert(n <= cap && "a piece larger than the space the agent offered");
    memcpy(dst, s_piece[s_next].bytes, n);
    s_next++;
    s_given_this_tick = 1;
    return n;
}

int FAKE_SEND(const uint8_t *data, uint32_t len)
{
    assert(s_sends < 16);
    memcpy(s_sent[s_sends], data, len);
    s_sent_len[s_sends] = len;
    s_sends++;
    return 1;
}

/* ---- helpers ------------------------------------------------------------------ */

static void reset_script(void)
{
    s_pieces = 0;
    s_next = 0;
    s_sends = 0;
    s_one_per_tick = 0;
}

static void queue(const uint8_t *b, uint32_t n)
{
    assert(s_pieces < MAX_PIECES);
    memcpy(s_piece[s_pieces].bytes, b, n);
    s_piece[s_pieces].len = n;
    s_pieces++;
}

static void queue_split(const uint8_t *b, uint32_t n, uint32_t size)
{
    uint32_t off;
    for (off = 0; off < n; off += size) {
        queue(b + off, n - off < size ? n - off : size);
    }
}

/** An L3 frame of `type`/channel 0 carrying M64P with an unknown message and request id `rid`. */
static uint32_t frame(uint8_t *out, uint8_t type, uint16_t rid, uint32_t extra_payload)
{
    uint32_t payload = 5 + 2 + extra_payload;
    uint32_t i;
    memset(out, 0, 16 + payload);
    memcpy(out, "M64B", 4);
    out[4] = type;
    out[5] = 0x00; /* APPLICATION */
    out[7] = 0x01; /* FINAL */
    out[12] = (uint8_t)(payload >> 24);
    out[13] = (uint8_t)(payload >> 16);
    out[14] = (uint8_t)(payload >> 8);
    out[15] = (uint8_t)payload;
    memcpy(out + 16, "M64P", 4);
    out[20] = 0x7F; /* unknown message: answered with ERR */
    out[21] = (uint8_t)(rid >> 8);
    out[22] = (uint8_t)rid;
    for (i = 0; i < extra_payload; i++) {
        out[23 + i] = (uint8_t)(i * 7);
    }
    return 16 + payload;
}

/** The request id echoed by reply `k`, after checking it is an M64P ERR inside an L3 DATA frame. */
static uint16_t reply_rid(int k)
{
    const uint8_t *r = s_sent[k];
    assert(s_sent_len[k] == 16 + 8);
    assert(memcmp(r, "M64B", 4) == 0 && r[4] == 0x10);
    assert(memcmp(r + 16, "M64P", 4) == 0 && r[20] == 0xE0);
    return (uint16_t)((r[21] << 8) | r[22]);
}

static void tick(void)
{
    s_given_this_tick = 0;
    agent_tick();
}

static void ticks(int n)
{
    while (n-- > 0) {
        tick();
    }
}

/* ---- cases -------------------------------------------------------------------- */

static uint8_t f1[9000], f2[9000], junk[9000];

static void whole_frame_in_one_piece(void)
{
    uint32_t n = frame(f1, 0x10, 0x1234, 0);
    reset_script();
    queue(f1, n);
    tick();
    assert(s_sends == 1 && reply_rid(0) == 0x1234);
}

static void a_large_frame_in_host_sized_pieces_in_one_tick(void)
{
    /* 8 KB of payload in the pieces this cart's host sends, all waiting within one tick. */
    uint32_t n = frame(f1, 0x10, 0x0BEE, 7900);
    reset_script();
    queue_split(f1, n, HOST_PIECE);
    tick();
    assert(s_sends == 1 && reply_rid(0) == 0x0BEE);
    ticks(3);
    assert(s_sends == 1 && "the handled frame must be dropped, not handled again");
}

static void pieces_across_ticks_answer_only_when_complete(void)
{
    uint32_t n = frame(f1, 0x10, 0x0042, 40);
    int t;
    reset_script();
    queue_split(f1, n, 7);
    s_one_per_tick = 1;
    for (t = 0; t < s_pieces - 1; t++) {
        tick();
        assert(s_sends == 0);
    }
    tick();
    assert(s_sends == 1 && reply_rid(0) == 0x0042);
}

static void garbage_and_a_false_magic_before_the_frame(void)
{
    uint32_t n = frame(f1, 0x10, 0x7777, 3);
    uint32_t g = 0;
    reset_script();
    memcpy(junk + g, "\x00\xffM6", 4); /* looks like the start of a magic, is not */
    g += 4;
    memcpy(junk + g, "xyzM64", 6); /* another false start, cut short */
    g += 6;
    memcpy(junk + g, f1, n);
    g += n;
    queue_split(junk, g, 5);
    tick();
    assert(s_sends == 1 && reply_rid(0) == 0x7777);
}

static void two_frames_in_one_piece(void)
{
    uint32_t n1 = frame(f1, 0x10, 0x0001, 0);
    uint32_t n2 = frame(f2, 0x10, 0x0002, 10);
    reset_script();
    memcpy(junk, f1, n1);
    memcpy(junk + n1, f2, n2);
    queue(junk, n1 + n2);
    ticks(2);
    assert(s_sends == 2 && reply_rid(0) == 0x0001 && reply_rid(1) == 0x0002);
}

static void a_stale_partial_frame_does_not_swallow_the_retry(void)
{
    uint32_t n1 = frame(f1, 0x10, 0x0DEA, 100);
    uint32_t n2 = frame(f2, 0x10, 0x0FEE, 0);
    reset_script();
    queue(f1, n1 / 2); /* the rest is lost */
    ticks(59);
    queue(f2, n2);     /* would complete the stale header's payload if it were still held */
    s_one_per_tick = 1;
    ticks(1);          /* tick 60 delivers the retry: arrivals reset the stale count */
    assert(s_sends == 0 && "the retry is still inside the stale frame's claimed payload");

    /* Let that leftover go stale and be discarded, so the next case starts clean. */
    reset_script();
    ticks(61);
    assert(s_sends == 0);

    /* The realistic case: the host waits longer than 60 ticks before retrying. */
    reset_script();
    queue(f1, n1 / 2);
    ticks(1);
    ticks(60);
    queue(f2, n2);
    tick();
    assert(s_sends == 1 && reply_rid(0) == 0x0FEE);
}

static void an_impossible_length_is_skipped(void)
{
    uint32_t n = frame(f1, 0x10, 0x5151, 0);
    reset_script();
    memcpy(junk, "M64B\x10\x00\x00\x01\x00\x00\x00\x00\xff\xff\xff\xff", 16);
    memcpy(junk + 16, f1, n);
    queue(junk, 16 + n);
    tick();
    assert(s_sends == 1 && reply_rid(0) == 0x5151);
}

static void a_non_data_frame_is_consumed_and_ignored(void)
{
    uint32_t n1 = frame(f1, 0x20, 0x0009, 0); /* HEARTBEAT: not handled */
    uint32_t n2 = frame(f2, 0x10, 0x000A, 0);
    reset_script();
    queue(f1, n1);
    queue(f2, n2);
    ticks(2);
    assert(s_sends == 1 && reply_rid(0) == 0x000A);
}

int main(void)
{
    ticks(1); /* init */
    assert(agent_is_ready());
    whole_frame_in_one_piece();
    a_large_frame_in_host_sized_pieces_in_one_tick();
    pieces_across_ticks_answer_only_when_complete();
    garbage_and_a_false_magic_before_the_frame();
    two_frames_in_one_piece();
    a_stale_partial_frame_does_not_swallow_the_retry();
    an_impossible_length_is_skipped();
    a_non_data_frame_is_consumed_and_ignored();
    printf("reassembly (" CART_NAME "): 8 cases passed\n");
    return 0;
}
