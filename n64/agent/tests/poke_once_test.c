/*
 * Host-side test of a repeated POKEV (spec 4.4): the real mem_proto.c compiled for the PC, over an
 * ordinary buffer standing in for RDRAM, with fake transport hooks recording each reply.
 *
 * Built twice. With M64P_POKE_ONCE, as every agent build is, it checks the reason the switch
 * exists: a host that never saw its write acknowledged sends it again under the same rid, and a
 * mailbox the game emptied after the first copy landed is not filled again. Then the rules around
 * that -- only the request straight after a write can repeat it, a refused write holds nothing,
 * HELLO ends the hold, and a new rid is a new write. Without the switch, as the test ROM and
 * n64/bringup's ROM build mem_proto.c, it checks that HELLO_ACK says nothing of it, so a host knows
 * a copy sent again would be applied again, which this also shows.
 *
 * It says nothing about a real console: no cart, no PI bus, no frame hook.
 */
#undef NDEBUG
#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "mem_proto.h"

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

static uint8_t s_ram[256];

uint32_t m64p_rdram_size(void)
{
    return sizeof s_ram;
}

volatile uint8_t *m64p_rdram_base(void)
{
    return s_ram;
}

/* PEEKROM is another test's subject; this host serves no cart ROM. */
uint32_t m64p_cart_rom_size(void)
{
    return 0;
}

int m64p_cart_rom_read(uint32_t off, uint8_t *dst, uint32_t len)
{
    (void)off;
    (void)dst;
    (void)len;
    return 0;
}

/* ---- helpers ------------------------------------------------------------------------------- */

/* A mailbox: the host puts an item in it, the game takes the item out and zeroes it. */
#define MAILBOX 0x40u

static uint8_t s_req[64];

static void handle(int len)
{
    s_sends = 0;
    assert(m64p_handle(s_req, (size_t)len) == 1);
    assert(s_sends == 1 && "every request is answered exactly once");
}

static void hello(void)
{
    memcpy(s_req, "M64P", 4);
    s_req[4] = M64P_MSG_HELLO;
    handle(5);
    assert(s_last[4] == M64P_MSG_HELLO_ACK);
}

/**
 * A POKEV of one byte, `value` at `addr`, under `rid`. Returns the `applied` its POKE_ACK
 * carries, or minus the ERR code.
 */
static int poke(uint16_t rid, uint32_t addr, uint8_t value)
{
    memcpy(s_req, "M64P", 4);
    s_req[4] = M64P_MSG_POKEV;
    s_req[5] = (uint8_t)(rid >> 8);
    s_req[6] = (uint8_t)rid;
    s_req[7] = 1; /* n */
    s_req[8] = (uint8_t)(addr >> 24);
    s_req[9] = (uint8_t)(addr >> 16);
    s_req[10] = (uint8_t)(addr >> 8);
    s_req[11] = (uint8_t)addr;
    s_req[12] = 0;
    s_req[13] = 1; /* len */
    s_req[14] = value;
    handle(15);
    assert(s_last[5] == (uint8_t)(rid >> 8) && s_last[6] == (uint8_t)rid && "the rid is echoed");
    if (s_last[4] == M64P_MSG_ERR) {
        return -(int)s_last[7];
    }
    assert(s_last[4] == M64P_MSG_POKE_ACK && s_last_len == 8);
    return s_last[7];
}

/* The game, between two of the agent's frames: it delivers the item and empties the mailbox. */
static int game_takes_the_item(void)
{
    int item = s_ram[MAILBOX];
    s_ram[MAILBOX] = 0;
    return item;
}

#ifdef M64P_POKE_ONCE

static void peek(uint16_t rid)
{
    memcpy(s_req, "M64P", 4);
    s_req[4] = M64P_MSG_PEEKV;
    s_req[5] = (uint8_t)(rid >> 8);
    s_req[6] = (uint8_t)rid;
    s_req[7] = 1;
    s_req[8] = 0;
    s_req[9] = 0;
    s_req[10] = 0;
    s_req[11] = (uint8_t)MAILBOX;
    s_req[12] = 0;
    s_req[13] = 1;
    handle(14);
    assert(s_last[4] == M64P_MSG_PEEKV_RESP);
}

static void hello_says_a_repeated_write_is_recognized(void)
{
    hello();
    assert((s_last[12] & M64P_FLAG_POKE_ONCE) != 0u);
    /* Bit 3 appends no field: still only watch_slots after the flags (no cart ROM here). */
    assert(s_last_len == 5 + 9);
}

/*
 * The whole point. The first copy landed, but its acknowledgment never reached the host, which
 * sends the write again under the same rid. The game took the item in between; it must not get
 * it a second time.
 */
static void a_write_sent_again_after_it_landed_is_acknowledged_not_applied(void)
{
    hello();
    assert(poke(10, MAILBOX, 0x2A) == 1);
    assert(game_takes_the_item() == 0x2A);
    assert(poke(10, MAILBOX, 0x2A) == 1 && "acknowledged as the first copy was");
    assert(game_takes_the_item() == 0 && "the item was delivered once");
    /* However many copies the host sent while it waited. */
    assert(poke(10, MAILBOX, 0x2A) == 1);
    assert(s_ram[MAILBOX] == 0);
}

