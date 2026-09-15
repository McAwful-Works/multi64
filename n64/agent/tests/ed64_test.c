/*
 * Host-side test of the EverDrive-64 X7 driver's receive path.
 *
 * `make host-test` compiles the real ed64.c for the PC with this file in place of pi_io.c: the
 * pi_io_* functions below are a fake cart. It models the X7's USB unit as libdragon's usb.c drives
 * it: USBCFG (a read transfer of n bytes fills the last n bytes of the 512-byte USBDAT window from
 * what the host has sent; the status word carries POWER, RXF while nothing is waiting, and ACT
 * while a transfer is in progress), and the window itself. A read asking for more bytes than the
 * host sent never finishes, and any window load can be made to fail, the way pi_io.c fails when the
 * PI stays busy.
 *
 * Bytes are copied between the window and the driver's words in host byte order, so the driver's
 * byte view of a loaded word is the window's bytes in order, as it is on the big-endian console.
 *
 * It checks that a message is delivered in order and whole, or reported lost, and that every wait
 * is bounded. It says nothing about the cart: the register model is transcribed, not observed, and
 * this driver has never run on an X7.
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
    /* Read transfers started, and window loads made. */
    uint32_t reads;
    uint32_t loads;
    /* The load with this 1-based number fails; 0 for none. */
    uint32_t fail_load;
} f;

int pi_io_read(uint32_t addr, uint32_t *value)
{
    if (addr == F_USBCFG) {
        *value = F_POWER | (f.host_pos == f.host_len ? F_RXF : 0u) | (f.stuck ? F_ACT : 0u);
    } else if (addr == F_VERSION) {
        *value = 0xED640013u;
    } else {
        assert(0 && "a read of a register the fake does not model");
    }
    return 1;
}

int pi_io_write(uint32_t addr, uint32_t value)
{
    if (addr == F_KEY || addr == F_SYSCFG) {
        return 1;
    }
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
    } else {
        assert((value & F_MODE_MASK) == F_MODE_RDNOP && "only the receive path is modelled");
    }
    return 1;
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
    (void)src;
    (void)addr;
    (void)words;
    assert(0 && "only the receive path is modelled");
    return 0;
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
    RUN(a_read_that_never_finishes_is_reported_lost);
    RUN(a_message_too_big_even_with_the_leftover_buffer_is_lost);
    RUN(a_non_l3_message_is_drained_and_ignored);
    printf("ed64 driver: %d cases passed\n", s_cases);
    return 0;
}
