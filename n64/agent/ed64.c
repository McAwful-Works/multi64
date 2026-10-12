#include "ed64.h"

#include "pi_io.h"

/* ---- registers (libdragon usb.c) ------------------------------------------- */

#define ED_REG_USBCFG 0x1F800004u
#define ED_REG_VERSION 0x1F800014u
#define ED_REG_USBDAT 0x1F800400u
#define ED_REG_SYSCFG 0x1F808000u
#define ED_REG_KEY 0x1F808004u

#define ED_USBMODE_RDNOP 0xC400u
#define ED_USBMODE_RD 0xC600u
#define ED_USBMODE_WRNOP 0xC000u
#define ED_USBMODE_WR 0xC200u

#define ED_USBSTAT_ACT 0x0200u
#define ED_USBSTAT_RXF 0x0400u
#define ED_USBSTAT_POWER 0x1000u

#define ED_REGKEY 0xAA55u
#define ED3_VERSION 0xED640008u
#define EDX_VERSION 0xED640013u

/* ---- framing (l3-over-everdrive-x7.md 4.2) ------------------------------------ */

#define ED_WINDOW 512u
#define ED_DATATYPE_L3 0x01u
#define ED_MAX_MESSAGE 0x00FFFFFFu

/* ---- time ---------------------------------------------------------------------
 *
 * Every wait here ends by the clock as well as by a count. A count of PI accesses does not bound a
 * wait in time: one pi_io access can itself wait PI_WAIT_SPINS status reads for the PI to go idle,
 * about 26 ms at the 263 ns one was measured to take (n64/bringup), and then succeed, so 20,000
 * of them could hold a frame for minutes while a game streamed from ROM.
 *
 * The clock is the CPU's Count register, which runs at half the CPU clock whatever the PI is doing:
 * 46,875 ticks a millisecond. The limit is looked at before each PI access, so a call runs past
 * its limit by the one pi_io call then under way, and after a status read that finds a transfer
 * overdue, by the write that abandons it. Either way that is at most two of pi_io.c's waits for the
 * PI to go idle, each bounded by PI_WAIT_SPINS: about 53 ms. pi_io.c's waits for its own stores to
 * land are not in that figure. It makes them with the PI just seen idle and interrupts masked,
 * where nothing but the store itself can be holding the bus.
 */
#define ED_TICKS_PER_MS 46875u

/** USBCFG reads while waiting for one transfer to finish, as before: about 72 ms on an idle PI. */
#define ED_BUSY_SPINS 20000u
/** And how long: a transfer still active after this is abandoned, however few reads that took. */
#define ED_BUSY_TICKS (75u * ED_TICKS_PER_MS)

/** One ed64_receive call. What it has not finished by then, the next call takes up. */
#define ED_RECEIVE_TICKS (100u * ED_TICKS_PER_MS)
/** ed64_receive calls in a row that deliver bytes: once they have used this, the next returns 0. */
#define ED_BURST_TICKS (30u * ED_TICKS_PER_MS)

/**
 * One ed64_send call: what it reads ahead, and the whole message. It is also the longest the cart
 * can go quiet part-way through a message it then finishes, and with the access under way when it
 * runs out, about 0.3 s, it stays under the 500 ms crates/ed64-l2 lets a message stall before
 * dropping the part it has (l3-over-everdrive-x7.md 4.4). The longest send an X7 has been seen to
 * make took 62.9 ms, under the heaviest PI load tried (n64/bringup, 2026-10-09); the replies in
 * that run were 9 blocks at most, and the largest the agent sends is 16.
 */
#define ED_SEND_TICKS (250u * ED_TICKS_PER_MS)

#ifdef ED64_HOST_TEST
/* tests/ed64_test.c supplies the clock, and moves it as its fake PI is used. */
uint32_t ed64_host_count(void);
#define ed_count() ed64_host_count()
#else
static uint32_t ed_count(void)
{
    uint32_t count;
    __asm__ __volatile__("mfc0 %0, $9" : "=r"(count));
    return count;
}
#endif

/* The call under way: when it began, how long it may run, and whether a PI access that fails is
   made again while it has time. */
static uint32_t s_call_at;
static uint32_t s_call_ticks;
static int s_retry;

static void call_begin(uint32_t ticks)
{
    s_call_at = ed_count();
    s_call_ticks = ticks;
    s_retry = 0;
}

