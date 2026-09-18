/*
 * Host-side test of PEEKROM: the real mem_proto.c and cart_rom.c, compiled for the PC, with fake
 * pi_io_load_words() serving a known ROM image and fake transport hooks recording each reply.
 *
 * It checks the protocol handling (HELLO_ACK advertises the ROM window, PEEKROM answers in the
 * request's order, ranges and limits are refused, a busy bus is reported rather than guessed
 * around) and cart_rom.c's byte extraction from aligned big-endian words at every alignment. It
 * says nothing about reading a real cart.
 */
#undef NDEBUG
#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "mem_proto.h"
#include "pi_io.h"

/* ---- fake PI: a ROM image whose byte at offset o is (o * 13 + 5) & 0xFF ------------------ */

static uint32_t s_loads;         /* pi_io_load_words calls */
static uint32_t s_busy_after = ~0u; /* fail the load after this many succeed */

static uint8_t rom_byte(uint32_t off)
{
    return (uint8_t)((off * 13u + 5u) & 0xFFu);
}

int pi_io_load_words(uint32_t *dst, uint32_t addr, uint32_t words)
{
    uint32_t w;
    assert((addr & 3u) == 0u && "the PI reads aligned words");
    assert(addr >= 0x10000000u && addr < 0x14000000u && "cart ROM space only");
    assert(words > 0u && words <= 16u);
    if (s_loads++ >= s_busy_after) {
        return 0;
    }
    for (w = 0; w < words; w++) {
        uint32_t o = addr - 0x10000000u + w * 4u;
        dst[w] = ((uint32_t)rom_byte(o) << 24) | ((uint32_t)rom_byte(o + 1u) << 16) |
                 ((uint32_t)rom_byte(o + 2u) << 8) | (uint32_t)rom_byte(o + 3u);
    }
    return 1;
}

/* Never called by PEEKROM; present because pi_io.h declares them. */
int pi_io_read(uint32_t addr, uint32_t *value) { (void)addr; (void)value; return 0; }
int pi_io_write(uint32_t addr, uint32_t value) { (void)addr; (void)value; return 0; }

/* ---- fake transport ------------------------------------------------------------------------ */

static uint8_t s_reply_space[M64P_APP_CAP];
static uint8_t s_last[M64P_APP_CAP];
static int s_last_len;
static int s_sends;

void m64p_transport_send(const uint8_t *app, int app_len)
{
    assert(app_len > 0 && app_len <= (int)sizeof s_last);
    memmove(s_last, app, (size_t)app_len);
    s_last_len = app_len;
    s_sends++;
}

uint8_t *m64p_reply_buffer(void)
{
    return s_reply_space;
}

static uint8_t s_ram[64];

uint32_t m64p_rdram_size(void)
{
    return sizeof s_ram;
}

volatile uint8_t *m64p_rdram_base(void)
{
    return s_ram;
}

/* ---- helpers ------------------------------------------------------------------------------- */

static uint8_t s_req[M64P_APP_CAP];

/** A PEEKROM with `n` regions from `regions` (addr, len pairs). */
static void peekrom(uint16_t rid, const uint32_t *regions, int n)
{
    int k = 0;
    int i;
    memcpy(s_req, "M64P", 4);
    s_req[4] = M64P_MSG_PEEKROM;
    s_req[5] = (uint8_t)(rid >> 8);
    s_req[6] = (uint8_t)rid;
    s_req[7] = (uint8_t)n;
    k = 8;
    for (i = 0; i < n; i++) {
        uint32_t a = regions[2 * i];
        uint32_t l = regions[2 * i + 1];
        s_req[k++] = (uint8_t)(a >> 24);
        s_req[k++] = (uint8_t)(a >> 16);
        s_req[k++] = (uint8_t)(a >> 8);
        s_req[k++] = (uint8_t)a;
        s_req[k++] = (uint8_t)(l >> 8);
        s_req[k++] = (uint8_t)l;
    }
    s_sends = 0;
    assert(m64p_handle(s_req, (size_t)k) == 1);
    assert(s_sends == 1);
}

static uint8_t err_code(uint16_t rid)
{
    assert(s_last_len == 8 && s_last[4] == M64P_MSG_ERR);
    assert(((s_last[5] << 8) | s_last[6]) == rid);
    return s_last[7];
}

/* ---- cases --------------------------------------------------------------------------------- */

static void hello_advertises_the_rom_window(void)
{
    const uint8_t hello[5] = {'M', '6', '4', 'P', M64P_MSG_HELLO};
    s_sends = 0;
    assert(m64p_handle(hello, sizeof hello) == 1 && s_sends == 1);
    assert(s_last_len == 5 + 12 && s_last[4] == M64P_MSG_HELLO_ACK);
    /* The first eight body bytes are where a host that predates PEEKROM reads them. */
    assert(s_last[5] == M64P_PROTO_VERSION);
    assert(s_last[8] == 0 && s_last[9] == 0 && s_last[10] == 0 && s_last[11] == 64);
    assert(s_last[12] == (M64P_FLAG_WRITABLE | M64P_FLAG_CART_ROM));
    assert(s_last[13] == 0x04 && s_last[14] == 0 && s_last[15] == 0 && s_last[16] == 0);
}

