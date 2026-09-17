/*
 * Host-side test of the SummerCart64 driver's failure handling.
 *
 * `make host-test` compiles the real sc64.c for the PC with SC64_HOST_TEST defined, which routes its
 * PI loads and stores and its interrupt mask through the fake cart below. The fake models the
 * command register, the 8 KiB staging buffer (whose stores land only while ROM writes are enabled,
 * as observed on hardware) and PI_STATUS, and can hold PI_STATUS busy so the driver's bounded waits
 * give up the way they do when a game's DMA holds the bus.
 *
 * It checks that no wait is unbounded, that no failure is reported as success, and that the cart
 * is not left writable. It says nothing about the cart: whether the register model and the wait
 * bounds match an SC64 are hardware questions.
 */
#undef NDEBUG
#include <assert.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "sc64.h"

uint32_t sc64_test_pi_load(uint32_t addr);
void sc64_test_pi_store(uint32_t addr, uint32_t value);
uint32_t sc64_test_int_mask(void);
void sc64_test_int_restore(uint32_t sr);

/* Physical addresses, as sc64.c's register map has them. */
#define F_PI_STATUS 0x04600010u
#define F_DMA_BUSY 0x1u
#define F_IO_BUSY 0x2u
#define F_SR_CMD 0x1FFF0000u
#define F_DATA_0 0x1FFF0004u
#define F_DATA_1 0x1FFF0008u
#define F_IDENT 0x1FFF000Cu
#define F_KEY 0x1FFF0010u
#define F_BUFFER 0x1FFE0000u
#define F_BUFFER_WORDS 2048u
#define F_CMD_BUSY 0x80000000u

#define FOREVER 0xFFFFFFFFu

/*
 * sc64.c gives up on a busy PI after PI_WAIT_SPINS (100000) reads of PI_STATUS. A busy spell of
 * BUSY_ONE_WAIT outlasts one wait but not two; BUSY_TWO_WAITS outlasts two.
 */
#define BUSY_ONE_WAIT 150000u
#define BUSY_TWO_WAITS 250000u

enum trigger { ON_NOTHING, ON_STORE, ON_LOAD };

static struct {
    uint32_t data[2];
    uint32_t rom_write;
    uint32_t buffer[F_BUFFER_WORDS];

    /* The host's next packet, until a USB_READ stages it. */
    uint8_t host_type;
    uint32_t host_len;
    uint32_t host_words[F_BUFFER_WORDS];
    int usb_reads;
    /* USB_READ_STATUS keeps reporting a transfer in progress. */
    int usb_read_stuck;

    /* The last packet a USB_WRITE sent. */
    uint32_t sent_len;
    uint32_t sent_words[F_BUFFER_WORDS];
    int usb_writes;
    /* USB_WRITE_STATUS reports the previous transfer still in progress. */
    int usb_write_busy;

    /* PI_STATUS reports busy_bits for the next busy_reads reads, or until cleared if FOREVER. */
    uint32_t busy_bits;
    uint32_t busy_reads;
    /* SR_CMD reports a command in progress for this many reads. */
    uint32_t cmd_busy_reads;

    /* One shot: after a store of trig_value to trig_addr (ON_STORE), or a load of trig_addr while
       rom_write equals trig_value (ON_LOAD), PI_STATUS goes busy. */
    enum trigger trig_on;
    uint32_t trig_addr;
    uint32_t trig_value;
    uint32_t trig_bits;
    uint32_t trig_reads;

    int masked;
    /* Cart loads and stores made while PI_STATUS said the bus was busy. */
    int access_while_busy;
} f;

static void arm(enum trigger on, uint32_t addr, uint32_t value, uint32_t bits, uint32_t reads)
{
    f.trig_on = on;
    f.trig_addr = addr;
    f.trig_value = value;
    f.trig_bits = bits;
    f.trig_reads = reads;
}

static void fire(void)
{
    f.trig_on = ON_NOTHING;
    f.busy_bits = f.trig_bits;
    f.busy_reads = f.trig_reads;
}

static void bus_recovers(void)
{
    f.busy_reads = 0u;
}

static int in_buffer(uint32_t a)
{
    return a >= F_BUFFER && a < F_BUFFER + 4u * F_BUFFER_WORDS;
}

