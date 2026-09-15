#include "ed64pro.h"

#include "pi_io.h"

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
/* X-series and older IDs at the same address; none is a PRO (spec 6.2). */
#define EDID_ED25 0xED640007u
#define EDID_ED3 0xED640008u
#define EDID_EDX 0xED640013u
#define EDID_X5 0xED640014u

/* ---- edlink commands (ed64-pro-usb-host.md 3, 5, 6) ------------------------ */

#define CMD_STATUS 0x10u
#define CMD_EPO 0x81u
#define EPO_SCMD_XFER 0x10u
#define EPO_LINK 0x10u
#define EPO_USB 0x18u

#define STATUS_KEY 0x5Au
#define PROTOCOL_ID 0x07u
#define DEVICE_ID_ED64PRO 0x27u

#define XFER_BLOCK 1024u

/* ---- bounds: spins, not time; the agent has no clock ----------------------- */

/** FIFOSTAT reads while waiting for the status reply at init. */
#define STATUS_SPINS 20000u
/** SYSSTAT reads while the MCU forwards one block to USB. */
#define MCU_SPINS 100000u
/** Stale bytes discarded at init before giving up. */
#define DRAIN_LIMIT 8192u

static uint8_t s_scratch[64];

static int sysstat_signature(void)
{
    int attempt;

    /* The strobe's timing is unverified, so allow a few pairs; a wrong constant fails at once. */
    for (attempt = 0; attempt < 4; attempt++) {
        uint32_t v1;
        uint32_t v2;

        if (!pi_io_read(REG_SYSSTAT, &v1) || !pi_io_read(REG_SYSSTAT, &v2)) {
            return 0;
        }
        if ((v1 & SYSSTAT_CONST_MASK) != SYSSTAT_CONST || (v2 & SYSSTAT_CONST_MASK) != SYSSTAT_CONST) {
            return 0;
        }
        if ((v1 ^ v2) & SYSSTAT_STROBE) {
            return 1;
        }
    }
    return 0;
}

int ed64pro_init(void)
{
    uint32_t id;
    uint32_t drained = 0u;
    uint32_t got = 0u;
    uint32_t spins;
    uint8_t cmd[4];
    uint8_t status[4];

    /* Reads only, until the cart has looked like a PRO twice over. */
    if (!pi_io_read(REG_EDID, &id) || (id & EDID_FAMILY_MASK) != EDID_FAMILY) {
        return 0;
    }
    if (id == EDID_ED25 || id == EDID_ED3 || id == EDID_EDX || id == EDID_X5) {
        return 0;
    }
    if (!sysstat_signature()) {
        return 0;
    }

    /* Anything already waiting would be read as the status reply. */
    while (drained < DRAIN_LIMIT) {
        uint32_t n = ed64pro_receive(s_scratch, sizeof(s_scratch));
        if (n == 0u || n == ED64PRO_RECEIVE_LOST) {
            break;
        }
        drained += n;
    }

    cmd[0] = 0x2Bu; /* '+' */
    cmd[1] = 0xD4u; /* '+' ^ 0xFF */
    cmd[2] = CMD_STATUS;
    cmd[3] = (uint8_t)(CMD_STATUS ^ 0xFFu);
    if (!pi_io_store_port(cmd, REG_FIFODATA, sizeof(cmd))) {
        return 0;
    }

    for (spins = 0u; got < sizeof(status) && spins < STATUS_SPINS; spins++) {
        uint32_t n = ed64pro_receive(status + got, sizeof(status) - got);
        if (n == ED64PRO_RECEIVE_LOST) {
            return 0;
        }
        got += n;
    }
    return got == sizeof(status) && status[0] == STATUS_KEY && status[1] == PROTOCOL_ID &&
           status[2] == DEVICE_ID_ED64PRO;
}

uint32_t ed64pro_receive(uint8_t *dst, uint32_t cap)
{
    uint32_t n;

    if (!pi_io_read(REG_FIFOSTAT, &n)) {
        return 0u;
    }
    n &= FIFOSTAT_COUNT;
    if (n > cap) {
        n = cap;
    }
    if (n == 0u) {
        return 0u;
    }
    if (!pi_io_load_port(dst, REG_FIFODATA, n)) {
        /* A failed load has still drained some bytes: say so, and the agent drops the partial
           frame they belonged to (#151). */
        return ED64PRO_RECEIVE_LOST;
    }
    return n;
}

static void put_be32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)(v >> 16);
    p[2] = (uint8_t)(v >> 8);
    p[3] = (uint8_t)v;
}

int ed64pro_send(const uint8_t *data, uint32_t len)
{
    while (len > 0u) {
        uint32_t block = len < XFER_BLOCK ? len : XFER_BLOCK;
        uint32_t spins;
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

        if (!pi_io_store_port(head, REG_FIFODATA, sizeof(head)) ||
            !pi_io_store_port(data, REG_FIFODATA, block)) {
            return 0;
        }

        /* Nothing comes back through the FIFO for this; only SYSSTAT says when it is done. */
        for (spins = 0u;; spins++) {
            uint32_t v;
            if (spins >= MCU_SPINS || !pi_io_read(REG_SYSSTAT, &v)) {
                return 0;
            }
            if (!(v & SYSSTAT_BUSY)) {
                break;
            }
        }
        data += block;
        len -= block;
    }
    return 1;
}