static void every_alignment_reads_the_right_bytes(void)
{
    uint32_t off;
    uint32_t len;
    for (off = 0x20u; off < 0x28u; off++) {
        for (len = 1u; len <= 70u; len++) {
            uint32_t r[2];
            uint32_t i;
            r[0] = off;
            r[1] = len;
            peekrom(0x0101, r, 1);
            assert(s_last[4] == M64P_MSG_PEEKROM_RESP);
            assert(s_last[7] == 1);
            assert(((s_last[8] << 8) | s_last[9]) == (int)len);
            for (i = 0; i < len; i++) {
                assert(s_last[10 + i] == rom_byte(off + i));
            }
            assert(s_last_len == (int)(10 + len));
        }
    }
}

static void regions_come_back_in_request_order(void)
{
    /* The reads the Archipelago clients make at connect: the name, a magic, the slot auth. */
    const uint32_t r[] = {0x20u, 0x14u, 0x1D00000u, 4u, 0x0BFFFF0u, 16u, 0x3FFFFFCu, 4u};
    int k = 10;
    int i;
    peekrom(0x0202, r, 4);
    assert(s_last[4] == M64P_MSG_PEEKROM_RESP && s_last[7] == 4);
    for (i = 0; i < 4; i++) {
        uint32_t len = r[2 * i + 1];
        uint32_t j;
        assert(((s_last[k - 2] << 8) | s_last[k - 1]) == (int)len);
        for (j = 0; j < len; j++) {
            assert(s_last[k + j] == rom_byte(r[2 * i] + j));
        }
        k += (int)len + 2;
    }
}

static void a_full_request_fits(void)
{
    const uint32_t r[] = {0x1000u, 4096u, 0x3000u, 3840u}; /* 7936 bytes */
    peekrom(0x0303, r, 2);
    assert(s_last[4] == M64P_MSG_PEEKROM_RESP);
    assert(s_last_len == 5 + 3 + 2 + 4096 + 2 + 3840);
}

static void out_of_range_and_over_limit_are_refused(void)
{
    const uint32_t past[] = {0x3FFFFFEu, 4u};
    const uint32_t huge[] = {0u, 4097u};
    const uint32_t total[] = {0u, 4096u, 0x1000u, 3841u};
    peekrom(0x0404, past, 1);
    assert(err_code(0x0404) == M64P_ERR_RANGE);
    peekrom(0x0405, huge, 1);
    assert(err_code(0x0405) == M64P_ERR_TOO_LARGE);
    peekrom(0x0406, total, 2);
    assert(err_code(0x0406) == M64P_ERR_TOO_LARGE);
}

static void a_busy_bus_is_reported_not_guessed(void)
{
    const uint32_t r[] = {0x40u, 200u};
    s_loads = 0;
    s_busy_after = 2; /* the third 64-byte chunk finds the PI busy */
    peekrom(0x0505, r, 1);
    s_busy_after = ~0u;
    assert(err_code(0x0505) == M64P_ERR_BUSY);
}

static void peekv_still_reads_rdram(void)
{
    const uint8_t req[] = {'M', '6', '4', 'P', M64P_MSG_PEEKV, 0x06, 0x06, 1, 0, 0, 0, 0x10, 0, 4};
    const uint8_t past[] = {'M', '6', '4', 'P', M64P_MSG_PEEKV, 0x06, 0x07, 1, 0, 0, 0, 0x40, 0, 1};
    memcpy(s_ram + 0x10, "\xDE\xAD\xBE\xEF", 4);
    s_sends = 0;
    assert(m64p_handle(req, sizeof req) == 1 && s_sends == 1);
    assert(s_last[4] == M64P_MSG_PEEKV_RESP && memcmp(s_last + 10, "\xDE\xAD\xBE\xEF", 4) == 0);
    s_sends = 0;
    assert(m64p_handle(past, sizeof past) == 1);
    assert(err_code(0x0607) == M64P_ERR_RANGE && "RDRAM keeps its own range, not the ROM's");
}

int main(void)
{
    hello_advertises_the_rom_window();
    every_alignment_reads_the_right_bytes();
    regions_come_back_in_request_order();
    a_full_request_fits();
    out_of_range_and_over_limit_are_refused();
    a_busy_bus_is_reported_not_guessed();
    peekv_still_reads_rdram();
    printf("cart_rom: all cases passed\n");
    return 0;
}