static int out_of_time(void)
{
    return ed_count() - s_call_at >= s_call_ticks;
}

/* ---- state -------------------------------------------------------------------- */

/* The data window, a word at a time. Bytes are placed at the window's end and the whole tail is
   moved by word, so a window offset that is not a word boundary costs nothing extra. */
static uint32_t s_window[ED_WINDOW / 4u];

/* L3 bytes taken off USB but not yet handed to the agent, oldest first: s_rxq[s_rxq_off] onwards,
   s_rxq_len of them. Every message is put together here, at the queue's end, and joins the queue
   once its trailer has been checked; ed64_receive hands the queue over before it reads USB again.
   If a message ed64_send read ahead was lost, s_rxq_lost is set and the loss is reported after the
   first s_rxq_before_loss bytes, where it fell in the stream. */
#define ED_RXQ_CAP (4u * ED_WINDOW)
static uint8_t s_rxq[ED_RXQ_CAP];
static uint32_t s_rxq_off;
static uint32_t s_rxq_len;
static int s_rxq_lost;
static uint32_t s_rxq_before_loss;

/* Passes of ed64_send's read-ahead in one call, at most. */
#define ED_READ_AHEAD_MAX 32u

/*
 * Where the receive path is in the host's stream, kept from one call to the next. A call that the
 * PI or the clock stops takes nothing with it: the next carries on from the same byte.
 *
 * It did not use to. Any failure once a message's header had been read reported the message lost
 * and forgot the rest of it, which was then read as headers, 8 bytes a call: a 512-byte message
 * cost about 70 calls of ED64_RECEIVE_LOST, and the host's retry with it.
 *
 * s_rx_phase   What the next bytes of the stream are.
 * s_pull       Bytes of a read that has been asked for and not yet loaded, 0 when there is none.
 *              They are in the cart's window, or on their way to it, and stay there until they
 *              are loaded: loading takes nothing off USB, so it can be put off or done again.
 * s_msg_*      The message being put together at the queue's end.
 * s_owed       Bytes of a message that is not wanted (another datatype, or too big to hold) and
 *              not yet asked of the cart: what is left of its padded payload, and its trailer.
 *              They are taken off USB unread before the next header is looked for.
 * s_reading    A read has been asked for and the USB unit not seen idle since.
 * s_rx_cut     A read was abandoned with part of a message in it, and that has not been reported.
 */
#define ED_RX_HEADER 0u  /* between messages: a header is next */
#define ED_RX_ROOM 1u    /* a header has been read; its payload waits for room in the queue */
#define ED_RX_PAYLOAD 2u /* the payload is being read to the queue's end */
#define ED_RX_TRAILER 3u /* the trailer has been asked for */
static uint32_t s_rx_phase;
static uint32_t s_pull;
static uint32_t s_msg_size; /* its L3 bytes */
static uint32_t s_msg_left; /* bytes of its padded payload not yet asked of the cart */
static uint32_t s_msg_got;  /* L3 bytes of it at the queue's end so far */
static uint32_t s_owed;
static int s_reading;
static int s_rx_cut;

/* Count ticks the ed64_receive calls since the last one that returned 0 or a loss have used. */
static uint32_t s_rx_used;

/* ---- USB unit ------------------------------------------------------------------ */

#define ED_USB_BUSY 0  /* not known: the PI stayed busy, or the call is out of time */
#define ED_USB_IDLE 1  /* no transfer active; *status is the USBCFG word that said so */
#define ED_USB_STUCK 2 /* a transfer never finished, and was abandoned */

/**
 * Wait for the USB unit to finish a transfer: ED_BUSY_SPINS status reads or ED_BUSY_TICKS,
 * whichever is over first. A transfer still active then is abandoned. The wait also ends, with the
 * transfer left as it is for a later call to wait on, when the call runs out of time or a status
 * read fails; with s_retry set a failed read counts as a spin instead.
 */
