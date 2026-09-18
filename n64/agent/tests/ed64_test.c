/*
 * Host-side test of the EverDrive-64 X7 driver.
 *
 * `make host-test` compiles the real ed64.c for the PC with this file in place of pi_io.c: the
 * pi_io_* functions below are a fake cart. It models the X7's USB unit as libdragon's usb.c drives
 * it: USBCFG (a read transfer of n bytes fills the last n bytes of the 512-byte USBDAT window from
 * what the host has sent; a write transfer sends the window from the offset it names to the
 * window's end; the status word carries POWER, RXF while nothing is waiting, and ACT while a
 * transfer is in progress), and the window itself. A read asking for more bytes than the host sent
 * never finishes, and any window load can be made to fail, the way pi_io.c fails when the PI stays
 * busy.
 *
 * One behaviour is observed rather than transcribed: a write transfer started while the host has
 * sent bytes the console has not read never finishes (ACT stays set), and the switch back to
 * RDNOP that ends the wait abandons it with nothing sent. An X7 running test ROM 1.11 gave up on
 * writes exactly then, and 1.12, which reads everything waiting before each write, gave up on none
 * (l3-over-everdrive-x7.md 4.5 item 6).
 *
 * Bytes are copied between the window and the driver's words in host byte order, so the driver's
 * byte view of a loaded word is the window's bytes in order, as it is on the big-endian console.
 *
 * It checks that a received message is delivered in order and whole, or reported lost, that a sent
 * message carries the framing the host parses, and that every wait is bounded. It says nothing
 * about the cart: the register model is transcribed, not observed, and this driver has never run
 * on an X7.
 */
#undef NDEBUG
#include <assert.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "ed64.h"
#include "pi_io.h"

/* As ed64.c's register map has them. */
#define F_USBCFG 0x1F800004u
#define F_VERSION 0x1F800014u
#define F_USBDAT 0x1F800400u
#define F_SYSCFG 0x1F808000u
#define F_KEY 0x1F808004u
#define F_WINDOW 512u

#define F_MODE_MASK 0xFE00u
#define F_MODE_RDNOP 0xC400u
#define F_MODE_RD 0xC600u
#define F_MODE_WRNOP 0xC000u
#define F_MODE_WR 0xC200u

#define F_ACT 0x0200u
#define F_RXF 0x0400u
#define F_POWER 0x1000u

#define F_L3 0x01u
/* The largest L3 payload crates/ed64-l2 puts in one DMA@ message (DEFAULT_ED64_CHUNK). */
#define HOST_MESSAGE 512u

#define F_HOST_BYTES 16384u

static struct {
    /* Everything the host has sent, and how much of it the cart has handed to the console. */
    uint8_t host[F_HOST_BYTES];
    uint32_t host_len;
    uint32_t host_pos;

    uint8_t window[F_WINDOW];
    /* A read transfer is waiting for bytes the host never sent: ACT stays set. */
    int stuck;
    /* A write transfer started with host bytes unread: ACT stays set until RDNOP abandons it. */
    int write_stuck;
    /* Write transfers asked for, whether or not they finished. */
    uint32_t writes;
    /* Read transfers started, and window loads made. */
    uint32_t reads;
    uint32_t loads;
    /* The load with this 1-based number fails; 0 for none. */
    uint32_t fail_load;
    /* The next read-transfer write fails before its store, the PI busy from the start. */
    int fail_rd_before_store;

    /* Everything the cart has transmitted, in order: a write transfer sends the window from the
       offset it names to the window's end. */
    uint8_t sent[F_HOST_BYTES];
    uint32_t sent_len;
} f;

int pi_io_read(uint32_t addr, uint32_t *value)
{
    if (addr == F_USBCFG) {
        *value = F_POWER | (f.host_pos == f.host_len ? F_RXF : 0u) |
                 (f.stuck || f.write_stuck ? F_ACT : 0u);
    } else if (addr == F_VERSION) {
        *value = 0xED640013u;
    } else {
        assert(0 && "a read of a register the fake does not model");
    }
    return 1;
}