static void command(uint32_t cmd)
{
    switch (cmd & 0xFFu) {
    case 'C':
        if (f.data[0] == 1u) {
            uint32_t old = f.rom_write;
            f.rom_write = f.data[1];
            f.data[1] = old;
        }
        f.data[0] = 0u;
        break;
    case 'U':
        f.data[0] = f.usb_write_busy ? F_CMD_BUSY : 0u;
        f.data[1] = 0u;
        break;
    case 'M':
        assert(f.data[0] == F_BUFFER && (f.data[1] & 0x00FFFFFFu) <= 4u * F_BUFFER_WORDS);
        f.sent_len = f.data[1] & 0x00FFFFFFu;
        memcpy(f.sent_words, f.buffer, sizeof(uint32_t) * ((f.sent_len + 3u) / 4u));
        f.usb_writes++;
        f.data[0] = 0u;
        f.data[1] = 0u;
        break;
    case 'u':
        f.data[0] = (f.usb_read_stuck ? F_CMD_BUSY : 0u) | f.host_type;
        f.data[1] = f.host_len;
        break;
    case 'm':
        assert(f.data[0] == F_BUFFER && f.data[1] == f.host_len);
        memcpy(f.buffer, f.host_words, sizeof(uint32_t) * ((f.host_len + 3u) / 4u));
        f.host_len = 0u;
        f.host_type = 0u;
        f.usb_reads++;
        f.data[0] = 0u;
        f.data[1] = 0u;
        break;
    default:
        assert(0 && "a command the fake does not model");
    }
}

void sc64_test_pi_store(uint32_t addr, uint32_t value)
{
    uint32_t a = addr & 0x1FFFFFFFu;

    if (f.busy_reads > 0u) {
        f.access_while_busy++;
    }
    if (a == F_DATA_0) {
        f.data[0] = value;
    } else if (a == F_DATA_1) {
        f.data[1] = value;
    } else if (a == F_SR_CMD) {
        command(value);
    } else if (a == F_KEY) {
        /* unlock sequence: nothing to model */
    } else if (in_buffer(a)) {
        if (f.rom_write) {
            f.buffer[(a - F_BUFFER) / 4u] = value;
        }
    } else {
        assert(0 && "a store outside the cart's registers and buffer");
    }
    if (f.trig_on == ON_STORE && a == f.trig_addr && value == f.trig_value) {
        fire();
    }
}

uint32_t sc64_test_pi_load(uint32_t addr)
{
    uint32_t a = addr & 0x1FFFFFFFu;
    uint32_t v;

    if (a == F_PI_STATUS) {
        if (f.busy_reads == 0u) {
            return 0u;
        }
        if (f.busy_reads != FOREVER) {
            f.busy_reads--;
        }
        return f.busy_bits;
    }
    if (f.busy_reads > 0u) {
        f.access_while_busy++;
    }
    if (a == F_SR_CMD) {
        v = 0u;
        if (f.cmd_busy_reads > 0u) {
            if (f.cmd_busy_reads != FOREVER) {
                f.cmd_busy_reads--;
            }
            v = F_CMD_BUSY;
        }
    } else if (a == F_DATA_0) {
        v = f.data[0];
    } else if (a == F_DATA_1) {
        v = f.data[1];
    } else if (a == F_IDENT) {
        v = 0x53437632u; /* 'SCv2' */
    } else if (in_buffer(a)) {
        v = f.buffer[(a - F_BUFFER) / 4u];
    } else {
        assert(0 && "a load outside the cart's registers and buffer");
        v = 0u;
    }
    if (f.trig_on == ON_LOAD && a == f.trig_addr && f.rom_write == f.trig_value) {
        fire();
    }
    return v;
}

uint32_t sc64_test_int_mask(void)
{
    f.masked++;
    return 0x5A5A0001u;
}

void sc64_test_int_restore(uint32_t sr)
{
    assert(sr == 0x5A5A0001u && f.masked > 0);
    f.masked--;
}

/* ---- helpers -------------------------------------------------------------------- */

/* Word-aligned, as the agent's own buffers are for the driver's word copies. */
static uint32_t msg[F_BUFFER_WORDS];
static uint32_t got[F_BUFFER_WORDS];

static void fill(uint32_t *w, uint32_t words, uint32_t seed)
{
    uint32_t i;
    for (i = 0u; i < words; i++) {
        w[i] = seed + i * 0x01010101u;
    }
}

static void host_sends(uint8_t type, const uint32_t *words, uint32_t len)
{
    f.host_type = type;
    f.host_len = len;
    memcpy(f.host_words, words, sizeof(uint32_t) * ((len + 3u) / 4u));
}

static void fresh_cart(void)
{
    memset(&f, 0, sizeof f);
    assert(sc64_init() == 1);
}

/* ---- cases ------------------------------------------------------------------------ */