static int usb_idle(uint32_t *status)
{
    uint32_t began = ed_count();
    uint32_t spins;
    int stored;

    for (spins = 0u; spins < ED_BUSY_SPINS; spins++) {
        if (out_of_time()) {
            return ED_USB_BUSY;
        }
        if (!pi_io_read(ED_REG_USBCFG, status)) {
            if (!s_retry) {
                return ED_USB_BUSY;
            }
            continue;
        }
        if (!(*status & ED_USBSTAT_ACT)) {
            s_reading = 0;
            return ED_USB_IDLE;
        }
        if (ed_count() - began >= ED_BUSY_TICKS) {
            break;
        }
    }
    /* Leave the USB unit in its idle read mode, as libdragon does on a timeout. If the PI stays
       busy before that is stored, the transfer is still active, and a later call's wait ends here
       again. Stored and then not seen to land, it may have taken effect, and counts as having:
       to go on treating an abandoned read as live would be to load whatever its window held. */
    if (!pi_io_write_stored(ED_REG_USBCFG, ED_USBMODE_RDNOP, &stored) && !stored) {
        return ED_USB_BUSY;
    }
    if (s_reading) {
        /* A read the cart did not finish: the host sent less than was asked for. How much of it
           came off USB is not known, so neither is where the next header is. The next call looks
           for one where the stream now stands. If the read was part of a message, that message is
           gone; if it was of bytes owed, their message was done with already. */
        s_rx_cut = s_pull != 0u;
        s_reading = 0;
        s_pull = 0u;
        s_owed = 0u;
        s_rx_phase = ED_RX_HEADER;
    }
    return ED_USB_STUCK;
}

int ed64_init(void)
{
    uint32_t v;

    /* Nothing an earlier use left behind outlives this. */
    s_rxq_off = 0u;
    s_rxq_len = 0u;
    s_rxq_lost = 0;
    s_rx_phase = ED_RX_HEADER;
    s_pull = 0u;
    s_owed = 0u;
    s_reading = 0;
    s_rx_cut = 0;
    s_rx_used = 0u;

    if (!pi_io_write(ED_REG_KEY, ED_REGKEY)) {
        return 0;
    }
    if (!pi_io_read(ED_REG_VERSION, &v) || (v != EDX_VERSION && v != ED3_VERSION)) {
        return 0;
    }
    if (!pi_io_write(ED_REG_SYSCFG, 0u) || !pi_io_write(ED_REG_USBCFG, ED_USBMODE_RDNOP)) {
        return 0;
    }
    /* An X5 passes the version check but has no USB: its USB unit reads as powered off. */
    return pi_io_read(ED_REG_USBCFG, &v) && (v & ED_USBSTAT_POWER);
}

/**
 * Ask for a read of `n` (1..512) bytes from USB into the window's end. Returns 0 when the PI
 * stayed busy before the request was stored: nothing has left the cart. A store that was made and
 * then failed to land may have started the read, so it counts as asked.
 */
static int usb_ask(uint32_t n)
{
    int stored;

    if (out_of_time()) {
        return 0;
    }
    if (!pi_io_write_stored(ED_REG_USBCFG, ED_USBMODE_RD | (ED_WINDOW - n), &stored) && !stored) {
        return 0;
    }
    s_reading = 1;
    return 1;
}

/**
 * Wait for the read of s_pull bytes that has been asked for, and load them. Returns where they
 * start, with `*n` their count and s_pull cleared, or 0 with the read still to be taken: by a later
 * call, unless the cart never finished it and usb_idle() abandoned it.
 */
static const uint8_t *usb_take(uint32_t *n)
{
    uint32_t addr = ED_WINDOW - s_pull;
    uint32_t first = addr & ~3u;
    uint32_t v;

    if (usb_idle(&v) != ED_USB_IDLE || out_of_time() ||
        !pi_io_load_words(s_window, ED_REG_USBDAT + first, (ED_WINDOW - first) / 4u)) {
        return 0;
    }
    *n = s_pull;
    s_pull = 0u;
    return (const uint8_t *)s_window + (addr - first);
}

/* The queue starts at the front of its buffer again. Only between messages: one being put
   together sits at the queue's end, and must not have that moved under it. */
static void rxq_to_front(void)
{
    uint32_t i;

    if (s_rxq_off > 0u) {
        for (i = 0u; i < s_rxq_len; i++) {
            s_rxq[i] = s_rxq[s_rxq_off + i];
        }
        s_rxq_off = 0u;
    }
}

