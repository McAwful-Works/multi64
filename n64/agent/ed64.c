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

/** Pull `n` (1..512) bytes from USB into the window; returns where they start, or 0. */
static const uint8_t *usb_pull(uint32_t n)
{
    uint32_t addr = ED_WINDOW - n;
    uint32_t first = addr & ~3u;

    if (!pi_io_write(ED_REG_USBCFG, ED_USBMODE_RD | addr) || !usb_idle()) {
        return 0;
    }
    if (!pi_io_load_words(s_window, ED_REG_USBDAT + first, (ED_WINDOW - first) / 4u)) {
        return 0;
    }
    return (const uint8_t *)s_window + (addr - first);
}

uint32_t ed64_receive(uint8_t *dst, uint32_t cap)
{
    const uint8_t *b;
    uint32_t v;
    uint32_t size;
    uint32_t keep;
    uint32_t left;
    uint32_t got = 0u;

    if (!usb_idle()) {
        return 0u;
    }
    /* Powered, and the receive FIFO not empty: a message is waiting. */
    if (!pi_io_read(ED_REG_USBCFG, &v) || (v & (ED_USBSTAT_POWER | ED_USBSTAT_RXF)) != ED_USBSTAT_POWER) {
        return 0u;
    }

    b = usb_pull(8u);
    if (b == 0 || b[0] != 'D' || b[1] != 'M' || b[2] != 'A' || b[3] != '@') {
        /* Out of step with the host. The agent's L3 layer resynchronises on the next frame. */
        return 0u;
    }
    size = ((uint32_t)b[5] << 16) | ((uint32_t)b[6] << 8) | (uint32_t)b[7];
    keep = (b[4] == ED_DATATYPE_L3 && size <= cap) ? size : 0u;

    /* The payload is padded to 2 bytes on the wire; drain all of it even when keeping none. */
    left = (size + 1u) & ~1u;
    while (left > 0u) {
        uint32_t n = left > ED_WINDOW ? ED_WINDOW : left;
        uint32_t i;

        b = usb_pull(n);
        if (b == 0) {
            return 0u;
        }
        for (i = 0u; i < n && got < keep; i++) {
            dst[got++] = b[i];
        }
        left -= n;
    }

    b = usb_pull(4u);
    if (b == 0 || b[0] != 'C' || b[1] != 'M' || b[2] != 'P' || b[3] != 'H') {
        return 0u;
    }
    return got;
}

/** Byte `p` of the message DMA@ | type and size | payload | pad to 2 | CMPH. */
static uint8_t message_byte(const uint8_t *data, uint32_t len, uint32_t p)
{
    uint32_t padded = (len + 1u) & ~1u;
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
    if (p < padded) {
        return 0u;
    }
    return (uint8_t)"CMPH"[p - padded];
}

int ed64_send(const uint8_t *data, uint32_t len)
{
    uint32_t total = 8u + ((len + 1u) & ~1u) + 4u;
    uint32_t p = 0u;
    uint8_t *bytes = (uint8_t *)s_window;

    if (len == 0u || len > ED_MAX_MESSAGE) {
        return 0;
    }
    if (!usb_idle()) {
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