static void a_request_and_its_reply_round_trip(void)
{
    uint8_t dt = 0u;

    fresh_cart();
    fill(msg, 6u, 0x4D363442u);
    host_sends(1u, msg, 24u);
    assert(sc64_poll(&dt) == 24u && dt == 1u && f.usb_reads == 1);
    assert(sc64_read(got, 0u, 24u) == 1 && memcmp(got, msg, 24u) == 0);

    fill(msg, 6u, 0x11110000u);
    assert(sc64_write(1u, msg, 24u) == 1);
    assert(f.usb_writes == 1 && f.sent_len == 24u && memcmp(f.sent_words, msg, 24u) == 0);
    assert(f.rom_write == 0u && f.masked == 0 && f.access_while_busy == 0);
}

/*
 * #153: io_write gave up silently, so a USB_READ whose command store never landed was reported as
 * a staged packet, and the agent read its own last reply back out of the buffer as a request.
 */
static void a_dropped_usb_read_is_not_reported_as_a_packet(void)
{
    uint8_t dt = 0u;

    fresh_cart();
    fill(f.buffer, 6u, 0x5245504Cu); /* the agent's own previous reply, still staged */
    fill(msg, 6u, 0x4D363442u);
    host_sends(1u, msg, 24u);
    /* The PI goes busy just after USB_READ's arguments land, for longer than one wait. */
    arm(ON_STORE, F_DATA_1, 24u, F_DMA_BUSY, BUSY_ONE_WAIT);
    assert(sc64_poll(&dt) == 0u && "USB_READ never reached the cart: there is no packet to read");
    assert(f.usb_reads == 0 && f.masked == 0);

    /* Nothing is lost: the packet is still waiting for the next poll. */
    bus_recovers();
    assert(sc64_poll(&dt) == 24u && f.usb_reads == 1);
    assert(sc64_read(got, 0u, 24u) == 1 && memcmp(got, msg, 24u) == 0);
}

/*
 * #153: a CONFIG_SET restoring write-enable that did not land left the cart writable under the
 * running game, and the driver never tried again.
 */
static void a_failed_restore_of_write_enable_is_retried(void)
{
    uint8_t dt = 0u;

    fresh_cart();
    fill(msg, 6u, 0x22220000u);
    /* The PI goes busy just after the restoring CONFIG_SET's arguments land. */
    arm(ON_STORE, F_DATA_1, 0u, F_DMA_BUSY, BUSY_ONE_WAIT);
    (void)sc64_write(1u, msg, 24u);
    assert(f.masked == 0);
    bus_recovers();
    assert(sc64_poll(&dt) == 0u);
    assert(f.rom_write == 0u && "the next poll must put write-enable back");

    /* Again, with a write rather than a poll next: the value put back is the one from before the
       agent's first write, not the writable state its failed restore left behind. */
    arm(ON_STORE, F_DATA_1, 0u, F_DMA_BUSY, BUSY_ONE_WAIT);
    (void)sc64_write(1u, msg, 24u);
    bus_recovers();
    assert(sc64_write(1u, msg, 24u) == 1);
    assert(f.rom_write == 0u && f.masked == 0);
}

/*
 * #222: an enable that never reached the cart still ran the restore, which put back
 * s_rom_write_restore -- 0 before any write had succeeded -- and switched off a write-enable the
 * agent had never touched.
 */
static void a_write_enable_that_never_reached_the_cart_changes_nothing(void)
{
    uint8_t dt = 0u;

    fresh_cart();
    f.rom_write = 1u; /* on before the agent ran */
    fill(msg, 6u, 0x44440000u);
    /* The PI goes busy as soon as the enable's arguments land, for longer than one wait: the
       command store itself gives up, so CONFIG_SET never runs. */
    arm(ON_STORE, F_DATA_1, 1u, F_DMA_BUSY, BUSY_ONE_WAIT);
    assert(sc64_write(1u, msg, 24u) == 0 && f.usb_writes == 0);
    assert(f.rom_write == 1u && "write-enable was switched off by a restore of a change never made");
    bus_recovers();
    assert(sc64_poll(&dt) == 0u && f.rom_write == 1u && "nor put back later as a pending restore");
    assert(f.masked == 0);
}

/*
 * The other side of #222: an enable whose command did reach the cart, but whose completion could
 * not be read, may have made the cart writable, so it is still put back.
 */
static void a_write_enable_that_may_have_run_is_still_put_back(void)
{
    fresh_cart();
    fill(msg, 6u, 0x55550000u);
    /* CONFIG_SET runs as its command store lands; then the PI goes busy for longer than the wait
       to read that it finished. */
    arm(ON_STORE, F_SR_CMD, 'C', F_DMA_BUSY, BUSY_ONE_WAIT);
    assert(sc64_write(1u, msg, 24u) == 0 && f.usb_writes == 0);
    assert(f.rom_write == 0u && "the cart was left writable");
    assert(f.masked == 0);
}

/*
 * #226: a restore still pending when sc64_write runs is retried there first, as it is by
 * sc64_poll, so a game that only writes does not keep the cart writable.
 */
