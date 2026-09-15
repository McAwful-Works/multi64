/*
 * Host-side test of the cart agent's L3 reassembly, which only the EverDrive builds use.
 *
 * `make host-test` compiles the real agent.c and mem_proto.c for the PC, once per EverDrive cart
 * (AGENT_CART_ED64, then AGENT_CART_ED64PRO), with this file standing in for the cart driver: it
 * hands the agent scripted pieces of the byte stream and records what the agent sends back.
 * Requests use an unknown M64P message type, which mem_proto answers with ERR echoing the request
 * id, so nothing touches RDRAM.
 *
 * The fake driver copies what each real driver does when a piece does not fit the space the agent
 * offers (the X7 drops the message and reports the loss, the PRO reads what fits), and can lose a
 * piece the way both do when a read fails: consumed, and reported as lost.
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
#define FAKE_RECEIVE_LOST ED64PRO_RECEIVE_LOST
#define FAKE_SEND ed64pro_send
#define CART_NAME "ed64pro"
/* The PRO host writes the cart FIFO in blocks of up to 1024 bytes. */
#define HOST_PIECE 1024u
#elif defined(AGENT_CART_ED64)
#include "ed64.h"
#define FAKE_INIT ed64_init
#define FAKE_RECEIVE ed64_receive
#define FAKE_RECEIVE_LOST ED64_RECEIVE_LOST
#define FAKE_SEND ed64_send
#define CART_NAME "ed64"
/* The X7 host sends 512-byte DMA@ messages. */
#define HOST_PIECE 512u
#else
#error "build with -DAGENT_CART_ED64 or -DAGENT_CART_ED64PRO (see make host-test)"
#endif

/* ---- fake driver ------------------------------------------------------------ */

/* Larger than agent.c's AGENT_RX_CAP (8296), so one piece can overflow the agent's buffer. */
#define PIECE_BYTES 9000u

#define MAX_PIECES 64
static struct {
    uint8_t bytes[PIECE_BYTES];
    uint32_t len;
    int lost;
} s_piece[MAX_PIECES];
static int s_pieces;
static int s_next;
/* Bytes of s_piece[s_next] already delivered: the PRO can read a piece in parts. */
static uint32_t s_offset;
/* 0: deliver every queued piece as fast as the agent asks; 1: one piece per tick. */
static int s_one_per_tick;
static int s_given_this_tick;
/* Receives where the waiting piece was larger than the space the agent offered. */
static int s_overflows;
static int s_lost;

static uint8_t s_sent[16][PIECE_BYTES];
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
    s_given_this_tick = 1;
    if (s_piece[s_next].lost) {
        /* Consumed but not delivered, as when usb_pull fails or a CMPH trailer does not match
           (ed64.c), or a FIFO load fails (ed64pro.c): the driver reports the loss. */
        s_next++;
        s_lost++;
        return FAKE_RECEIVE_LOST;
    }
    n = s_piece[s_next].len - s_offset;
    if (n > cap) {
        s_overflows++;
#if defined(AGENT_CART_ED64)
        /* ed64_receive drains a message that does not fit and reports the loss: it is gone. */
        s_next++;
        s_offset = 0;
        return FAKE_RECEIVE_LOST;
#else
        /* ed64pro_receive reads only what fits; the rest waits in the FIFO. */
        n = cap;
#endif
    }
    memcpy(dst, s_piece[s_next].bytes + s_offset, n);
    s_offset += n;
    if (s_offset == s_piece[s_next].len) {
        s_next++;
        s_offset = 0;
    }
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
    s_offset = 0;
    s_sends = 0;
    s_one_per_tick = 0;
    s_overflows = 0;
    s_lost = 0;
}

static void queue(const uint8_t *b, uint32_t n)
{
    assert(s_pieces < MAX_PIECES && n <= PIECE_BYTES);
    memcpy(s_piece[s_pieces].bytes, b, n);
    s_piece[s_pieces].len = n;
    s_piece[s_pieces].lost = 0;
    s_pieces++;
}