int pi_io_write_stored(uint32_t addr, uint32_t value, int *stored)
{
    *stored = 0;
    if (addr == F_KEY || addr == F_SYSCFG) {
        *stored = 1;
        return 1;
    }
    if (f.fail_rd_before_store && addr == F_USBCFG && (value & F_MODE_MASK) == F_MODE_RD) {
        f.fail_rd_before_store = 0;
        return 0;
    }
    *stored = 1;
    assert(addr == F_USBCFG && "a write to a register the fake does not model");
    if ((value & F_MODE_MASK) == F_MODE_RD) {
        uint32_t start = value & 0x1FFu;
        uint32_t n;

        assert(start < F_WINDOW);
        n = F_WINDOW - start;
        f.reads++;
        if (f.host_len - f.host_pos < n) {
            f.stuck = 1;
        } else {
            memcpy(f.window + start, f.host + f.host_pos, n);
            f.host_pos += n;
        }
    } else if ((value & F_MODE_MASK) == F_MODE_WR) {
        /* A write transfer sends the window from `start` to the window's end, as libdragon's
           usb_everdrive_write drives it. */
        uint32_t start = value & 0x1FFu;
        uint32_t n;

        assert(start < F_WINDOW);
        n = F_WINDOW - start;
        f.writes++;
        if (f.host_pos < f.host_len) {
            /* What an X7 did: the write does not go out while host bytes wait unread. */
            f.write_stuck = 1;
            return 1;
        }
        assert(f.sent_len + n <= F_HOST_BYTES);
        memcpy(f.sent + f.sent_len, f.window + start, n);
        f.sent_len += n;
    } else if ((value & F_MODE_MASK) == F_MODE_RDNOP) {
        f.write_stuck = 0;
    } else {
        assert((value & F_MODE_MASK) == F_MODE_WRNOP && "a USB mode the fake does not model");
    }
    return 1;
}

int pi_io_write(uint32_t addr, uint32_t value)
{
    int stored;
    return pi_io_write_stored(addr, value, &stored);
}

int pi_io_load_words(uint32_t *dst, uint32_t addr, uint32_t words)
{
    f.loads++;
    if (f.loads == f.fail_load) {
        return 0;
    }
    assert(addr >= F_USBDAT && (addr - F_USBDAT) % 4u == 0u);
    assert(addr - F_USBDAT + 4u * words <= F_WINDOW);
    memcpy(dst, f.window + (addr - F_USBDAT), 4u * words);
    return 1;
}

int pi_io_store_words(const uint32_t *src, uint32_t addr, uint32_t words)
{
    assert(addr >= F_USBDAT && (addr - F_USBDAT) % 4u == 0u);
    assert(addr - F_USBDAT + 4u * words <= F_WINDOW);
    memcpy(f.window + (addr - F_USBDAT), src, 4u * words);
    return 1;
}

/* ---- helpers -------------------------------------------------------------------- */

static uint8_t payload[4096];
static uint8_t payload2[4096];
static uint8_t got[8192];

static void pattern(uint8_t *b, uint32_t n, uint8_t seed)
{
    uint32_t i;
    for (i = 0u; i < n; i++) {
        b[i] = (uint8_t)(seed + i * 13u);
    }
}

static void host_raw(const void *b, uint32_t n)
{
    assert(f.host_len + n <= F_HOST_BYTES);
    memcpy(f.host + f.host_len, b, n);
    f.host_len += n;
}

/** A DMA@ message as the host frames it (l3-over-everdrive-x7.md 4.2), with its magics given. */
static void host_message_framed(const char *head, uint8_t type, const uint8_t *data, uint32_t size,
                                const char *tail)
{
    static const uint8_t zero[1] = { 0u };
    uint8_t h[4];

    h[0] = type;
    h[1] = (uint8_t)(size >> 16);
    h[2] = (uint8_t)(size >> 8);
    h[3] = (uint8_t)size;
    host_raw(head, 4u);
    host_raw(h, 4u);
    host_raw(data, size);
    if (size & 1u) {
        host_raw(zero, 1u);
    }
    host_raw(tail, 4u);
}

static void host_message(uint8_t type, const uint8_t *data, uint32_t size)
{
    host_message_framed("DMA@", type, data, size, "CMPH");
}

static void fresh_cart(void)
{
    memset(&f, 0, sizeof f);
    assert(ed64_init() == 1);
    /* Nothing the last case left behind in the driver may reach this one. */
    assert(ed64_receive(got, sizeof got) == 0u && f.reads == 0u);
}

/* ---- cases ------------------------------------------------------------------------ */