/* A write the agent never received is the one a resend is for: its first copy is applied. */
static void a_write_whose_first_copy_was_lost_lands_from_the_resend(void)
{
    hello();
    peek(20);
    assert(poke(21, MAILBOX, 0x33) == 1);
    assert(game_takes_the_item() == 0x33);
}

/* Each write the host means is its own rid, and is applied. */
static void a_new_rid_is_a_new_write(void)
{
    hello();
    assert(poke(30, MAILBOX, 0x01) == 1);
    assert(game_takes_the_item() == 0x01);
    assert(poke(31, MAILBOX, 0x02) == 1);
    assert(game_takes_the_item() == 0x02);
}

/*
 * The agent holds one write, and only until it serves anything else. A host resends at once or
 * not at all, so a rid seen again after another request is a new write: a host whose rids wrap
 * can reuse one without the agent mistaking it for a repeat.
 */
static void another_request_in_between_ends_the_hold(void)
{
    hello();
    assert(poke(40, MAILBOX, 0x05) == 1);
    assert(game_takes_the_item() == 0x05);
    peek(41);
    assert(poke(40, MAILBOX, 0x05) == 1);
    assert(game_takes_the_item() == 0x05 && "applied again: the read in between ended the hold");

    /* A write under another rid ends it too, and is held in its place. */
    assert(poke(42, MAILBOX, 0x06) == 1);
    assert(poke(43, MAILBOX, 0x07) == 1);
    assert(game_takes_the_item() == 0x07);
    assert(poke(42, MAILBOX, 0x06) == 1);
    assert(game_takes_the_item() == 0x06);
    assert(poke(42, MAILBOX, 0x06) == 1);
    assert(game_takes_the_item() == 0 && "42 is the write held now");
}

/* Nothing was written, so there is nothing to hold: a resend is checked, and refused, again. */
static void a_refused_write_holds_nothing(void)
{
    hello();
    assert(poke(50, MAILBOX, 0x08) == 1);
    assert(poke(51, sizeof s_ram, 0x09) == -(int)M64P_ERR_RANGE);
    assert(poke(51, sizeof s_ram, 0x09) == -(int)M64P_ERR_RANGE);
    /* And the refusal ended the hold on the write before it. */
    assert(game_takes_the_item() == 0x08);
    assert(poke(50, MAILBOX, 0x08) == 1);
    assert(game_takes_the_item() == 0x08);
}

/* HELLO is a new host, or the same one starting over: nothing it sends is a resend. */
static void hello_ends_the_hold(void)
{
    hello();
    assert(poke(1, MAILBOX, 0x0A) == 1);
    assert(game_takes_the_item() == 0x0A);
    hello();
    assert(poke(1, MAILBOX, 0x0A) == 1);
    assert(game_takes_the_item() == 0x0A);
}

/* The acknowledgment of a repeat is the first copy's, applied count included. */
static void a_repeat_is_acknowledged_with_the_first_copys_count(void)
{
    hello();
    memcpy(s_req, "M64P", 4);
    s_req[4] = M64P_MSG_POKEV;
    s_req[5] = 0;
    s_req[6] = 60;
    s_req[7] = 0; /* n = 0 is legal, and applies nothing */
    handle(8);
    assert(s_last[4] == M64P_MSG_POKE_ACK && s_last[7] == 0);
    assert(poke(61, MAILBOX, 0x0B) == 1);
    assert(game_takes_the_item() == 0x0B);
    /* 61 again: acknowledged with its own count, 1, not the 0 before it. */
    assert(poke(61, MAILBOX, 0x0B) == 1);
    assert(game_takes_the_item() == 0);
}

int main(void)
{
    hello_says_a_repeated_write_is_recognized();
    a_write_sent_again_after_it_landed_is_acknowledged_not_applied();
    a_write_whose_first_copy_was_lost_lands_from_the_resend();
    a_new_rid_is_a_new_write();
    another_request_in_between_ends_the_hold();
    a_refused_write_holds_nothing();
    hello_ends_the_hold();
    a_repeat_is_acknowledged_with_the_first_copys_count();
    printf("poke_once: all cases passed\n");
    return 0;
}

#else /* the test ROM's mem_proto.c, and n64/bringup's */

/* Bit 3 clear: a host must not send this agent a write twice, and here is why. */
static void without_the_switch_a_repeat_is_applied_again_and_hello_says_so(void)
{
    hello();
    assert((s_last[12] & M64P_FLAG_POKE_ONCE) == 0u);
    assert(poke(10, MAILBOX, 0x2A) == 1);
    assert(game_takes_the_item() == 0x2A);
    assert(poke(10, MAILBOX, 0x2A) == 1);
    assert(game_takes_the_item() == 0x2A && "delivered twice");
}

int main(void)
{
    without_the_switch_a_repeat_is_applied_again_and_hello_says_so();
    printf("poke_once (switch off): all cases passed\n");
    return 0;
}

#endif
