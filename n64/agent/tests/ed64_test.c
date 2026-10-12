/*
 * Host-side test of the EverDrive-64 X7 driver.
 *
 * `make host-test` compiles the real ed64.c for the PC with this file in place of pi_io.c: the
 * pi_io_* functions below are a fake cart. It models the X7's USB unit as libdragon's usb.c drives
 * it: USBCFG (a read transfer of n bytes fills the last n bytes of the 512-byte USBDAT window from
 * what the host has sent; a write transfer sends the window from the offset it names to the
 * window's end; the status word carries POWER, RXF while nothing is waiting, and ACT while a
 * transfer is in progress), and the window itself. Any window load, or any run of pi_io accesses,
 * can be made to fail, the way pi_io.c fails when the PI is busy, and a USBCFG write can be made to
 * take effect and still report failure, the way pi_io.c does when a store it made does not land.
 *
 * It also supplies the driver's clock (ED64_HOST_TEST), which is the CPU's Count register on a
 * console. Every pi_io access moves it on: by a few microseconds, by however long a test says an
 * access waits for the PI before it succeeds, or by however long one waits before it fails.
 *
 * Two behaviors are not transcribed from usb.c:
 *
 * - Observed: a write transfer started while the host has sent bytes the console has not read
 *   never finishes (ACT stays set), and the switch back to RDNOP that ends the wait abandons it
 *   with nothing sent. An X7 running test ROM 1.11 gave up on writes exactly then, and 1.12, which
 *   reads everything waiting before each write, gave up on none (l3-over-everdrive-x7.md 4.5
 *   item 6).
 * - Assumed: a read transfer asking for more bytes than the host has sent takes what there is and
 *   stays active, taking the rest as it arrives, until the switch back to RDNOP abandons it. What
 *   it took is then gone. No cart has been watched doing this; it is what a unit that moves bytes
 *   from the USB chip to the window one at a time would do.
 *
 * Bytes are copied between the window and the driver's words in host byte order, so the driver's
 * byte view of a loaded word is the window's bytes in order, as it is on the big-endian console.
 *
 * It checks that a received message is delivered in order and whole, or reported lost once with
 * the messages behind it intact; that a PI that is busy or slow part-way through a message, for
 * however long, delays it and loses nothing; that a sent message carries the framing the host
 * parses and is not cut off by a busy PI once it has started; and that every call ends within its
 * limit on the clock. It says nothing about the cart: the register model is transcribed and, where
 * noted, assumed. The driver has run on one X7, in n64/bringup's ROM and inside one game, Ocarina
 * of Time, before it kept its place in the stream between calls or timed its waits; no cart has
 * run it since.
 *
 *   build/host/ed64          every case
 *   build/host/ed64 NAME     the cases whose name contains NAME
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

/* What the driver holds of what it has read, as ed64.c has it (ED_RXQ_CAP). */
#define F_QUEUE 2048u

/* The driver's limits, in Count ticks, as ed64.c has them. */
#define F_MS 46875u
#define F_BUSY_TICKS (75u * F_MS)
#define F_RECEIVE_TICKS (100u * F_MS)
#define F_BURST_TICKS (30u * F_MS)
#define F_SEND_TICKS (250u * F_MS)

/* The longest pi_io.c waits for an idle PI before it fails an access: PI_WAIT_SPINS status reads,
   26.3 ms at the 263 ns one was measured to take. */
#define F_PI_WAIT (27u * F_MS)
/* What a call may run past its limit by: the pi_io call under way when the limit passes, and after
   a status read that finds a transfer overdue, the write that abandons it. Two such waits. */
#define F_OVERRUN (2u * F_PI_WAIT)
/* One access with the PI idle: an X7's USBCFG read was measured at 3.6 us. */
#define F_COST 169u

static struct {
    /* Everything the host has sent, and how much of it the cart has handed to the console. The
       last host_hold bytes have not arrived yet: they do after hold_reads more status reads. */
    uint8_t host[F_HOST_BYTES];
    uint32_t host_len;
    uint32_t host_pos;
    uint32_t host_hold;
    uint32_t hold_reads;

    uint8_t window[F_WINDOW];
    /* A read transfer still wants rd_want bytes the host has not sent, the next of them for
       window[rd_at]: ACT stays set until they arrive or RDNOP abandons it. */
    uint32_t rd_at;
    uint32_t rd_want;
    /* A write transfer started with host bytes unread: ACT stays set until RDNOP abandons it. */
    int write_stuck;
    /* The next write transfer does the same, whatever is waiting. */
    int hold_wr;
    /* Write transfers asked for, whether or not they finished. */
    uint32_t writes;
    /* Read transfers started, and window loads made. */
    uint32_t reads;
    uint32_t loads;
    /* The load with this 1-based number fails; 0 for none. */
    uint32_t fail_load;
    /* Every load from this 1-based number on fails, as a busy PI's do; 0 for none. */
    uint32_t fail_loads_from;
    /* The next read-transfer write fails before its store, the PI busy from the start. */
    int fail_rd_before_store;
    /* And the next switch back to RDNOP. */
    int fail_rdnop;
    /* A USBCFG write of this mode, after land_skip others of it, takes effect and then reports
       failure with *stored set: a store that was made and did not land. 0 for none. */
    uint32_t land_fail;
    uint32_t land_skip;

    /* Every pi_io access, numbered from 1. Accesses busy_at to busy_at + busy_len - 1 fail before
       they touch the cart, as pi_io.c's do when the PI is busy; busy_at 0 for none. */
    uint32_t pio;
    uint32_t busy_at;
    uint32_t busy_len;
    /* The access that started the first write transfer, 0 until one has. */
    uint32_t first_wr_pio;

    /* The clock, in Count ticks, and what an access adds to it: one that goes through, and one the
       PI is busy for. pi_io.c fails at once when it finds a DMA under way, and after PI_WAIT_SPINS
       when the PI never goes idle. */
    uint32_t now;
    uint32_t cost;
    uint32_t busy_cost;

    /* Everything the cart has transmitted, in order: a write transfer sends the window from the
       offset it names to the window's end. */
    uint8_t sent[F_HOST_BYTES];
    uint32_t sent_len;
} f;

/* The driver's clock. Reading it takes time too, so no loop can stand still on it. */
uint32_t ed64_host_count(void)
{
    return f.now++;
}

/* Count an access and the time it takes, and say whether the PI is busy for it. */
static int pi_busy(void)
{
    int busy;

    f.pio++;
    busy = f.busy_at != 0u && f.pio >= f.busy_at && f.pio - f.busy_at < f.busy_len;
    f.now += busy ? f.busy_cost : f.cost;
    return busy;
}