static void a_message_that_fits_is_delivered_whole(void)
{
    fresh_cart();
    assert(ed64_receive(got, sizeof got) == 0u && "nothing is waiting");

    pattern(payload, HOST_MESSAGE, 0x11u);
    host_message(F_L3, payload, HOST_MESSAGE);
    assert(ed64_receive(got, HOST_MESSAGE) == HOST_MESSAGE && memcmp(got, payload, HOST_MESSAGE) == 0);
    assert(f.host_pos == f.host_len && ed64_receive(got, sizeof got) == 0u);
}

/*
 * The agent offers what is left of its receive buffer. A normal 512-byte message arriving while
 * less than that is free used to be drained and reported lost, taking with it a request that
 * would have fitted once the frame in front of it was handled.
 */
static void a_message_with_less_than_512_bytes_free_is_not_lost(void)
{
    uint32_t reads;

    fresh_cart();
    pattern(payload, HOST_MESSAGE, 0x21u);
    pattern(payload2, 40u, 0x77u);
    host_message(F_L3, payload, HOST_MESSAGE);
    host_message(F_L3, payload2, 40u);

    assert(ed64_receive(got, 200u) != ED64_RECEIVE_LOST && "a message that fits the leftover space was lost");
    assert(memcmp(got, payload, 200u) == 0);

    /* The rest comes next, before the message behind it, and without another USB transfer. */
    reads = f.reads;
    assert(ed64_receive(got, 100u) == 100u && memcmp(got, payload + 200u, 100u) == 0);
    assert(ed64_receive(got, sizeof got) == 212u && memcmp(got, payload + 300u, 212u) == 0);
    assert(f.reads == reads && "the leftover must be handed over before USB is read again");

    assert(ed64_receive(got, sizeof got) == 40u && memcmp(got, payload2, 40u) == 0);
    assert(ed64_receive(got, sizeof got) == 0u);
}

static void odd_length_payloads_still_drain_their_padding(void)
{
    fresh_cart();
    pattern(payload, 5u, 0x31u);
    host_message(F_L3, payload, 5u);
    assert(ed64_receive(got, sizeof got) == 5u && memcmp(got, payload, 5u) == 0);

    /* Split, with the padding byte after the part kept back. */
    pattern(payload, 301u, 0x32u);
    host_message(F_L3, payload, 301u);
    assert(ed64_receive(got, 100u) == 100u && memcmp(got, payload, 100u) == 0);
    assert(ed64_receive(got, sizeof got) == 201u && memcmp(got, payload + 100u, 201u) == 0);

    /* Longer than one window, split across both of its pulls. */
    pattern(payload, 1023u, 0x33u);
    host_message(F_L3, payload, 1023u);
    assert(ed64_receive(got, 700u) == 700u && memcmp(got, payload, 700u) == 0);
    assert(ed64_receive(got, sizeof got) == 323u && memcmp(got, payload + 700u, 323u) == 0);

    pattern(payload2, 7u, 0x34u);
    host_message(F_L3, payload2, 7u);
    assert(ed64_receive(got, sizeof got) == 7u && memcmp(got, payload2, 7u) == 0 && "out of step");
    assert(f.host_pos == f.host_len && ed64_receive(got, sizeof got) == 0u);
}

static void a_bad_header_or_trailer_is_reported_lost(void)
{
    fresh_cart();
    pattern(payload, 64u, 0x41u);
    host_message_framed("DMA#", F_L3, payload, 64u, "CMPH");
    assert(ed64_receive(got, sizeof got) == ED64_RECEIVE_LOST);

    fresh_cart();
    host_message_framed("DMA@", F_L3, payload, 64u, "CMPX");
    assert(ed64_receive(got, sizeof got) == ED64_RECEIVE_LOST);

    /* A message that would have been split: its kept-back part must not be delivered either. */
    fresh_cart();
    pattern(payload, HOST_MESSAGE, 0x42u);
    host_message_framed("DMA@", F_L3, payload, HOST_MESSAGE, "XMPH");
    pattern(payload2, 16u, 0x43u);
    host_message(F_L3, payload2, 16u);
    assert(ed64_receive(got, 100u) == ED64_RECEIVE_LOST);
    assert(ed64_receive(got, sizeof got) == 16u && memcmp(got, payload2, 16u) == 0);

    /* A leftover is delivered before a bad message behind it is found. */
    fresh_cart();
    host_message(F_L3, payload, HOST_MESSAGE);
    host_message_framed("DMA!", F_L3, payload2, 16u, "CMPH");
    assert(ed64_receive(got, 400u) == 400u);
    assert(ed64_receive(got, sizeof got) == 112u && memcmp(got, payload + 400u, 112u) == 0);
    assert(ed64_receive(got, sizeof got) == ED64_RECEIVE_LOST);
}

