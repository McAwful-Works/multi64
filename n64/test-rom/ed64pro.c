#include "ed64pro.h"

#include <dma.h>
#include <n64sys.h>

/* ---- registers (l3-over-everdrive-pro.md 6.1) ----------------------------- */

#define EDIO_BASE 0x1F800000u
#define REG_FIFODATA (EDIO_BASE + 0x00u)
#define REG_FIFOSTAT (EDIO_BASE + 0x04u)
#define REG_SYSSTAT (EDIO_BASE + 0x08u)
#define REG_EDID (EDIO_BASE + 0x14u)

#define FIFOSTAT_COUNT 0xFFFFu

#define SYSSTAT_BUSY 0x01u
#define SYSSTAT_STROBE 0x08u
#define SYSSTAT_CONST_MASK 0xF0u
#define SYSSTAT_CONST 0xA0u

#define EDID_FAMILY_MASK 0xFFFF0000u
#define EDID_FAMILY 0xED640000u

/*
 * IDs other EverDrives report at the same address. libdragon's usb.c accepts 0xED640013 (X7, X5)
 * and 0xED640008 (3.0) and rejects 0xED640007 (2.5); 0xED640014 is the X5's cart ID. None of these
 * is a PRO, and writing the FIFO on one of them would write that cart's own registers instead.
 */
#define EDID_ED25 0xED640007u
#define EDID_ED3 0xED640008u
#define EDID_EDX 0xED640013u
#define EDID_X5 0xED640014u

/* ---- edlink commands (ed64-pro-usb-host.md 3, 5, 6) ------------------------ */

#define CMD_STATUS 0x10u
#define CMD_EPO 0x81u
#define EPO_SCMD_XFER 0x10u
#define EPO_LINK 0x10u
/* Console-side endpoint numbering: edlink's PC code numbers USB differently (spec 10). */
#define EPO_USB 0x18u

#define STATUS_KEY 0x5Au
#define PROTOCOL_ID 0x07u
#define DEVICE_ID_ED64PRO 0x27u

/* The console library sends at most this much per transfer command (SIZE_ACK_BLOCK). */
#define XFER_BLOCK 1024u

/* Bounded waits: a cart that stops answering must cost a failed call, never a hung console. */
#define STATUS_WAIT_MS 100u
#define MCU_WAIT_MS 500u

/* Longest a drain of stale host bytes may run at detection. */
#define DRAIN_LIMIT 8192u

static uint32_t now_ms(void)
{
    return (uint32_t)get_ticks_ms();
}

static void fifo_put(const uint8_t *src, uint32_t len)
{
    uint32_t i;
    for (i = 0u; i < len; i++) {
        io_write(REG_FIFODATA, (uint32_t)src[i]);
    }
}

uint32_t ed64pro_rx_available(void)
{
    return io_read(REG_FIFOSTAT) & FIFOSTAT_COUNT;
}

uint32_t ed64pro_rx_read(uint8_t *dst, uint32_t len)
{
    uint32_t n = ed64pro_rx_available();
    uint32_t i;

    if (n > len) {
        n = len;
    }
    for (i = 0u; i < n; i++) {
        dst[i] = (uint8_t)io_read(REG_FIFODATA);
    }
    return n;
}

/** Read exactly `len` bytes, or give up after `wait_ms`. Returns 1 on success. */
static int fifo_get_exact(uint8_t *dst, uint32_t len, uint32_t wait_ms)
{
    uint32_t got = 0u;
    uint32_t start = now_ms();

    while (got < len) {
        uint32_t n = ed64pro_rx_read(dst + got, len - got);
        got += n;
        if (n == 0u && now_ms() - start > wait_ms) {
            return 0;
        }
    }
    return 1;
}

static int wait_mcu(void)
{
    uint32_t start = now_ms();

    while (io_read(REG_SYSSTAT) & SYSSTAT_BUSY) {
        if (now_ms() - start > MCU_WAIT_MS) {
            return 0;
        }
    }
    return 1;
}

/** SYSSTAT's constant high nibble, and its strobe bit inverting between two reads. */
static int sysstat_signature(void)
{
    int attempt;

    for (attempt = 0; attempt < 4; attempt++) {
        uint32_t v1 = io_read(REG_SYSSTAT);
        uint32_t v2 = io_read(REG_SYSSTAT);

        if ((v1 & SYSSTAT_CONST_MASK) != SYSSTAT_CONST || (v2 & SYSSTAT_CONST_MASK) != SYSSTAT_CONST) {
            return 0;
        }
        if ((v1 ^ v2) & SYSSTAT_STROBE) {
            return 1;
        }
    }
    return 0;
}

int ed64pro_detect(void)
{
    uint32_t id = io_read(REG_EDID);
    uint32_t drained = 0u;
    uint8_t cmd[4];
    uint8_t status[4];

    /* Reads only, until the cart has looked like a PRO twice over. */
    if ((id & EDID_FAMILY_MASK) != EDID_FAMILY) {
        return 0;
    }
    if (id == EDID_ED25 || id == EDID_ED3 || id == EDID_EDX || id == EDID_X5) {
        return 0;
    }
    if (!sysstat_signature()) {
        return 0;
    }

    /* Anything already waiting would be read as the status reply. Nothing should be: the host has
       no reason to send before this ROM answers. */
    while (drained < DRAIN_LIMIT) {
        uint8_t scratch[64];
        uint32_t n = ed64pro_rx_read(scratch, sizeof(scratch));
        if (n == 0u) {
            break;
        }
        drained += n;
    }

    cmd[0] = 0x2Bu; /* '+' */
    cmd[1] = 0xD4u; /* '+' ^ 0xFF */
    cmd[2] = CMD_STATUS;
    cmd[3] = (uint8_t)(CMD_STATUS ^ 0xFFu);
    fifo_put(cmd, sizeof(cmd));

    if (!fifo_get_exact(status, sizeof(status), STATUS_WAIT_MS)) {
        return 0;
    }
    return status[0] == STATUS_KEY && status[1] == PROTOCOL_ID && status[2] == DEVICE_ID_ED64PRO;
}

static void put_be32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)(v >> 16);
    p[2] = (uint8_t)(v >> 8);
    p[3] = (uint8_t)v;
}

int ed64pro_tx_write(const uint8_t *src, uint32_t len)
{
    while (len > 0u) {
        uint32_t block = len < XFER_BLOCK ? len : XFER_BLOCK;
        /* Frame (5) + transfer header (16) + start byte (1): ed64-pro-usb-host.md 6. */
        uint8_t head[22];

        head[0] = 0x2Bu;
        head[1] = 0xD4u;
        head[2] = CMD_EPO;
        head[3] = (uint8_t)(CMD_EPO ^ 0xFFu);
        head[4] = EPO_SCMD_XFER;
        put_be32(&head[5], 0u);     /* source address */
        put_be32(&head[9], 0u);     /* destination address */
        put_be32(&head[13], block); /* length */
        head[17] = EPO_LINK;        /* from the FIFO... */
        head[18] = EPO_USB;         /* ...to the USB port */
        head[19] = 0u;              /* reserved */
        head[20] = 0u;
        head[21] = 0u; /* start the transfer */

        fifo_put(head, sizeof(head));
        fifo_put(src, block);

        /* The MCU forwards the block while it reports busy. It sends nothing back through the
           FIFO, so host bytes arriving meanwhile stay queued for ed64pro_rx_read(). */
        if (!wait_mcu()) {
            return 0;
        }
        src += block;
        len -= block;
    }
    return 1;
}