/** A piece the host sent that the driver consumes and never delivers. */
static void queue_lost(const uint8_t *b, uint32_t n)
{
    queue(b, n);
    s_piece[s_pieces - 1].lost = 1;
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

static uint8_t f1[PIECE_BYTES], f2[PIECE_BYTES], junk[PIECE_BYTES];

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

/*
 * Bytes that spell the magic but not a header the protocol defines, each claiming a payload long
 * enough to swallow the request behind it: an undefined TYPE, then DATA on a reserved CHANNEL.
 * Waiting for either claimed length would hold the real request until it went stale (#151).
 */
static void a_header_with_an_undefined_type_or_channel_is_skipped(void)
{
    uint32_t n = frame(f1, 0x10, 0x5152, 0);
    reset_script();
    memcpy(junk, "M64B\x7e\x00\x00\x01\x00\x00\x00\x00\x00\x00\x01\x00", 16);
    memcpy(junk + 16, "M64B\x10\x03\x00\x01\x00\x00\x00\x00\x00\x00\x01\x00", 16);
    memcpy(junk + 32, f1, n);
    queue(junk, 32 + n);
    tick();
    assert(s_sends == 1 && reply_rid(0) == 0x5152);
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

/*
 * A message larger than all the space the agent offers: a small frame, then filler with no magic,
 * PIECE_BYTES in all. The X7 driver drains and drops the whole message, frame included; the PRO
 * reads what fits and the rest on the next receive. Either way the agent stays in step and answers
 * the request that follows.
 */
static void a_message_larger_than_the_receive_space(void)
{
    uint32_t n1 = frame(f1, 0x10, 0x0B16, 0);
    uint32_t n2 = frame(f2, 0x10, 0x0FF1, 0);
    reset_script();
    memset(junk, 0xAA, sizeof junk);
    memcpy(junk, f1, n1);
    queue(junk, sizeof junk);
    queue(f2, n2);
    ticks(4);
    assert(s_overflows == 1 && "the oversize piece must reach the driver's overflow path");
#if defined(AGENT_CART_ED64)
    assert(s_sends == 1 && reply_rid(0) == 0x0FF1 && "a dropped message's frame must not be handled");
#else
    assert(s_sends == 2 && reply_rid(0) == 0x0B16 && reply_rid(1) == 0x0FF1);
#endif
}

/*
 * Bug #151. Reassembly used to trust a partial frame's payload_len, so when a middle piece of a
 * request was lost, the next request's bytes filled out the claimed length and the spliced bytes
 * were handled as a real request. Here the first request's rid survives in its first piece, so a
 * spliced frame would be answered with that rid. The driver reports the loss, and the agent drops
 * the partial frame.
 */
static void a_lost_piece_is_not_completed_by_the_next_request(void)
{
    /* 2.5 host pieces, and the next request longer than the one piece that is lost. */
    uint32_t n1 = frame(f1, 0x10, 0x0151, HOST_PIECE * 5u / 2u - 23u);
    uint32_t n2 = frame(f2, 0x10, 0x0B0B, HOST_PIECE * 5u / 4u - 23u);
    int k;
    reset_script();
    queue(f1, HOST_PIECE);
    queue_lost(f1 + HOST_PIECE, HOST_PIECE);
    queue(f1 + 2u * HOST_PIECE, n1 - 2u * HOST_PIECE);
    queue_split(f2, n2, HOST_PIECE);
    ticks(4);
    assert(s_lost == 1);
    for (k = 0; k < s_sends; k++) {
        assert(reply_rid(k) != 0x0151 && "a frame spliced from a lost piece and later bytes was handled");
    }
    assert(s_sends == 1 && reply_rid(0) == 0x0B0B && "the request after the loss must be answered");
}

static int s_cases;
#define RUN(test) (test(), s_cases++)

int main(void)
{
    ticks(1); /* init */
    assert(agent_is_ready());
    RUN(whole_frame_in_one_piece);
    RUN(a_large_frame_in_host_sized_pieces_in_one_tick);
    RUN(pieces_across_ticks_answer_only_when_complete);
    RUN(garbage_and_a_false_magic_before_the_frame);
    RUN(two_frames_in_one_piece);
    RUN(a_stale_partial_frame_does_not_swallow_the_retry);
    RUN(an_impossible_length_is_skipped);
    RUN(a_header_with_an_undefined_type_or_channel_is_skipped);
    RUN(a_non_data_frame_is_consumed_and_ignored);
    RUN(a_message_larger_than_the_receive_space);
    RUN(a_lost_piece_is_not_completed_by_the_next_request);
    printf("reassembly (" CART_NAME "): %d cases passed\n", s_cases);
    return 0;
}