/* A read transfer under way takes what the host has sent. */
static void pump(void)
{
    while (f.rd_want > 0u && f.host_pos < f.host_len - f.host_hold) {
        f.window[f.rd_at++] = f.host[f.host_pos++];
        f.rd_want--;
    }
}

int pi_io_read(uint32_t addr, uint32_t *value)
{
    if (pi_busy()) {
        return 0;
    }
    if (addr == F_USBCFG) {
        if (f.host_hold != 0u && --f.hold_reads == 0u) {
            f.host_hold = 0u;
        }
        pump();
        *value = F_POWER | (f.host_pos == f.host_len - f.host_hold ? F_RXF : 0u) |
                 (f.rd_want != 0u || f.write_stuck ? F_ACT : 0u);
    } else if (addr == F_VERSION) {
        *value = 0xED640013u;
    } else {
        assert(0 && "a read of a register the fake does not model");
    }
    return 1;
}

int pi_io_write_stored(uint32_t addr, uint32_t value, int *stored)
{
    uint32_t mode = value & F_MODE_MASK;

    *stored = 0;
    if (pi_busy()) {
        return 0;
    }
    if (addr == F_KEY || addr == F_SYSCFG) {
        *stored = 1;
        return 1;
    }
    if (f.fail_rd_before_store && addr == F_USBCFG && mode == F_MODE_RD) {
        f.fail_rd_before_store = 0;
        return 0;
    }
    if (f.fail_rdnop && addr == F_USBCFG && mode == F_MODE_RDNOP) {
        f.fail_rdnop = 0;
        return 0;
    }
    *stored = 1;
    assert(addr == F_USBCFG && "a write to a register the fake does not model");
    if (mode == F_MODE_RDNOP) {
        f.write_stuck = 0;
        f.rd_want = 0u;
    } else {
        assert(f.rd_want == 0u && !f.write_stuck && "a USB mode set with a transfer still active");
    }
    if (mode == F_MODE_RD) {
        uint32_t start = value & 0x1FFu;

        assert(start < F_WINDOW);
        f.reads++;
        f.rd_at = start;
        f.rd_want = F_WINDOW - start;
        pump();
    } else if (mode == F_MODE_WR) {
        /* A write transfer sends the window from `start` to the window's end, as libdragon's
           usb_everdrive_write drives it. */
        uint32_t start = value & 0x1FFu;
        uint32_t n;

        assert(start < F_WINDOW);
        n = F_WINDOW - start;
        f.writes++;
        if (f.first_wr_pio == 0u) {
            f.first_wr_pio = f.pio;
        }
        if (f.hold_wr || f.host_pos < f.host_len - f.host_hold) {
            /* What an X7 did: the write does not go out while host bytes wait unread. */
            f.hold_wr = 0;
            f.write_stuck = 1;
        } else {
            assert(f.sent_len + n <= F_HOST_BYTES);
            memcpy(f.sent + f.sent_len, f.window + start, n);
            f.sent_len += n;
        }
    } else {
        assert((mode == F_MODE_RDNOP || mode == F_MODE_WRNOP) && "a USB mode the fake does not model");
    }
    if (f.land_fail == mode && mode != 0u) {
        if (f.land_skip == 0u) {
            f.land_fail = 0u;
            return 0;
        }
        f.land_skip--;
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
    if (pi_busy()) {
        return 0;
    }
    f.loads++;
    if (f.loads == f.fail_load || (f.fail_loads_from != 0u && f.loads >= f.fail_loads_from)) {
        /* The PI was busy for this one too: it took as long as an access that fails does. */
        f.now += f.busy_cost - f.cost;
        return 0;
    }
    assert(addr >= F_USBDAT && (addr - F_USBDAT) % 4u == 0u);
    assert(addr - F_USBDAT + 4u * words <= F_WINDOW);
    memcpy(dst, f.window + (addr - F_USBDAT), 4u * words);
    return 1;
}

int pi_io_store_words(const uint32_t *src, uint32_t addr, uint32_t words)
{
    if (pi_busy()) {
        return 0;
    }
    assert(addr >= F_USBDAT && (addr - F_USBDAT) % 4u == 0u);
    assert(addr - F_USBDAT + 4u * words <= F_WINDOW);
    memcpy(f.window + (addr - F_USBDAT), src, 4u * words);
    return 1;
}

/* ---- helpers -------------------------------------------------------------------- */

static uint8_t payload[12288];
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
    f.cost = F_COST;
    f.busy_cost = F_COST;
    /* ed64_init() forgets whatever the last case left behind in the driver. */
    assert(ed64_init() == 1);
    assert(ed64_receive(got, sizeof got) == 0u && f.reads == 0u);
}