#define ED_RX_BUSY 0  /* stopped by the PI or the clock, with its place kept: come back */
#define ED_RX_CLEAR 1 /* between messages, and the host has sent nothing unread */
#define ED_RX_DONE 2  /* a message joined the queue, or one not for the agent was set aside */
#define ED_RX_LOST 3  /* bytes of the stream are gone: a message, or the place in it */
#define ED_RX_FULL 4  /* a message waits whose payload the queue has no room for yet */

/* A read could not be taken: because its time has not come, or because it never will. */
static int rx_stopped(void)
{
    if (s_rx_cut) {
        s_rx_cut = 0;
        return ED_RX_LOST;
    }
    return ED_RX_BUSY;
}

/**
 * Carry the receive path on from where it stands, as far as one message's end: drain what is
 * owed, then read a header, a payload to the queue's end, and a trailer. Every step is one that a
 * later call can take up again, so a busy PI or the clock only puts the rest off. What loses a
 * message is the stream itself: a header that is not DMA@, a trailer that is not CMPH, a message
 * too big to hold, or a read the cart never finishes.
 */
static int usb_receive(void)
{
    const uint8_t *b;
    uint32_t v = 0u;
    uint32_t n;
    uint32_t i;

    /* With no read to take, wait out whatever is still active, and take what is owed off USB a
       window at a time. Those bytes are not wanted, so their reads are asked for and never loaded. */
    while (s_pull == 0u) {
        n = s_owed > ED_WINDOW ? ED_WINDOW : s_owed;
        if (usb_idle(&v) != ED_USB_IDLE) {
            return rx_stopped();
        }
        if (n == 0u) {
            break;
        }
        if (!usb_ask(n)) {
            return ED_RX_BUSY;
        }
        s_owed -= n;
    }

    if (s_rx_phase == ED_RX_HEADER) {
        uint32_t size;

        if (s_pull == 0u) {
            /* Powered, and the receive FIFO not empty. */
            if ((v & (ED_USBSTAT_POWER | ED_USBSTAT_RXF)) != ED_USBSTAT_POWER) {
                return ED_RX_CLEAR;
            }
            /* A PI that stays busy before the header's read is even asked for takes nothing: the
               message is still whole in the cart. Reporting that as a loss made the agent throw
               away a partial request it could still have completed (#223). */
            if (!usb_ask(8u)) {
                return ED_RX_BUSY;
            }
            s_pull = 8u;
        }
        b = usb_take(&n);
        if (b == 0) {
            return rx_stopped();
        }
        if (b[0] != 'D' || b[1] != 'M' || b[2] != 'A' || b[3] != '@') {
            /* Out of step with the host. The agent's L3 layer resynchronizes on the next frame. */
            return ED_RX_LOST;
        }
        size = ((uint32_t)b[5] << 16) | ((uint32_t)b[6] << 8) | (uint32_t)b[7];
        /* The payload is padded to 2 bytes on the wire, and the trailer follows the padding. */
        s_msg_left = (size + 1u) & ~1u;
        if (b[4] != ED_DATATYPE_L3) {
            /* Not for the agent: drained unread, trailer and all. */
            s_owed = s_msg_left + 4u;
            return ED_RX_DONE;
        }
        if (size > ED_RXQ_CAP) {
            /* More than the queue could ever hold. crates/ed64-l2 sends 512 bytes a message. */
            s_owed = s_msg_left + 4u;
            return ED_RX_LOST;
        }
        s_msg_size = size;
        s_msg_got = 0u;
        s_rx_phase = ED_RX_ROOM;
    }

    if (s_rx_phase == ED_RX_ROOM) {
        rxq_to_front();
        if (s_msg_size > ED_RXQ_CAP - s_rxq_len) {
            return ED_RX_FULL;
        }
        s_rx_phase = ED_RX_PAYLOAD;
    }

    if (s_rx_phase == ED_RX_PAYLOAD) {
        /* Where the queue ends stays put while it is handed over: what s_rxq_off gains,
           s_rxq_len loses. */
        uint8_t *to = s_rxq + s_rxq_off + s_rxq_len;

        while (s_pull != 0u || s_msg_left > 0u) {
            if (s_pull == 0u) {
                n = s_msg_left > ED_WINDOW ? ED_WINDOW : s_msg_left;
                if (!usb_ask(n)) {
                    return ED_RX_BUSY;
                }
                s_pull = n;
                s_msg_left -= n;
            }
            b = usb_take(&n);
            if (b == 0) {
                return rx_stopped();
            }
            /* All of it but the padding byte, which is the last of an odd payload. */
            for (i = 0u; i < n && s_msg_got < s_msg_size; i++) {
                to[s_msg_got++] = b[i];
            }
        }
        if (!usb_ask(4u)) {
            return ED_RX_BUSY;
        }
        s_pull = 4u;
        s_rx_phase = ED_RX_TRAILER;
    }

    b = usb_take(&n);
    if (b == 0) {
        return rx_stopped();
    }
    s_rx_phase = ED_RX_HEADER;
    if (b[0] != 'C' || b[1] != 'M' || b[2] != 'P' || b[3] != 'H') {
        /* Its bytes never joined the queue, so none of them is delivered. */
        return ED_RX_LOST;
    }
    s_rxq_len += s_msg_got;
    return ED_RX_DONE;
}