static void a_failed_load_is_reported_lost(void)
{
    fresh_cart();
    pattern(payload, HOST_MESSAGE, 0x51u);

    /* Loads, per message of up to one window: header, payload, trailer. */
    f.fail_load = f.loads + 2u;
    host_message(F_L3, payload, HOST_MESSAGE);
    assert(ed64_receive(got, 100u) == ED64_RECEIVE_LOST);
    f.host_pos = f.host_len; /* the rest of the stream is out of step: flush it */
    assert(ed64_receive(got, sizeof got) == 0u && "a message whose payload failed to load left a leftover");

    f.fail_load = f.loads + 3u;
    host_message(F_L3, payload, HOST_MESSAGE);
    assert(ed64_receive(got, 100u) == ED64_RECEIVE_LOST);
    assert(ed64_receive(got, sizeof got) == 0u && "a message whose trailer failed to load left a leftover");

    f.fail_load = f.loads + 1u;
    host_message(F_L3, payload, 16u);
    assert(ed64_receive(got, sizeof got) == ED64_RECEIVE_LOST);
}

/*
 * #223: a PI that stays busy before the header's read transfer is even asked for has taken nothing
 * from the cart, so it is not a loss. Reporting one made the agent throw away the part of a request
 * it already held, and the rest then arrived with nothing to join.
 */
static void a_read_that_never_started_is_not_a_loss(void)
{
    fresh_cart();
    pattern(payload, 64u, 0x91u);
    host_message(F_L3, payload, 64u);

    f.fail_rd_before_store = 1;
    assert(ed64_receive(got, sizeof got) == 0u && "nothing was read, so nothing was lost");
    assert(f.reads == 0u && f.host_pos == 0u);

    /* The message is still whole in the cart, for the next call. */
    assert(ed64_receive(got, sizeof got) == 64u && memcmp(got, payload, 64u) == 0);
    assert(f.host_pos == f.host_len);
}

/* The host's header promises more than it sends: the read never finishes, and the wait gives up. */
static void a_read_that_never_finishes_is_reported_lost(void)
{
    fresh_cart();
    pattern(payload, 100u, 0x61u);
    host_raw("DMA@\x01\x00\x02\x00", 8u);
    host_raw(payload, 100u);
    assert(ed64_receive(got, 100u) == ED64_RECEIVE_LOST && f.stuck);
}

static void a_message_too_big_even_with_the_leftover_buffer_is_lost(void)
{
    fresh_cart();
    pattern(payload, 4000u, 0x71u);
    pattern(payload2, 24u, 0x72u);

    /* One byte over: the space offered plus one host message. */
    host_message(F_L3, payload, 80u + HOST_MESSAGE + 1u);
    host_message(F_L3, payload2, 24u);
    assert(ed64_receive(got, 80u) == ED64_RECEIVE_LOST);
    assert(ed64_receive(got, sizeof got) == 24u && memcmp(got, payload2, 24u) == 0 && "not drained");

    /* Exactly that much is kept. */
    host_message(F_L3, payload, 80u + HOST_MESSAGE);
    assert(ed64_receive(got, 80u) == 80u && memcmp(got, payload, 80u) == 0);
    assert(ed64_receive(got, sizeof got) == HOST_MESSAGE && memcmp(got, payload + 80u, HOST_MESSAGE) == 0);

    host_message(F_L3, payload, 4000u);
    host_message(F_L3, payload2, 24u);
    assert(ed64_receive(got, 1000u) == ED64_RECEIVE_LOST);
    assert(ed64_receive(got, sizeof got) == 24u && memcmp(got, payload2, 24u) == 0 && "not drained");
    assert(f.host_pos == f.host_len);
}

/*
 * #134: the cart writes CMPH straight after the unpadded payload and pads the whole message
 * afterwards. libdragon's usb_everdrive_write does this by restarting its copy loop to append the
 * trailer and then sending ALIGN(block+offset, 2) bytes, and UNFLoader's receive path reads the
 * trailer at `size` before consuming the alignment. The driver used to pad the payload first and
 * put CMPH after the pad, which mis-framed every odd-length message it sent.
 */
