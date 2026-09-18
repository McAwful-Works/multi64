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

/** USBCFG reads while waiting for a transfer to finish. Bounded: a stuck cart costs a failed call. */
#define ED_BUSY_SPINS 20000u

/* The data window, a word at a time. Bytes are placed at the window's end and the whole tail is
   moved by word, so a window offset that is not a word boundary costs nothing extra. */
static uint32_t s_window[ED_WINDOW / 4u];

/* L3 bytes taken off USB but not yet handed to the agent, oldest first: s_rxq[s_rxq_off] onwards,
   s_rxq_len of them. ed64_receive hands them over before reading USB again. Two things fill it:
   the part of a message that did not fit the space the caller offered, and everything ed64_send
   reads before it writes. If a message read ahead was lost, s_rxq_lost is set and the loss is
   reported after the first s_rxq_before_loss bytes, where it fell in the stream. */
#define ED_RXQ_CAP (4u * ED_WINDOW)
static uint8_t s_rxq[ED_RXQ_CAP];
static uint32_t s_rxq_off;
static uint32_t s_rxq_len;
static int s_rxq_lost;
static uint32_t s_rxq_before_loss;

/* The most of a message ed64_receive keeps back when it does not fit the space offered. One host
   message's worth: crates/ed64-l2 puts at most 512 L3 bytes in a DMA@ message, so a message that
   arrives while the agent's buffer is nearly full is kept. */
#define ED_SPILL_CAP ED_WINDOW

/* Messages ed64_send reads ahead in one call, at most. Bounds the loop if the PI stays busy. */
#define ED_READ_AHEAD_MAX 32u

static int usb_idle(void)
{
    uint32_t spins;
    uint32_t v;

    for (spins = 0u; spins < ED_BUSY_SPINS; spins++) {
        if (!pi_io_read(ED_REG_USBCFG, &v)) {
            return 0;
        }
        if (!(v & ED_USBSTAT_ACT)) {
            return 1;
        }
    }
    /* Leave the USB unit in its idle read mode, as libdragon does on a timeout. */
    (void)pi_io_write(ED_REG_USBCFG, ED_USBMODE_RDNOP);
    return 0;
}