/** Hand over what is queued: a loss where it fell, else as many bytes as `cap` allows. */
static uint32_t rxq_take(uint8_t *dst, uint32_t cap)
{
    uint32_t n;
    uint32_t i;

    if (s_rxq_lost && s_rxq_before_loss == 0u) {
        s_rxq_lost = 0;
        return ED64_RECEIVE_LOST;
    }
    /* What was already read comes before anything still waiting on USB, and a loss read ahead
       stops the hand-over where it fell. */
    n = s_rxq_len < cap ? s_rxq_len : cap;
    if (s_rxq_lost && n > s_rxq_before_loss) {
        n = s_rxq_before_loss;
    }
    for (i = 0u; i < n; i++) {
        dst[i] = s_rxq[s_rxq_off + i];
    }
    s_rxq_off += n;
    s_rxq_len -= n;
    if (s_rxq_lost) {
        s_rxq_before_loss -= n;
    }
    return n;
}

uint32_t ed64_receive(uint8_t *dst, uint32_t cap)
{
    uint32_t n = rxq_take(dst, cap);

    /* USB is read only once the queue is empty: with no space offered for what is queued, nothing
       is taken from it and nothing behind it is looked at. */
    if (n == 0u && s_rxq_len == 0u) {
        /* A caller reads until it has a whole frame, or gets 0 or a loss, as agent.c does once a
           tick, so each call that delivers bytes may be followed by another. This is what keeps a
           run of them to ED_BURST_TICKS and one call more: nothing is begun once they have used
           that. What waits stays where it is, in the cart or part-read here, for the next run. */
        if (s_rx_used >= ED_BURST_TICKS) {
            s_rx_used = 0u;
            return 0u;
        }
        call_begin(ED_RECEIVE_TICKS);
        switch (usb_receive()) {
        case ED_RX_DONE:
            n = rxq_take(dst, cap);
            break;
        case ED_RX_LOST:
            n = ED64_RECEIVE_LOST;
            break;
        default:
            break;
        }
        s_rx_used += ed_count() - s_call_at;
    }
    if (n == 0u || n == ED64_RECEIVE_LOST) {
        s_rx_used = 0u;
    }
    return n;
}

/**
 * Read every message the host has sent into the queue. Returns 1 when nothing is left waiting, 0
 * when something is and there is no room for it, the PI stayed busy, or the call ran out of time.
 *
 * An X7 does not finish a write while host bytes wait unread (l3-over-everdrive-x7.md 4.5 item 6):
 * test ROM 1.11 lost 6 writes to it, and 1.12, reading everything waiting before each write, none.
 */
static int read_ahead(void)
{
    uint32_t i;

    for (i = 0u; i < ED_READ_AHEAD_MAX; i++) {
        switch (usb_receive()) {
        case ED_RX_CLEAR:
            return 1;
        case ED_RX_DONE:
            break;
        case ED_RX_LOST:
            if (!s_rxq_lost) {
                s_rxq_lost = 1;
                s_rxq_before_loss = s_rxq_len;
            } else {
                /* The queue marks one loss. A second folds into the first, taking what lay between
                   them: a loss already costs the agent its partial frame, and it resyncs after. */
                s_rxq_len = s_rxq_before_loss;
            }
            break;
        default:
            return 0;
        }
    }
    return 0;
}