static void a_sent_message_puts_its_padding_after_the_trailer(void)
{
    uint8_t body[3];

    fresh_cart();
    pattern(body, sizeof body, 0x40u);
    assert(ed64_send(body, sizeof body) == 1);

    /* 8 header + 3 payload + 4 trailer, padded to 2. */
    assert(f.sent_len == 16u);
    assert(memcmp(f.sent, "DMA@", 4u) == 0);
    assert(f.sent[4] == F_L3 && f.sent[5] == 0u && f.sent[6] == 0u && f.sent[7] == 3u);
    assert(memcmp(f.sent + 8, body, sizeof body) == 0);
    assert(memcmp(f.sent + 11, "CMPH", 4u) == 0);
}

/* An even payload needs no padding at all, so the trailer ends the message either way. */
static void a_sent_even_message_has_no_padding(void)
{
    uint8_t body[4];

    fresh_cart();
    pattern(body, sizeof body, 0x70u);
    assert(ed64_send(body, sizeof body) == 1);

    assert(f.sent_len == 16u);
    assert(f.sent[7] == 4u);
    assert(memcmp(f.sent + 8, body, sizeof body) == 0);
    assert(memcmp(f.sent + 12, "CMPH", 4u) == 0);
}

/*
 * A message longer than the 512-byte window is sent in several transfers, and the trailer still
 * lands straight after the payload - across a window boundary, which is where an off-by-one in the
 * chunking would show.
 */
static void a_sent_message_longer_than_the_window_keeps_its_framing(void)
{
    static uint8_t body[600];
    uint32_t total = 8u + sizeof body + 4u;

    fresh_cart();
    pattern(body, sizeof body, 0x11u);
    assert(ed64_send(body, sizeof body) == 1);

    assert((sizeof body % 2u) == 0u);
    assert(f.sent_len == total);
    assert(memcmp(f.sent, "DMA@", 4u) == 0);
    assert(memcmp(f.sent + 8, body, sizeof body) == 0);
    assert(memcmp(f.sent + 8 + sizeof body, "CMPH", 4u) == 0);
}

static void a_non_l3_message_is_drained_and_ignored(void)
{
    fresh_cart();
    pattern(payload, HOST_MESSAGE, 0x81u);
    pattern(payload2, 20u, 0x82u);
    host_message(0x02u, payload, HOST_MESSAGE);
    host_message(0x03u, payload, 3u);
    host_message(F_L3, payload2, 20u);
    assert(ed64_receive(got, 10u) == 0u);
    assert(ed64_receive(got, 10u) == 0u);
    assert(ed64_receive(got, sizeof got) == 20u && memcmp(got, payload2, 20u) == 0);
    assert(ed64_receive(got, sizeof got) == 0u && f.host_pos == f.host_len);
}

/*
 * On an X7 a write does not finish while the host has sent bytes the cart has not read, and the old
 * driver gave up part-way, leaving the host a message cut off after its first block. Everything
 * waiting is read first and kept for ed64_receive, in order, and the message goes out whole.
 */
static void a_send_with_host_data_waiting_reads_it_first_and_sends_whole(void)
{
    static uint8_t body[600];
    uint32_t reads;

    fresh_cart();
    pattern(payload, HOST_MESSAGE, 0xA1u);
    pattern(payload2, 40u, 0xA2u);
    pattern(body, sizeof body, 0xA3u);
    host_message(F_L3, payload, HOST_MESSAGE);
    host_message(F_L3, payload2, 40u);

    assert(ed64_send(body, sizeof body) == 1 && "the send gave up with host data waiting");
    assert(f.sent_len == 8u + sizeof body + 4u && memcmp(f.sent, "DMA@", 4u) == 0);
    assert(memcmp(f.sent + 8, body, sizeof body) == 0);
    assert(memcmp(f.sent + 8 + sizeof body, "CMPH", 4u) == 0);
    assert(f.host_pos == f.host_len && "the host's messages were not read before the write");

    /* What was read comes out of ed64_receive in order, and without another USB transfer. */
    reads = f.reads;
    assert(ed64_receive(got, 300u) == 300u && memcmp(got, payload, 300u) == 0);
    assert(ed64_receive(got, sizeof got) == 252u);
    assert(memcmp(got, payload + 300u, 212u) == 0 && memcmp(got + 212u, payload2, 40u) == 0);
    assert(f.reads == reads);
    assert(ed64_receive(got, sizeof got) == 0u);
}