static void a_pending_restore_is_retried_by_a_write_that_stops_early(void)
{
    fresh_cart();
    fill(msg, 6u, 0x66660000u);
    arm(ON_STORE, F_DATA_1, 0u, F_DMA_BUSY, BUSY_ONE_WAIT);
    (void)sc64_write(1u, msg, 24u);
    assert(f.rom_write == 1u && "the restore was meant to fail");
    bus_recovers();
    /* The next write finds a transfer still in progress and returns before staging anything. */
    f.usb_write_busy = 1;
    assert(sc64_write(1u, msg, 24u) == 0);
    assert(f.rom_write == 0u && "the pending restore waited for a poll");
    assert(f.masked == 0);
}

/* #152: pi_copy's IO_BUSY loops had no bound, and ran with interrupts masked. */
static void a_bus_that_stays_busy_during_a_copy_does_not_hang(void)
{
    uint8_t dt = 0u;

    fresh_cart();
    fill(msg, 6u, 0x0BADF00Du);
    /* IO_BUSY sticks after the first staging store. */
    arm(ON_STORE, F_BUFFER, msg[0], F_IO_BUSY, FOREVER);
    assert(sc64_write(1u, msg, 24u) == 0);
    assert(f.masked == 0 && f.usb_writes == 0);

    /* Once the bus recovers, the write-enable the failed copy left set is put back. */
    bus_recovers();
    assert(sc64_poll(&dt) == 0u && f.rom_write == 0u);
    assert(sc64_write(1u, msg, 24u) == 1 && f.usb_writes == 1 && f.rom_write == 0u);
}

/* #152: pi_copy ignored pi_wait_idle() failing, and stored into cart space while a DMA held the bus. */
static void a_copy_does_not_touch_the_cart_while_a_dma_holds_the_bus(void)
{
    uint8_t dt = 0u;

    fresh_cart();
    fill(msg, 6u, 0x33330000u);
    /* A game DMA starts just after write-enable is set, and outlasts both of the copy's idle waits. */
    arm(ON_LOAD, F_DATA_1, 1u, F_DMA_BUSY, BUSY_TWO_WAITS);
    assert(sc64_write(1u, msg, 24u) == 0);
    assert(f.access_while_busy == 0 && "the copy reached the cart while the PI was busy");
    assert(f.masked == 0 && f.usb_writes == 0);
    bus_recovers();
    assert(sc64_poll(&dt) == 0u && f.rom_write == 0u);
}

/* #152: the read direction too. A failed read must say so, not hand back whatever it loaded. */
static void a_read_while_the_bus_stays_busy_fails(void)
{
    uint8_t dt = 0u;

    fresh_cart();
    fill(msg, 6u, 0x4D363442u);
    host_sends(1u, msg, 24u);
    assert(sc64_poll(&dt) == 24u);
    f.busy_bits = F_DMA_BUSY;
    f.busy_reads = FOREVER;
    assert(sc64_read(got, 0u, 24u) == 0);
    assert(f.access_while_busy == 0 && f.masked == 0);
}

/* #153: the SR_CMD busy loop and sc64_poll's USB_READ_STATUS loop had no bound. */
static void the_command_waits_are_bounded(void)
{
    uint8_t dt = 0u;

    fresh_cart();
    f.cmd_busy_reads = FOREVER;
    assert(sc64_poll(&dt) == 0u && f.masked == 0);
    f.cmd_busy_reads = 0u;

    fill(msg, 6u, 0x4D363442u);
    host_sends(1u, msg, 24u);
    f.usb_read_stuck = 1;
    assert(sc64_poll(&dt) == 0u && f.usb_reads == 1 && f.masked == 0);
}

/* ---- runner ------------------------------------------------------------------------ */

static const char *s_case;

static void on_alarm(int sig)
{
    static const char a[] = "sc64: ";
    static const char b[] = " did not return: a wait in sc64.c is unbounded\n";
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
    RUN(a_request_and_its_reply_round_trip);
    RUN(a_dropped_usb_read_is_not_reported_as_a_packet);
    RUN(a_failed_restore_of_write_enable_is_retried);
    RUN(a_write_enable_that_never_reached_the_cart_changes_nothing);
    RUN(a_write_enable_that_may_have_run_is_still_put_back);
    RUN(a_pending_restore_is_retried_by_a_write_that_stops_early);
    RUN(a_bus_that_stays_busy_during_a_copy_does_not_hang);
    RUN(a_copy_does_not_touch_the_cart_while_a_dma_holds_the_bus);
    RUN(a_read_while_the_bus_stays_busy_fails);
    RUN(the_command_waits_are_bounded);
    printf("sc64 driver: %d cases passed\n", s_cases);
    return 0;
}