/** Byte `p` of the message DMA@ | type and size | payload | CMPH | pad to 2.
 *
 * The trailer goes straight after the *unpadded* payload and the whole message is padded after it
 * (#134), which is what libdragon's usb_everdrive_write sends and what the host reads. The other
 * direction pads the payload instead and puts the trailer after the padding; ed64_receive drains
 * that layout. The two differ only for an odd-length payload.
 */
static uint8_t message_byte(const uint8_t *data, uint32_t len, uint32_t p)
{
    uint32_t head = (ED_DATATYPE_L3 << 24) | len;

    if (p < 4u) {
        return (uint8_t)"DMA@"[p];
    }
    if (p < 8u) {
        return (uint8_t)(head >> (8u * (7u - p)));
    }
    p -= 8u;
    if (p < len) {
        return data[p];
    }
    if (p - len < 4u) {
        return (uint8_t)"CMPH"[p - len];
    }
    /* The alignment byte. libdragon leaves whatever its buffer held here; a zero is as valid and
       keeps what the cart sends from depending on uninitialized memory. */
    return 0u;
}

/**
 * Start a write transfer of the window's bytes from `addr` (in the word at `first`) to its end.
 * Returns 1 once the transfer has been asked for. A failed PI access before that has sent nothing,
 * so with s_retry set the whole block is tried again while the call has time.
 */
static int start_block(uint32_t first, uint32_t addr)
{
    for (;;) {
        int stored = 0;

        /* A store that was made and then failed to land may have started the transfer: going round
           again could send the block twice, so it counts as started. */
        if (!out_of_time() && pi_io_write(ED_REG_USBCFG, ED_USBMODE_WRNOP) && !out_of_time() &&
            pi_io_store_words(s_window, ED_REG_USBDAT + first, (ED_WINDOW - first) / 4u) &&
            !out_of_time() &&
            (pi_io_write_stored(ED_REG_USBCFG, ED_USBMODE_WR | addr, &stored) || stored)) {
            return 1;
        }
        if (!s_retry || out_of_time()) {
            return 0;
        }
    }
}

int ed64_send(const uint8_t *data, uint32_t len)
{
    /* Header, payload and trailer, the whole message padded to 2 bytes (#134). */
    uint32_t total = (8u + len + 4u + 1u) & ~1u;
    uint32_t p = 0u;
    uint8_t *bytes = (uint8_t *)s_window;
    uint32_t v;

    if (len == 0u || len > ED_MAX_MESSAGE) {
        return 0;
    }
    call_begin(ED_SEND_TICKS);
    /* A reply means the agent has a whole frame and its run of receives is over: the next run has
       its own time. */
    s_rx_used = 0u;
    /* A write started with host bytes unread gives up part-way, and the host gets a message cut off
       after its first block. Read them first; if they cannot all be held, send nothing, since a
       reply the host never gets is a timeout, but a malformed one is a resync. */
    if (!read_ahead()) {
        return 0;
    }
    /* Until the first block is on the wire a failed PI access is not made again: a busy PI before
       then drops the message whole. */
    s_retry = 0;

    /* The message length is even, so every window-sized block is too. */
    while (p < total) {
        uint32_t n = total - p;
        uint32_t addr;
        uint32_t first;
        uint32_t i;

        if (n > ED_WINDOW) {
            n = ED_WINDOW;
        }
        addr = ED_WINDOW - n;
        first = addr & ~3u;
        for (i = 0u; i < addr - first; i++) {
            bytes[i] = 0u;
        }
        for (i = 0u; i < n; i++) {
            bytes[(addr - first) + i] = message_byte(data, len, p + i);
        }

        if (!start_block(first, addr)) {
            return 0;
        }
        /*
         * The header is on its way. From here the rest must follow it, or the host is left a
         * message cut off part-way, which an X7 under load did: a 4122-byte reply stopped after 4
         * of its 9 blocks (n64/bringup, 2026-10-09). pi_io fails an access when a DMA the game
         * started between its idle check and its interrupt mask is under way, which costs no time,
         * or when the PI stays busy for PI_WAIT_SPINS. So a busy PI is now waited out, for as long
         * as the call has time, instead of giving up.
         */
        s_retry = 1;
        /* A transfer that never finishes (ACT stays set) is not tried again: the cart has given up
           the write, and sending the block again could put it on the wire twice. */
        if (usb_idle(&v) != ED_USB_IDLE) {
            return 0;
        }
        p += n;
    }
    return 1;
}