/*
 * A message read ahead that turns out lost is reported where it fell, not dropped or moved. A bad
 * trailer, so the whole message is consumed and the one behind it lines up; a bad header takes only
 * its own 8 bytes and leaves the stream out of step, here as in ed64_receive.
 */
static void a_loss_read_ahead_of_a_send_is_reported_in_order(void)
{
    uint8_t body[8];

    fresh_cart();
    pattern(payload, 100u, 0xB1u);
    pattern(payload2, 20u, 0xB2u);
    pattern(body, sizeof body, 0xB3u);
    host_message(F_L3, payload, 100u);
    host_message_framed("DMA@", F_L3, payload, 64u, "CMPX");
    host_message(F_L3, payload2, 20u);

    assert(ed64_send(body, sizeof body) == 1);
    assert(ed64_receive(got, sizeof got) == 100u && memcmp(got, payload, 100u) == 0);
    assert(ed64_receive(got, sizeof got) == ED64_RECEIVE_LOST);
    assert(ed64_receive(got, sizeof got) == 20u && memcmp(got, payload2, 20u) == 0);
    assert(ed64_receive(got, sizeof got) == 0u);
}

/*
 * When more is waiting than the driver can hold, nothing is sent at all. A reply the agent loses is
 * a timeout on the host; a reply cut off part-way is a malformed message it has to resync past.
 */
static void more_waiting_than_the_driver_holds_sends_nothing(void)
{
    uint8_t body[8];
    uint32_t i;

    fresh_cart();
    pattern(payload, HOST_MESSAGE, 0xC1u);
    for (i = 0u; i < 16u; i++) {
        host_message(F_L3, payload, HOST_MESSAGE);
    }
    pattern(body, sizeof body, 0xC2u);

    assert(ed64_send(body, sizeof body) == 0);
    assert(f.writes == 0u && f.sent_len == 0u && "a write was started with host data unread");

    /* Nothing read ahead is lost: all sixteen come out, in order. */
    for (i = 0u; i < 16u; i++) {
        assert(ed64_receive(got, HOST_MESSAGE) == HOST_MESSAGE && memcmp(got, payload, HOST_MESSAGE) == 0);
    }
    assert(ed64_receive(got, sizeof got) == 0u && f.host_pos == f.host_len);
}

/* ---- runner ------------------------------------------------------------------------ */

static const char *s_case;

static void on_alarm(int sig)
{
    static const char a[] = "ed64: ";
    static const char b[] = " did not return: a wait in ed64.c is unbounded\n";
    (void)sig;
    (void)!write(2, a, sizeof a - 1u);
    (void)!write(2, s_case, strlen(s_case));
    (void)!write(2, b, sizeof b - 1u);
    _exit(1);
}

static int s_cases;
#define RUN(test) (s_case = #test, alarm(60u), test(), alarm(0u), s_cases++)

int main(void)
{
    signal(SIGALRM, on_alarm);
    RUN(a_message_that_fits_is_delivered_whole);
    RUN(a_message_with_less_than_512_bytes_free_is_not_lost);
    RUN(odd_length_payloads_still_drain_their_padding);
    RUN(a_bad_header_or_trailer_is_reported_lost);
    RUN(a_failed_load_is_reported_lost);
    RUN(a_read_that_never_started_is_not_a_loss);
    RUN(a_read_that_never_finishes_is_reported_lost);
    RUN(a_message_too_big_even_with_the_leftover_buffer_is_lost);
    RUN(a_non_l3_message_is_drained_and_ignored);
    RUN(a_sent_message_puts_its_padding_after_the_trailer);
    RUN(a_sent_even_message_has_no_padding);
    RUN(a_sent_message_longer_than_the_window_keeps_its_framing);
    RUN(a_send_with_host_data_waiting_reads_it_first_and_sends_whole);
    RUN(a_loss_read_ahead_of_a_send_is_reported_in_order);
    RUN(more_waiting_than_the_driver_holds_sends_nothing);
    printf("ed64 driver: %d cases passed\n", s_cases);
    return 0;
}