/** ed64_receive, with the Count ticks it took. */
static uint32_t timed_receive(uint8_t *dst, uint32_t cap, uint32_t *ticks)
{
    uint32_t t0 = f.now;
    uint32_t n = ed64_receive(dst, cap);

    *ticks = f.now - t0;
    return n;
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

/*
 * A window load the PI is busy for loses nothing, whichever of a message's three it is: the bytes
 * wait in the cart's window, and the next call loads them. Each used to be reported lost, and
 * after the header's or the payload's the driver no longer knew where in the stream it was.
 */
static void a_failed_load_loses_nothing(void)
{
    uint32_t which;

    /* Loads, per message of up to one window: header, payload, trailer. */
    for (which = 1u; which <= 3u; which++) {
        fresh_cart();
        pattern(payload, HOST_MESSAGE, 0x51u);
        pattern(payload2, 16u, 0x52u);
        host_message(F_L3, payload, HOST_MESSAGE);
        host_message(F_L3, payload2, 16u);
        f.fail_load = f.loads + which;

        assert(ed64_receive(got, 100u) == 0u && "bytes not yet loaded are not a loss, or a message");
        /* Then all of it, as the space offered allows, and the message behind it. */
        assert(ed64_receive(got, 100u) == 100u && memcmp(got, payload, 100u) == 0);
        assert(ed64_receive(got, sizeof got) == 412u && memcmp(got, payload + 100u, 412u) == 0);
        assert(ed64_receive(got, sizeof got) == 16u && memcmp(got, payload2, 16u) == 0);
        assert(f.host_pos == f.host_len && ed64_receive(got, sizeof got) == 0u);
    }
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

/*
 * The PI busy for 3 accesses in a row, starting at each access a clean receive of two messages
 * makes in turn. Whichever it is, both arrive whole and in order, and nothing is reported lost.
 *
 * Busy for the one access that asks for a payload's read, the driver used to report the message
 * lost with all of it still in the cart, and then read it as headers: 8 bytes and one more loss a
 * call, about 70 calls for a 512-byte message, with the host's retry read the same way behind it.
 * An X7 under load reported 67 losses in a run where the PI was found busy 7 times (n64/bringup,
 * 2026-10-09).
 */
static void a_busy_pi_part_way_through_a_message_loses_nothing(void)
{
    uint32_t setup;
    uint32_t accesses;
    uint32_t at;

    /* Longer than one window, so a payload has a second read to be busy for. */
    pattern(payload, 1100u, 0x12u);
    pattern(payload2, 40u, 0x78u);

    /* fresh_cart makes the same accesses every time, so access numbers line up across runs. */
    fresh_cart();
    setup = f.pio;
    host_message(F_L3, payload, 1100u);
    host_message(F_L3, payload2, 40u);
    assert(ed64_receive(got, sizeof got) == 1100u && ed64_receive(got, sizeof got) == 40u);
    accesses = f.pio;

    for (at = setup + 1u; at <= accesses; at++) {
        uint32_t calls;
        uint32_t n = 0u;

        fresh_cart();
        host_message(F_L3, payload, 1100u);
        host_message(F_L3, payload2, 40u);
        f.busy_at = at;
        f.busy_len = 3u;

        for (calls = 0u; calls < 8u && n == 0u; calls++) {
            n = ed64_receive(got, sizeof got);
            assert(n != ED64_RECEIVE_LOST && "a busy PI lost a message the cart still held");
        }
        assert(n == 1100u && memcmp(got, payload, 1100u) == 0);
        for (n = 0u; calls < 8u && n == 0u; calls++) {
            n = ed64_receive(got, sizeof got);
            assert(n != ED64_RECEIVE_LOST && "the stream went out of step behind a busy PI");
        }
        assert(n == 40u && memcmp(got, payload2, 40u) == 0);
        assert(f.host_pos == f.host_len && ed64_receive(got, sizeof got) == 0u);
    }
}

/*
 * The PI busy for good, from each access a clean receive of a message makes in turn, each access
 * waiting as long as pi_io.c does before it fails. However long that lasts, nothing is lost: every
 * call ends within its limit and reports nothing, and when the PI frees the message arrives whole,
 * with the one the host sent after it behind it.
 *
 * This is the case an earlier test here used to hide, by emptying the fake host's stream by hand
 * after a loss.
 */
static void a_pi_busy_for_good_part_way_through_a_message_loses_nothing(void)
{
    uint32_t setup;
    uint32_t accesses;
    uint32_t at;

    pattern(payload, 1100u, 0x13u);
    pattern(payload2, 40u, 0x79u);

    fresh_cart();
    setup = f.pio;
    host_message(F_L3, payload, 1100u);
    assert(ed64_receive(got, sizeof got) == 1100u);
    accesses = f.pio;

    for (at = setup + 1u; at <= accesses; at++) {
        uint32_t calls;
        uint32_t ticks;
        uint32_t n;

        fresh_cart();
        host_message(F_L3, payload, 1100u);
        f.busy_at = at;
        f.busy_len = 0xFFFFFFFFu;
        f.busy_cost = F_PI_WAIT;

        for (calls = 0u; calls < 4u; calls++) {
            n = timed_receive(got, sizeof got, &ticks);
            assert(ticks <= F_RECEIVE_TICKS + F_OVERRUN && "a receive ran past its limit");
            assert(n != ED64_RECEIVE_LOST && "a busy PI lost a message the cart still held");
            assert(n == 0u && "bytes delivered from a message not yet whole");
        }

        /* The PI frees, and the host, having had no reply, sends the request again. */
        f.busy_at = 0u;
        host_message(F_L3, payload2, 40u);
        n = ed64_receive(got, sizeof got);
        assert(n != ED64_RECEIVE_LOST && "the rest of a message was read as a header");
        assert(n == 1100u && memcmp(got, payload, 1100u) == 0);
        assert(ed64_receive(got, sizeof got) == 40u && memcmp(got, payload2, 40u) == 0);
        assert(f.host_pos == f.host_len && ed64_receive(got, sizeof got) == 0u);
    }
}

/*
 * A PI that is slow, not failing: every access waits for the PI and then succeeds, as one can
 * while a game streams from ROM. However slow, each message arrives, over as many calls as it
 * takes, and each call ends within its limit. At 26 ms an access, a message's ten accesses do not
 * fit one call's 100 ms; a driver that gave the message up when its time ran out would lose every
 * message for as long as the PI stayed that slow.
 */
static void a_slow_pi_delays_a_message_and_does_not_lose_it(void)
{
    static const uint32_t ms[] = { 1u, 8u, 14u, 20u, 26u };
    uint32_t k;

    pattern(payload, 1100u, 0x14u);
    pattern(payload2, 100u, 0x7Au);
    for (k = 0u; k < sizeof ms / sizeof ms[0]; k++) {
        uint32_t calls;
        uint32_t ticks;
        uint32_t n = 0u;

        fresh_cart();
        host_message(F_L3, payload, 1100u);
        host_message(F_L3, payload2, 100u);
        f.cost = ms[k] * F_MS;

        for (calls = 0u; calls < 12u && n == 0u; calls++) {
            n = timed_receive(got, sizeof got, &ticks);
            assert(ticks <= F_RECEIVE_TICKS + F_OVERRUN && "a receive ran past its limit");
            assert(n != ED64_RECEIVE_LOST && "a slow PI lost a message");
        }
        assert(n == 1100u && memcmp(got, payload, 1100u) == 0);
        for (n = 0u; calls < 24u && n == 0u; calls++) {
            n = timed_receive(got, sizeof got, &ticks);
            assert(ticks <= F_RECEIVE_TICKS + F_OVERRUN && "a receive ran past its limit");
            assert(n != ED64_RECEIVE_LOST && "a slow PI lost a message");
        }
        assert(n == 100u && memcmp(got, payload2, 100u) == 0);
        assert(f.host_pos == f.host_len);
    }
}

/*
 * The host's header promises more than it sends. The read never finishes; the wait gives up and
 * abandons it, the message is reported lost, and the next message the host sends is delivered.
 */
static void a_short_message_is_abandoned_and_the_next_one_arrives(void)
{
    uint32_t ticks;

    fresh_cart();
    pattern(payload, 100u, 0x61u);
    pattern(payload2, 24u, 0x62u);
    host_raw("DMA@\x01\x00\x02\x00", 8u);
    host_raw(payload, 100u);
    assert(timed_receive(got, sizeof got, &ticks) == ED64_RECEIVE_LOST);
    assert(ticks <= F_RECEIVE_TICKS + F_OVERRUN);
    assert(f.rd_want == 0u && "the read was left active");

    host_message(F_L3, payload2, 24u);
    assert(ed64_receive(got, sizeof got) == 24u && memcmp(got, payload2, 24u) == 0);
    assert(f.host_pos == f.host_len && ed64_receive(got, sizeof got) == 0u);

    /* The same while a message that is not wanted is being drained: there is nothing to report,
       since none of it was the agent's, and the next one still arrives. */
    fresh_cart();
    host_raw("DMA@\x02\x00\x02\x00", 8u);
    host_raw(payload, 100u);
    assert(ed64_receive(got, sizeof got) == 0u && "a message of another datatype");
    assert(timed_receive(got, sizeof got, &ticks) == 0u && f.rd_want == 0u);
    assert(ticks <= F_RECEIVE_TICKS + F_OVERRUN);
    host_message(F_L3, payload2, 24u);
    assert(ed64_receive(got, sizeof got) == 24u && memcmp(got, payload2, 24u) == 0);

    /* A header cut short, and a trailer. */
    fresh_cart();
    host_raw("DMA@", 4u);
    assert(ed64_receive(got, sizeof got) == ED64_RECEIVE_LOST && f.rd_want == 0u);
    host_message(F_L3, payload2, 24u);
    assert(ed64_receive(got, sizeof got) == 24u && memcmp(got, payload2, 24u) == 0);

    fresh_cart();
    host_raw("DMA@\x01\x00\x00\x64", 8u);
    host_raw(payload, 100u);
    host_raw("CM", 2u);
    assert(ed64_receive(got, sizeof got) == ED64_RECEIVE_LOST && f.rd_want == 0u);
    host_message(F_L3, payload2, 24u);
    assert(ed64_receive(got, sizeof got) == 24u && memcmp(got, payload2, 24u) == 0);
    assert(f.host_pos == f.host_len && ed64_receive(got, sizeof got) == 0u);
}

/*
 * Abandoning a read is a PI access too, and can fail. The read is then still active in the cart,
 * and the driver must go on treating it as its own: were it to forget the read there, bytes the
 * host sent late would finish it behind the driver's back and put the stream out of step. The
 * next call's wait ends the same way, the abandon goes through, and the loss is reported once.
 */
static void a_read_that_could_not_be_abandoned_is_abandoned_later(void)
{
    uint32_t ticks;

    fresh_cart();
    pattern(payload, 100u, 0x67u);
    pattern(payload2, 24u, 0x68u);
    host_raw("DMA@\x01\x00\x02\x00", 8u);
    host_raw(payload, 100u);
    f.fail_rdnop = 1;
    assert(timed_receive(got, sizeof got, &ticks) == 0u && "nothing is known to be lost yet");
    assert(ticks <= F_RECEIVE_TICKS + F_OVERRUN);
    assert(f.rd_want != 0u && "the fake abandoned a read whose abandon it failed");

    assert(timed_receive(got, sizeof got, &ticks) == ED64_RECEIVE_LOST && f.rd_want == 0u);
    assert(ticks <= F_RECEIVE_TICKS + F_OVERRUN);
    host_message(F_L3, payload2, 24u);
    assert(ed64_receive(got, sizeof got) == 24u && memcmp(got, payload2, 24u) == 0);
    assert(f.host_pos == f.host_len && ed64_receive(got, sizeof got) == 0u);

    /* An abandon that was stored and then reported failed did abandon the read, and counts as
       having: were the driver to go on waiting for that read, it would find the USB unit idle,
       load the window the read never filled, and take the next message's first bytes as this
       one's trailer. */
    fresh_cart();
    host_raw("DMA@\x01\x00\x00\x64", 8u);
    host_raw(payload, 60u);
    f.land_fail = F_MODE_RDNOP;
    assert(ed64_receive(got, sizeof got) == ED64_RECEIVE_LOST && f.rd_want == 0u);
    assert(f.land_fail == 0u && "the abandon that fails to land was never made");
    host_message(F_L3, payload2, 24u);
    assert(ed64_receive(got, sizeof got) == 24u && memcmp(got, payload2, 24u) == 0);
    assert(f.host_pos == f.host_len && ed64_receive(got, sizeof got) == 0u);
}

/*
 * A caller with no space to offer gets 0, and the driver reads nothing: what is queued stays
 * queued, and a loss waiting on USB behind it is not reported in front of it.
 */
static void no_space_offered_reads_nothing(void)
{
    uint32_t reads;

    fresh_cart();
    pattern(payload, 100u, 0x69u);
    pattern(payload2, 24u, 0x6Au);
    host_message(F_L3, payload, 100u);
    host_message_framed("DMA@", F_L3, payload2, 24u, "CMPX");
    assert(ed64_receive(got, 50u) == 50u && memcmp(got, payload, 50u) == 0);

    reads = f.reads;
    assert(ed64_receive(got, 0u) == 0u && f.reads == reads && "USB was read past queued bytes");
    assert(ed64_receive(got, sizeof got) == 50u && memcmp(got, payload + 50u, 50u) == 0);
    assert(ed64_receive(got, sizeof got) == ED64_RECEIVE_LOST);
    assert(ed64_receive(got, sizeof got) == 0u && f.host_pos == f.host_len);
}

/* A host whose bytes are still on their way is waited for: the read finishes when they arrive. */
static void a_message_still_arriving_is_waited_for(void)
{
    fresh_cart();
    pattern(payload, HOST_MESSAGE, 0x63u);
    host_message(F_L3, payload, HOST_MESSAGE);
    f.host_hold = 300u;
    f.hold_reads = 50u;
    assert(ed64_receive(got, sizeof got) == HOST_MESSAGE && memcmp(got, payload, HOST_MESSAGE) == 0);
    assert(f.host_pos == f.host_len && ed64_receive(got, sizeof got) == 0u);
}

/*
 * The driver puts a message together in its own 2 KiB before it hands any of it over. One that
 * size is held, however little space the caller offers; one byte more is lost, and drained so that
 * the message behind it is not.
 */
static void a_message_too_big_for_the_driver_to_hold_is_lost(void)
{
    fresh_cart();
    pattern(payload, 4000u, 0x71u);
    pattern(payload2, 24u, 0x72u);

    host_message(F_L3, payload, F_QUEUE + 1u);
    host_message(F_L3, payload2, 24u);
    assert(ed64_receive(got, sizeof got) == ED64_RECEIVE_LOST);
    assert(ed64_receive(got, sizeof got) == 24u && memcmp(got, payload2, 24u) == 0 && "not drained");

    /* Exactly that much is kept, and comes out in the pieces asked for. */
    host_message(F_L3, payload, F_QUEUE);
    assert(ed64_receive(got, 80u) == 80u && memcmp(got, payload, 80u) == 0);
    assert(ed64_receive(got, sizeof got) == F_QUEUE - 80u && memcmp(got, payload + 80u, F_QUEUE - 80u) == 0);

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

/*
 * A message of another datatype is not the agent's: it is taken off USB unread, a call at a time,
 * and the L3 message behind it is delivered.
 */
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
 * A message being drained is as long as its header says, up to 16 MiB, and no single call stays
 * for all of it: each ends within its limit, and the next takes up where it stopped. Here every PI
 * access waits 3 ms for the PI and then succeeds, so no access fails and only the clock ends a
 * call. The driver used to drain the whole message in the call that met it.
 */
static void a_long_message_is_drained_across_calls_each_within_its_limit(void)
{
    uint32_t calls;
    uint32_t ticks;
    uint32_t n = 0u;

    fresh_cart();
    pattern(payload, 12000u, 0x83u);
    pattern(payload2, 20u, 0x84u);
    host_message(0x02u, payload, 12000u);
    host_message(F_L3, payload2, 20u);
    f.cost = 3u * F_MS;

    for (calls = 0u; calls < 8u && n == 0u; calls++) {
        n = timed_receive(got, sizeof got, &ticks);
        assert(ticks <= F_RECEIVE_TICKS + F_OVERRUN && "a receive ran past its limit");
        assert(n != ED64_RECEIVE_LOST);
    }
    assert(calls > 2u && "12 KiB at 3 ms an access does not fit one call's limit");
    assert(n == 20u && memcmp(got, payload2, 20u) == 0);
    assert(f.host_pos == f.host_len);
}

/*
 * A read whose request pi_io stored and then could not confirm may have started, so it is treated
 * as started: asked again, it would take the next bytes of the stream as well. The fake starts it.
 */
static void a_read_request_that_was_stored_but_did_not_land_counts_as_made(void)
{
    uint32_t skip;

    /* The first message's 5 reads in turn: its header, its payload's 3, and its trailer. */
    for (skip = 0u; skip < 5u; skip++) {
        fresh_cart();
        pattern(payload, 1100u, 0x92u);
        pattern(payload2, 40u, 0x93u);
        host_message(F_L3, payload, 1100u);
        host_message(F_L3, payload2, 40u);
        f.land_fail = F_MODE_RD;
        f.land_skip = skip;

        assert(ed64_receive(got, sizeof got) == 1100u && memcmp(got, payload, 1100u) == 0);
        assert(f.land_fail == 0u && "the request that fails to land was never made");
        assert(ed64_receive(got, sizeof got) == 40u && memcmp(got, payload2, 40u) == 0);
        assert(f.host_pos == f.host_len && f.reads == 8u && "a read was asked for twice");
    }
}

/*
 * The same for a write: a block whose transfer was stored and not confirmed is not sent again.
 * start_block() has always done this; until now the fake could not produce the case.
 */
static void a_write_request_that_was_stored_but_did_not_land_is_not_sent_twice(void)
{
    static uint8_t body[1500];
    uint32_t total = 8u + sizeof body + 4u;
    uint32_t skip;

    pattern(body, sizeof body, 0x94u);
    /* Each of the message's 3 blocks in turn. */
    for (skip = 0u; skip < 3u; skip++) {
        fresh_cart();
        f.land_fail = F_MODE_WR;
        f.land_skip = skip;
        assert(ed64_send(body, sizeof body) == 1);
        assert(f.land_fail == 0u && "the request that fails to land was never made");
        assert(f.writes == 3u && f.sent_len == total && "a block went out twice");
        assert(memcmp(f.sent + 8, body, sizeof body) == 0);
        assert(memcmp(f.sent + 8 + sizeof body, "CMPH", 4u) == 0);
    }
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
 * A PI busy while a send reads ahead loses nothing either. The PI is busy for every window load
 * from the second message's payload on: the send gives up, having written nothing, and ends within
 * its limit. Afterward all three messages come out in order, the second taken up from where the
 * send left it. A header cut short while reading ahead is a loss, and is reported where it fell.
 */
static void a_busy_pi_while_reading_ahead_sends_nothing_and_loses_nothing(void)
{
    uint8_t body[8];
    uint32_t t0;

    fresh_cart();
    pattern(payload, 100u, 0xB4u);
    pattern(payload2, 20u, 0xB5u);
    pattern(body, sizeof body, 0xB6u);
    host_message(F_L3, payload, 100u);
    host_message(F_L3, payload, HOST_MESSAGE);
    host_message(F_L3, payload2, 20u);
    /* Loads: three for the first message, the second's header, then its payload. */
    f.fail_loads_from = f.loads + 5u;
    f.busy_cost = F_PI_WAIT;

    t0 = f.now;
    assert(ed64_send(body, sizeof body) == 0);
    assert(f.now - t0 <= F_SEND_TICKS + F_OVERRUN && "a send ran past its limit");
    assert(f.writes == 0u && f.sent_len == 0u && "a write was started with host data unread");

    f.fail_loads_from = 0u;
    assert(ed64_receive(got, sizeof got) == 100u && memcmp(got, payload, 100u) == 0);
    assert(ed64_receive(got, sizeof got) == HOST_MESSAGE && memcmp(got, payload, HOST_MESSAGE) == 0);
    assert(ed64_receive(got, sizeof got) == 20u && memcmp(got, payload2, 20u) == 0);
    assert(ed64_receive(got, sizeof got) == 0u && f.host_pos == f.host_len);

    /* A whole message, then half a header: the send reads the first, loses its place in the
       second, and goes ahead, since nothing is left waiting. */
    fresh_cart();
    host_message(F_L3, payload, 100u);
    host_raw("DMA@", 4u);
    assert(ed64_send(body, sizeof body) == 1 && f.sent_len == 20u);
    host_message(F_L3, payload2, 20u);
    assert(ed64_receive(got, sizeof got) == 100u && memcmp(got, payload, 100u) == 0);
    assert(ed64_receive(got, sizeof got) == ED64_RECEIVE_LOST);
    assert(ed64_receive(got, sizeof got) == 20u && memcmp(got, payload2, 20u) == 0);
    assert(ed64_receive(got, sizeof got) == 0u && f.host_pos == f.host_len);
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

/*
 * An X7 under load sent the header and first 4 blocks of a 4122-byte reply, and then nothing more
 * of it (n64/bringup, 2026-10-09): a PI access failed part-way, and ed64_send gave up with the rest
 * unsent. The host counted the cart's later messages as that reply's payload until it gave up.
 *
 * Here the PI is busy for 3 accesses in a row, starting at each access a clean send of that reply
 * makes in turn. Busy before the first block has started, nothing is sent; busy at any access after,
 * the message still arrives whole, and only once.
 */
static void a_busy_pi_never_cuts_a_message_off(void)
{
    static uint8_t body[4122];
    uint32_t total = 8u + sizeof body + 4u;
    uint32_t setup;
    uint32_t accesses;
    uint32_t first_wr;
    uint32_t at;

    /* fresh_cart makes the same accesses every time, so access numbers line up across runs. */
    pattern(body, sizeof body, 0xD1u);
    fresh_cart();
    setup = f.pio;
    assert(ed64_send(body, sizeof body) == 1 && f.sent_len == total && f.writes == 9u);
    accesses = f.pio;
    first_wr = f.first_wr_pio;

    for (at = setup + 1u; at <= accesses; at++) {
        int ok;

        fresh_cart();
        f.busy_at = at;
        f.busy_len = 3u;
        ok = ed64_send(body, sizeof body);
        if (at <= first_wr) {
            assert(ok == 0 && f.sent_len == 0u && "a busy PI before the first block sent part of it");
        } else {
            assert(ok == 1 && "a busy PI after the first block made the send give up");
            assert(f.sent_len == total && "a message went out cut off, or a block went out twice");
            assert(memcmp(f.sent, "DMA@", 4u) == 0 && memcmp(f.sent + 8, body, sizeof body) == 0);
            assert(memcmp(f.sent + 8 + sizeof body, "CMPH", 4u) == 0);
        }
    }
}

/*
 * A PI that stays busy for good once the first block is out still ends the send, by the clock:
 * every access waits as long as pi_io.c does and fails, the send keeps trying for as long as it
 * has, and returns within its limit with the host left the part that went out.
 */
static void a_pi_busy_for_good_still_ends_the_send(void)
{
    static uint8_t body[4122];
    uint32_t first_wr;
    uint32_t t0;

    pattern(body, sizeof body, 0xE1u);
    fresh_cart();
    assert(ed64_send(body, sizeof body) == 1);
    first_wr = f.first_wr_pio;

    fresh_cart();
    f.busy_at = first_wr + 1u;
    f.busy_len = 0xFFFFFFFFu;
    f.busy_cost = F_PI_WAIT;
    t0 = f.now;
    assert(ed64_send(body, sizeof body) == 0);
    assert(f.sent_len == F_WINDOW && "only the first block went out");
    assert(f.now - t0 >= F_SEND_TICKS && "the send gave up with time left to finish the message");
    assert(f.now - t0 <= F_SEND_TICKS + F_OVERRUN && "a send ran past its limit");
}

/*
 * A PI that is slow, not failing, ends a send by the clock as well: every access waits 20 ms for
 * the PI and then succeeds, as one can while a game streams from ROM. Nothing fails, so no count of
 * failed accesses ever stopped this; the old driver sent all 9 blocks, and took 0.7 s over it.
 */
static void a_slow_pi_does_not_hold_a_send_past_its_limit(void)
{
    static uint8_t body[4122];
    uint32_t t0;

    pattern(body, sizeof body, 0xE2u);
    fresh_cart();
    f.cost = 20u * F_MS;
    t0 = f.now;
    assert(ed64_send(body, sizeof body) == 0);
    assert(f.now - t0 <= F_SEND_TICKS + F_OVERRUN && "a send ran past its limit");
    assert(f.sent_len > 0u && f.sent_len % F_WINDOW == 0u && "whole blocks, and then no more");
}

/*
 * A transfer the cart never finishes is given up by the clock, not by a count of status reads.
 * With every access waiting 20 ms for the PI, 20,000 status reads are 400 s: that is how long one
 * wait on a held transfer could last, for a read and for a write alike.
 */
static void a_transfer_that_never_finishes_is_given_up_by_the_clock(void)
{
    static uint8_t body[600];
    uint32_t ticks;
    uint32_t t0;

    /* A read: the host's header promises more than it sends. The call that asks for the payload
       runs out of time waiting, which loses nothing yet, and the next abandons the read. */
    fresh_cart();
    pattern(payload, 100u, 0x64u);
    pattern(payload2, 24u, 0x65u);
    host_raw("DMA@\x01\x00\x02\x00", 8u);
    host_raw(payload, 100u);
    f.cost = 20u * F_MS;
    assert(timed_receive(got, sizeof got, &ticks) == 0u);
    assert(ticks <= F_RECEIVE_TICKS + F_OVERRUN && "a receive ran past its limit");
    assert(timed_receive(got, sizeof got, &ticks) == ED64_RECEIVE_LOST);
    assert(ticks <= F_RECEIVE_TICKS + F_OVERRUN && "a receive ran past its limit");
    assert(f.rd_want == 0u && "the read was left active");
    assert(timed_receive(got, sizeof got, &ticks) == 0u && "one message was reported lost twice");
    f.cost = F_COST;
    host_message(F_L3, payload2, 24u);
    assert(ed64_receive(got, sizeof got) == 24u && memcmp(got, payload2, 24u) == 0);

    /* A write: its first block's transfer stays active. */
    fresh_cart();
    pattern(body, sizeof body, 0x66u);
    f.cost = 20u * F_MS;
    f.hold_wr = 1;
    t0 = f.now;
    assert(ed64_send(body, sizeof body) == 0);
    assert(f.now - t0 <= F_SEND_TICKS + F_OVERRUN && "a send ran past its limit");
    assert(f.writes == 1u && !f.write_stuck && "the write was left active, or made again");

    /* And with the PI idle, where the count of status reads is the shorter limit, as it was. */
    fresh_cart();
    f.hold_wr = 1;
    t0 = f.now;
    assert(ed64_send(body, sizeof body) == 0);
    assert(f.now - t0 <= F_BUSY_TICKS + F_OVERRUN);
    assert(f.writes == 1u && !f.write_stuck);
}

/*
 * agent.c calls ed64_receive until it returns 0 or a loss, up to 32 times a frame, and each call
 * that delivers bytes is followed by another. With a slow PI, 2 ms an access here, every one of
 * those calls is within its own limit, and 16 waiting messages used to be read in one frame: 0.3 s.
 * A run of calls is now held to its own limit and one call more, and what it leaves in the cart is
 * there, whole and in order, for the runs after it.
 */
static void a_run_of_receives_is_held_to_its_limit(void)
{
    uint32_t delivered = 0u;
    uint32_t runs;

    fresh_cart();
    pattern(payload, 16u * HOST_MESSAGE, 0xF1u);
    for (runs = 0u; runs < 16u; runs++) {
        host_message(F_L3, payload + runs * HOST_MESSAGE, HOST_MESSAGE);
    }
    f.cost = 2u * F_MS;

    for (runs = 0u; runs < 32u && delivered < 16u; runs++) {
        uint32_t t0 = f.now;
        uint32_t pulls;

        /* stream_frame(), as far as the driver can tell. */
        for (pulls = 0u; pulls < 32u; pulls++) {
            uint32_t n = ed64_receive(got, sizeof got);

            assert(n != ED64_RECEIVE_LOST);
            if (n == 0u) {
                break;
            }
            assert(n == HOST_MESSAGE && memcmp(got, payload + delivered * HOST_MESSAGE, HOST_MESSAGE) == 0);
            delivered++;
        }
        assert(f.now - t0 <= F_BURST_TICKS + F_RECEIVE_TICKS + F_OVERRUN && "a frame's receives ran on");
    }
    assert(delivered == 16u && runs > 1u && f.host_pos == f.host_len);
}

/*
 * The run's limit is for one run. agent.c stops receiving as soon as it holds a whole frame, so a
 * tick that reads a request and answers it never ends on a 0 from ed64_receive, and the time its
 * receives used must not count against the next tick's. Here every tick reads one request and
 * sends one reply, at 1 ms an access, for long enough to use the run's limit several times over:
 * no tick finds the driver refusing a message that is waiting.
 */
static void a_reply_ends_the_run_it_answers(void)
{
    uint8_t body[8];
    uint32_t tick;

    fresh_cart();
    pattern(payload, 100u, 0xF2u);
    pattern(body, sizeof body, 0xF3u);
    f.cost = 1u * F_MS;
    for (tick = 0u; tick < 40u; tick++) {
        host_message(F_L3, payload, 100u);
        assert(ed64_receive(got, sizeof got) == 100u && "a waiting request was put off to the next tick");
        assert(ed64_send(body, sizeof body) == 1);
    }
    assert(f.host_pos == f.host_len && f.sent_len == 40u * 20u);
}

/* ---- a random run ---------------------------------------------------------------------- */

static uint32_t s_rng;

static uint32_t rnd(uint32_t n)
{
    s_rng = s_rng * 1664525u + 1013904223u;
    return (s_rng >> 8) % n;
}

/* The messages the host has sent in this run, and how far the bytes delivered have got: byte s_co
   of message s_cm is next. s_after_loss says a loss has been reported since the last bytes. */
static struct {
    uint8_t type;
    uint32_t size;
} s_msgs[64];
static uint32_t s_msg_count;
static uint32_t s_cm;
static uint32_t s_co;
static int s_after_loss;

/* Every payload names its message in its even bytes and counts in its odd ones, so a delivered
   byte says where in the stream it came from. */
static uint8_t message_fill(uint32_t message, uint32_t offset)
{
    return (uint8_t)((offset & 1u) ? offset >> 1 : message);
}

static void random_host_message(uint8_t type, uint32_t size, const char *tail)
{
    static uint8_t body[3072];
    uint32_t i;

    assert(s_msg_count < sizeof s_msgs / sizeof s_msgs[0] && size <= sizeof body);
    for (i = 0u; i < size; i++) {
        body[i] = message_fill(s_msg_count, i);
    }
    s_msgs[s_msg_count].type = type;
    s_msgs[s_msg_count].size = size;
    s_msg_count++;
    host_message_framed("DMA@", type, body, size, tail);
}

/** What ed64_receive returned: bytes carry on from the last, and a loss skips whole messages. */
static void check_delivery(const uint8_t *b, uint32_t n)
{
    uint32_t i;

    if (n == ED64_RECEIVE_LOST) {
        assert((s_co == 0u || s_co == s_msgs[s_cm].size) && "a loss fell inside a delivered message");
        s_after_loss = 1;
        return;
    }
    for (i = 0u; i < n; i++) {
        if (s_after_loss) {
            /* After a loss the stream takes up at the start of a message the host sent later. */
            uint32_t first = s_co == 0u ? s_cm : s_cm + 1u;

            assert(b[i] > first && b[i] < s_msg_count && "after a loss, bytes that start no later message");
            s_cm = b[i];
            s_co = 0u;
            s_after_loss = 0;
        }
        while (s_cm < s_msg_count && (s_msgs[s_cm].type != F_L3 || s_co == s_msgs[s_cm].size)) {
            s_cm++;
            s_co = 0u;
        }
        assert(s_cm < s_msg_count && "bytes delivered that the host never sent");
        assert(b[i] == message_fill(s_cm, s_co) && "the stream is out of step");
        s_co++;
    }
}

/*
 * Random traffic through a PI that is busy, slow, or fails a window load or a stored write at
 * random: messages of every size and of other datatypes, received into spaces of every size, with
 * replies sent in between. Whatever happens, the bytes delivered are the host's L3 payloads in
 * order, less whole messages each marked by a loss, every call ends within its limit, and once the
 * PI is left alone everything still in the cart arrives, ending with a message sent last of all.
 *
 * Nothing here cuts a host message short, so the place in the stream is always knowable, and a
 * driver that loses it has a defect.
 */
static void random_busy_and_slow_pi_never_puts_the_stream_out_of_step(void)
{
    static uint8_t body[1600];
    uint32_t run;

    pattern(body, sizeof body, 0x5Au);
    for (run = 0u; run < 400u; run++) {
        uint32_t tick;
        uint32_t ticks;
        uint32_t quiet;
        uint32_t n;

        s_rng = 0xC0FFEEu + run;
        fresh_cart();
        s_msg_count = 0u;
        s_cm = 0u;
        s_co = 0u;
        s_after_loss = 0;

        for (tick = 0u; tick < 60u; tick++) {
            for (n = rnd(3u); n > 0u && f.host_len + 4000u < F_HOST_BYTES && s_msg_count < 60u; n--) {
                /* Mostly what a host sends, 512 bytes or fewer; some longer, and a few too long
                   for the driver to hold, which it must lose and drain. Now and then one of
                   another datatype, or with a bad trailer. */
                uint32_t size = rnd(4u) ? rnd(520u) : rnd(8u) ? rnd(1300u) : 1900u + rnd(700u);

                random_host_message(rnd(8u) ? F_L3 : 0x02u, size, rnd(16u) ? "CMPH" : "CMPX");
            }

            f.busy_at = 0u;
            f.cost = F_COST;
            f.busy_cost = F_COST;
            f.fail_load = 0u;
            f.land_fail = 0u;
            f.fail_rd_before_store = 0;
            if (rnd(4u) == 0u) {
                f.busy_at = f.pio + 1u + rnd(30u);
                f.busy_len = 1u + rnd(60u);
                f.busy_cost = rnd(2u) ? F_COST : F_PI_WAIT;
            }
            if (rnd(4u) == 0u) {
                f.cost = (1u + rnd(40u)) * F_MS / 2u;
            }
            if (rnd(4u) == 0u) {
                f.fail_load = f.loads + 1u + rnd(6u);
            }
            if (rnd(4u) == 0u) {
                f.land_fail = rnd(2u) ? F_MODE_RD : F_MODE_WR;
                f.land_skip = rnd(4u);
            }
            f.fail_rd_before_store = rnd(8u) == 0u;

            if (rnd(4u) == 0u && f.sent_len + 2u * sizeof body < F_HOST_BYTES) {
                uint32_t len = 1u + rnd(sizeof body);
                uint32_t before = f.sent_len;
                uint32_t t0 = f.now;
                int ok = ed64_send(body, len);

                assert(f.now - t0 <= F_SEND_TICKS + F_OVERRUN && "a send ran past its limit");
                if (ok) {
                    assert(f.sent_len - before == ((8u + len + 4u + 1u) & ~1u));
                    assert(memcmp(f.sent + before, "DMA@", 4u) == 0);
                    assert(memcmp(f.sent + before + 8u, body, len) == 0);
                    assert(memcmp(f.sent + before + 8u + len, "CMPH", 4u) == 0);
                }
            } else {
                uint32_t t0 = f.now;
                uint32_t pulls;

                for (pulls = 0u; pulls < 32u; pulls++) {
                    n = timed_receive(got, 1u + rnd(2048u), &ticks);
                    assert(ticks <= F_RECEIVE_TICKS + F_OVERRUN && "a receive ran past its limit");
                    check_delivery(got, n);
                    if (n == 0u || n == ED64_RECEIVE_LOST) {
                        break;
                    }
                }
                assert(f.now - t0 <= F_BURST_TICKS + F_RECEIVE_TICKS + F_OVERRUN && "a frame's receives ran on");
            }
        }

        /* The PI is left alone, and the host sends one message more. */
        f.busy_at = 0u;
        f.cost = F_COST;
        f.busy_cost = F_COST;
        f.fail_load = 0u;
        f.land_fail = 0u;
        f.fail_rd_before_store = 0;
        random_host_message(F_L3, 100u, "CMPH");
        for (quiet = 0u, tick = 0u; quiet < 3u && tick < 400u; tick++) {
            n = ed64_receive(got, sizeof got);
            check_delivery(got, n);
            quiet = n == 0u ? quiet + 1u : 0u;
        }
        assert(quiet == 3u && f.host_pos == f.host_len && "something stayed in the cart");
        assert(!s_after_loss && s_cm == s_msg_count - 1u && s_co == 100u && "the last message did not arrive");
    }
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
static const char *s_only;

static void run(const char *name, void (*test)(void))
{
    if (s_only != 0 && strstr(name, s_only) == 0) {
        return;
    }
    s_case = name;
    alarm(60u);
    test();
    alarm(0u);
    s_cases++;
}

#define RUN(test) run(#test, test)

int main(int argc, char **argv)
{
    s_only = argc > 1 ? argv[1] : 0;
    signal(SIGALRM, on_alarm);
    RUN(a_message_that_fits_is_delivered_whole);
    RUN(a_message_with_less_than_512_bytes_free_is_not_lost);
    RUN(odd_length_payloads_still_drain_their_padding);
    RUN(a_bad_header_or_trailer_is_reported_lost);
    RUN(a_failed_load_loses_nothing);
    RUN(a_read_that_never_started_is_not_a_loss);
    RUN(a_busy_pi_part_way_through_a_message_loses_nothing);
    RUN(a_pi_busy_for_good_part_way_through_a_message_loses_nothing);
    RUN(a_slow_pi_delays_a_message_and_does_not_lose_it);
    RUN(a_short_message_is_abandoned_and_the_next_one_arrives);
    RUN(a_read_that_could_not_be_abandoned_is_abandoned_later);
    RUN(no_space_offered_reads_nothing);
    RUN(a_message_still_arriving_is_waited_for);
    RUN(a_message_too_big_for_the_driver_to_hold_is_lost);
    RUN(a_non_l3_message_is_drained_and_ignored);
    RUN(a_long_message_is_drained_across_calls_each_within_its_limit);
    RUN(a_read_request_that_was_stored_but_did_not_land_counts_as_made);
    RUN(a_write_request_that_was_stored_but_did_not_land_is_not_sent_twice);
    RUN(a_sent_message_puts_its_padding_after_the_trailer);
    RUN(a_sent_even_message_has_no_padding);
    RUN(a_sent_message_longer_than_the_window_keeps_its_framing);
    RUN(a_send_with_host_data_waiting_reads_it_first_and_sends_whole);
    RUN(a_loss_read_ahead_of_a_send_is_reported_in_order);
    RUN(a_busy_pi_while_reading_ahead_sends_nothing_and_loses_nothing);
    RUN(more_waiting_than_the_driver_holds_sends_nothing);
    RUN(a_busy_pi_never_cuts_a_message_off);
    RUN(a_pi_busy_for_good_still_ends_the_send);
    RUN(a_slow_pi_does_not_hold_a_send_past_its_limit);
    RUN(a_transfer_that_never_finishes_is_given_up_by_the_clock);
    RUN(a_run_of_receives_is_held_to_its_limit);
    RUN(a_reply_ends_the_run_it_answers);
    RUN(random_busy_and_slow_pi_never_puts_the_stream_out_of_step);
    if (s_cases == 0) {
        fprintf(stderr, "ed64 driver: no case named like '%s'\n", s_only);
        return 1;
    }
    printf("ed64 driver: %d cases passed\n", s_cases);
    return 0;
}