int ed64_init(void)
{
    uint32_t v;

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
 * Pull `n` (1..512) bytes from USB into the window; returns where they start, or 0. `*started`
 * says whether the read transfer was asked for: until it is, nothing has left the cart.
 */
static const uint8_t *usb_pull_started(uint32_t n, int *started)
{
    uint32_t addr = ED_WINDOW - n;
    uint32_t first = addr & ~3u;

    if (!pi_io_write_stored(ED_REG_USBCFG, ED_USBMODE_RD | addr, started) || !usb_idle()) {
        return 0;
    }
    if (!pi_io_load_words(s_window, ED_REG_USBDAT + first, (ED_WINDOW - first) / 4u)) {
        return 0;
    }
    return (const uint8_t *)s_window + (addr - first);
}

static const uint8_t *usb_pull(uint32_t n)
{
    int started;
    return usb_pull_started(n, &started);
}

/* 1 when the USB unit is idle, powered, and the host has sent something not yet read. */
static int usb_waiting(void)
{
    uint32_t v;

    if (!usb_idle()) {
        return 0;
    }
    /* Powered, and the receive FIFO not empty. */
    return pi_io_read(ED_REG_USBCFG, &v) &&
           (v & (ED_USBSTAT_POWER | ED_USBSTAT_RXF)) == ED_USBSTAT_POWER;
}

/**
 * Read the waiting message: its first `cap` L3 bytes to `dst`, and up to `spill_cap` more to the
 * end of the queue. Returns what went to `dst`, 0 when nothing was taken or it was not L3, or
 * ED64_RECEIVE_LOST. Call only when usb_waiting().
 */
static uint32_t usb_message(uint8_t *dst, uint32_t cap, uint32_t spill_cap)
{
    const uint8_t *b;
    uint32_t size;
    uint32_t keep = 0u;
    uint32_t spill = 0u;
    uint32_t left;
    uint32_t got = 0u;
    uint32_t spilled = 0u;
    uint8_t *spill_to = s_rxq + s_rxq_off + s_rxq_len;
    int is_l3;
    int started;

    /* A PI that stayed busy before the header's read was even asked for took nothing: the message
       is still whole in the cart for the next call. Reporting that as a loss made the agent throw
       away a partial request it could still have completed (#223). */
    b = usb_pull_started(8u, &started);
    if (b == 0 && !started) {
        return 0u;
    }
    /* From here on a message is being consumed: any failure has lost part of the stream, and says
       so, so the agent throws away the partial frame it belonged to (#151). */
    if (b == 0 || b[0] != 'D' || b[1] != 'M' || b[2] != 'A' || b[3] != '@') {
        /* Out of step with the host. The agent's L3 layer resynchronises on the next frame. */
        return ED64_RECEIVE_LOST;
    }
    size = ((uint32_t)b[5] << 16) | ((uint32_t)b[6] << 8) | (uint32_t)b[7];
    is_l3 = b[4] == ED_DATATYPE_L3;
    if (is_l3) {
        if (size <= cap) {
            keep = size;
        } else if (size - cap <= spill_cap) {
            /* Too big for the space offered, but not for it and the pending buffer together. */
            keep = cap;
            spill = size - cap;
        }
    }

    /* The payload is padded to 2 bytes on the wire; drain all of it even when keeping none. */
    left = (size + 1u) & ~1u;
    while (left > 0u) {
        uint32_t n = left > ED_WINDOW ? ED_WINDOW : left;
        uint32_t i;

        b = usb_pull(n);
        if (b == 0) {
            return ED64_RECEIVE_LOST;
        }
        for (i = 0u; i < n && got < keep; i++) {
            dst[got++] = b[i];
        }
        for (; i < n && spilled < spill; i++) {
            spill_to[spilled++] = b[i];
        }
        left -= n;
    }

    b = usb_pull(4u);
    if (b == 0 || b[0] != 'C' || b[1] != 'M' || b[2] != 'P' || b[3] != 'H') {
        return ED64_RECEIVE_LOST;
    }
    if (is_l3 && keep + spill != size) {
        /* Drained, but too big even with what could be kept back: its L3 bytes are gone. */
        return ED64_RECEIVE_LOST;
    }
    /* Only a message that arrived whole leaves its tail queued; on any loss above, nothing is. */
    s_rxq_len += spilled;
    return got;
}

uint32_t ed64_receive(uint8_t *dst, uint32_t cap)
{
    uint32_t n;
    uint32_t i;

    if (s_rxq_lost && s_rxq_before_loss == 0u) {
        s_rxq_lost = 0;
        return ED64_RECEIVE_LOST;
    }
    if (s_rxq_len > 0u) {
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
        if (s_rxq_len == 0u) {
            s_rxq_off = 0u;
        }
        return n;
    }
    if (!usb_waiting()) {
        return 0u;
    }
    /* The queue is empty, so a message too big for `cap` can keep its tail there. */
    return usb_message(dst, cap, ED_SPILL_CAP);
}

/**
 * Read every message the host has sent into the queue. Returns 1 when nothing is left waiting, 0
 * when something is and there is no room for it, or the PI stayed busy.
 *
 * An X7 does not finish a write while host bytes wait unread (l3-over-everdrive-x7.md 4.5 item 6):
 * test ROM 1.11 lost 6 writes to it, and 1.12, reading everything waiting before each write, none.
 */
static int read_ahead(void)
{
    uint32_t i;

    for (i = 0u; i < ED_READ_AHEAD_MAX; i++) {
        uint32_t v;
        uint32_t n;

        if (!usb_idle() || !pi_io_read(ED_REG_USBCFG, &v)) {
            return 0;
        }
        if ((v & (ED_USBSTAT_POWER | ED_USBSTAT_RXF)) != ED_USBSTAT_POWER) {
            return 1;
        }
        if (s_rxq_off > 0u) {
            for (n = 0u; n < s_rxq_len; n++) {
                s_rxq[n] = s_rxq[s_rxq_off + n];
            }
            s_rxq_off = 0u;
        }
        /* Room for a whole host message, or it stays waiting. */
        if (ED_RXQ_CAP - s_rxq_len < ED_WINDOW) {
            return 0;
        }
        n = usb_message(s_rxq + s_rxq_len, ED_RXQ_CAP - s_rxq_len, 0u);
        if (n != ED64_RECEIVE_LOST) {
            s_rxq_len += n;
        } else if (!s_rxq_lost) {
            s_rxq_lost = 1;
            s_rxq_before_loss = s_rxq_len;
        } else {
            /* The queue marks one loss. A second folds into the first, taking what lay between
               them: a loss already costs the agent its partial frame, and it resyncs after. */
            s_rxq_len = s_rxq_before_loss;
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
       keeps what the cart sends from depending on uninitialised memory. */
    return 0u;
}

int ed64_send(const uint8_t *data, uint32_t len)
{
    /* Header, payload and trailer, the whole message padded to 2 bytes (#134). */
    uint32_t total = (8u + len + 4u + 1u) & ~1u;
    uint32_t p = 0u;
    uint8_t *bytes = (uint8_t *)s_window;

    if (len == 0u || len > ED_MAX_MESSAGE) {
        return 0;
    }
    /* A write started with host bytes unread gives up part-way, and the host gets a message cut off
       after its first block. Read them first; if they cannot all be held, send nothing, since a
       reply the host never gets is a timeout, but a malformed one is a resync. */
    if (!read_ahead()) {
        return 0;
    }

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

        if (!pi_io_write(ED_REG_USBCFG, ED_USBMODE_WRNOP)) {
            return 0;
        }
        if (!pi_io_store_words(s_window, ED_REG_USBDAT + first, (ED_WINDOW - first) / 4u)) {
            return 0;
        }
        if (!pi_io_write(ED_REG_USBCFG, ED_USBMODE_WR | addr) || !usb_idle()) {
            return 0;
        }
        p += n;
    }
    return 1;
}
