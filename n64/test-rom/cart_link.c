#include "cart_link.h"

#include "ed64pro.h"
#include "test_proto.h"

#include <usb.h>

static enum cart_link_kind s_kind = CART_LINK_NONE;
/* Writes that gave up before the whole message was sent. Since boot: see cart_link_tx_failures(). */
static uint32_t s_tx_failures;

enum cart_link_kind cart_link_init(void)
{
    /* The PRO first. libdragon's EverDrive probe writes the X-series register key and reads its
       version register, at the address where the PRO keeps its device ID; left to libdragon, a PRO
       is at best rejected and at worst driven as an X7. ed64pro_detect() only reads until the cart
       has identified as a PRO, and ignores every X-series ID. */
    if (ed64pro_detect()) {
        s_kind = CART_LINK_ED64PRO;
        return s_kind;
    }

    if (!usb_initialize()) {
        s_kind = CART_LINK_NONE;
        return s_kind;
    }
    switch (usb_getcart()) {
    case CART_SC64:
        s_kind = CART_LINK_SC64;
        break;
    case CART_EVERDRIVE:
        s_kind = CART_LINK_EVERDRIVE;
        break;
    default:
        s_kind = CART_LINK_OTHER;
        break;
    }
    return s_kind;
}

uint32_t cart_link_poll(uint8_t *datatype)
{
    if (s_kind == CART_LINK_ED64PRO) {
        uint32_t n = ed64pro_rx_available();
        if (n == 0u) {
            return 0u;
        }
        if (n > TEST_USB_CHUNK) {
            n = TEST_USB_CHUNK;
        }
        *datatype = CART_LINK_L3;
        return n;
    }

    uint32_t hdr = usb_poll();
    if (hdr == 0u) {
        return 0u;
    }
    *datatype = (uint8_t)USBHEADER_GETTYPE(hdr);
    return USBHEADER_GETSIZE(hdr);
}

void cart_link_read(uint8_t *dst, int len)
{
    if (len <= 0) {
        return;
    }
    if (s_kind == CART_LINK_ED64PRO) {
        /* cart_link_poll() reported at least this many bytes waiting. */
        (void)ed64pro_rx_read(dst, (uint32_t)len);
        return;
    }
    usb_read(dst, len);
}

void cart_link_write(const uint8_t *data, int len)
{
    if (len <= 0) {
        return;
    }
    if (s_kind == CART_LINK_ED64PRO) {
        if (!ed64pro_tx_write(data, (uint32_t)len)) {
            s_tx_failures++;
        }
        return;
    }
    usb_write(CART_LINK_L3, data, len);
    /* libdragon gives up on a write when the cart stays busy, part-way through the message if that
       is where it happened, and says so only through usb_timedout(): set by the write that gave
       up, cleared by one that finished. Nothing reaches the host to tell it why the message it got
       is malformed, so this count is the only record. (usb_write also returns without writing
       while a received message is still unread, leaving the flag as it was; every caller here
       reads the whole message first.) */
    if (usb_timedout()) {
        s_tx_failures++;
    }
}

uint32_t cart_link_tx_failures(void)
{
    return s_tx_failures;
}
