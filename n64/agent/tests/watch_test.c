/*
 * Host-side test of WATCH: the real mem_proto.c compiled for the PC, over an ordinary buffer
 * standing in for RDRAM, with fake transport hooks recording each reply.
 *
 * What it is here to prove is the reason the feature exists (spec 4.3): a value the game writes
 * and overwrites between two of the host's reads is still delivered, in order, because the agent
 * looked every frame. The rest is the rules around that -- a baseline is not an event, a slot
 * cleared to zero is not an event, a filter keeps out what the host would never act on, a full
 * queue drops the oldest and says so, and a host that set no watch sees no trailer.
 *
 * It says nothing about a real console: no cart, no PI bus, and m64p_watch_tick() is called here
 * where a frame hook would call it.
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

#define SLOT 0x40u
#define SLOT_LEN 4

static uint8_t s_req[M64P_APP_CAP];

/** The slot as the game would write it: scene, type, 0, id. */
static void game_writes(uint8_t type, uint8_t id)
{
    s_ram[SLOT] = 0x60;
    s_ram[SLOT + 1] = type;
    s_ram[SLOT + 2] = 0;
    s_ram[SLOT + 3] = id;
}

static void game_clears(void)
{
    memset(&s_ram[SLOT], 0, SLOT_LEN);
}

/**
 * WATCH one slot at `addr` of `len` bytes, keeping changes whose byte `at` is one of
 * `values` (none: every change). Returns the slots the agent says it is watching.
 */
static int watch_one(uint32_t addr, uint8_t len, uint8_t at, const uint8_t *values, uint8_t nvalues)
{
    int k = 8;
    uint8_t i;

    memcpy(s_req, "M64P", 4);
    s_req[4] = M64P_MSG_WATCH;
    s_req[5] = 0;
    s_req[6] = 7; /* rid */
    s_req[7] = 1; /* n */
    s_req[k++] = (uint8_t)(addr >> 24);
    s_req[k++] = (uint8_t)(addr >> 16);
    s_req[k++] = (uint8_t)(addr >> 8);
    s_req[k++] = (uint8_t)addr;
    s_req[k++] = len;
    s_req[k++] = at;
    s_req[k++] = nvalues;
    for (i = 0; i < nvalues; i++) {
        s_req[k++] = values[i];
    }
    s_sends = 0;
    assert(m64p_handle(s_req, (size_t)k) == 1);
    assert(s_sends == 1);
    if (s_last[4] == M64P_MSG_ERR) {
        return -(int)s_last[7];
    }
    assert(s_last[4] == M64P_MSG_WATCH_ACK && s_last_len == 8);
    return s_last[7];
}

/** WATCH with no slots at all, which is how a host stops watching. */
static void watch_none(void)
{
    memcpy(s_req, "M64P", 4);
    s_req[4] = M64P_MSG_WATCH;
    s_req[5] = 0;
    s_req[6] = 9;
    s_req[7] = 0;
    s_sends = 0;
    assert(m64p_handle(s_req, 8) == 1);
    assert(s_last[4] == M64P_MSG_WATCH_ACK && s_last[7] == 0);
}

/** A PEEKV of `n` regions of `len` bytes, all at `addr`, as a host poll would carry. */
static void peekv_many(uint16_t rid, int n, uint32_t addr, uint16_t len)
{
    int k = 8;
    int i;

    memcpy(s_req, "M64P", 4);
    s_req[4] = M64P_MSG_PEEKV;
    s_req[5] = (uint8_t)(rid >> 8);
    s_req[6] = (uint8_t)rid;
    s_req[7] = (uint8_t)n;
    for (i = 0; i < n; i++) {
        s_req[k++] = (uint8_t)(addr >> 24);
        s_req[k++] = (uint8_t)(addr >> 16);
        s_req[k++] = (uint8_t)(addr >> 8);
        s_req[k++] = (uint8_t)addr;
        s_req[k++] = (uint8_t)(len >> 8);
        s_req[k++] = (uint8_t)len;
    }
    s_sends = 0;
    assert(m64p_handle(s_req, (size_t)k) == 1);
    assert(s_sends == 1);
    assert(s_last[4] == M64P_MSG_PEEKV_RESP);
}

static void peekv(uint16_t rid, uint32_t addr, uint16_t len)
{
    peekv_many(rid, 1, addr, len);
}

/** Where the trailer starts in the last PEEKV_RESP of `n` regions of `len` bytes each. */
static int trailer_at(int n, uint16_t len)
{
    return 5 + 3 + n * (2 + (int)len);
}

static int trailer_count(int n, uint16_t len)
{
    return s_last[trailer_at(n, len)];
}

static int trailer_dropped(int n, uint16_t len)
{
    int at = trailer_at(n, len);
    return (s_last[at + 1] << 8) | s_last[at + 2];
}

/** Event `i` of the last response: slot, len and bytes, through `out`. */
static int trailer_event(int n, uint16_t len, int i, uint8_t *out)
{
    int at = trailer_at(n, len) + 3;
    int k;

    for (k = 0; k < i; k++) {
        at += 2 + (int)s_last[at + 1];
    }
    out[0] = s_last[at];
    memcpy(out + 1, &s_last[at + 2], s_last[at + 1]);
    return s_last[at + 1];
}

/* ---- cases --------------------------------------------------------------------------------- */

static void hello_says_how_many_slots_it_watches(void)
{
    memcpy(s_req, "M64P", 4);
    s_req[4] = M64P_MSG_HELLO;
    s_sends = 0;
    assert(m64p_handle(s_req, 5) == 1);
    assert(s_sends == 1 && s_last[4] == M64P_MSG_HELLO_ACK);
    /* No cart ROM here, so watch_slots follows the flags directly: bit 1 clear, bit 2 set. */
    assert((s_last[12] & 0x02u) == 0u);
    assert((s_last[12] & 0x04u) != 0u);
    assert(s_last_len == 5 + 9);
    assert(s_last[13] == M64P_WATCH_SLOTS);
}

/*
 * The whole point. The game writes a value and clears it between two polls; without the watch
 * the second poll reads zeros and the host never learns it happened.
 */
static void a_value_written_and_gone_between_polls_still_reaches_the_host(void)
{
    uint8_t got[1 + M64P_WATCH_MAX_LEN];

    assert(watch_one(SLOT, SLOT_LEN, 0, 0, 0) == 1);
    game_clears();
    m64p_watch_tick(); /* baseline */
    peekv(1, SLOT, SLOT_LEN);
    assert(trailer_count(1, SLOT_LEN) == 0);

    game_writes(0x01, 0x11);
    m64p_watch_tick();
    game_clears();
    m64p_watch_tick();

    peekv(2, SLOT, SLOT_LEN);
    /* The region itself reads zero, because that is what is there now... */
    assert(s_last[10] == 0 && s_last[13] == 0);
    /* ...and the value the game actually wrote comes back beside it. */
    assert(trailer_count(1, SLOT_LEN) == 1);
    assert(trailer_event(1, SLOT_LEN, 0, got) == SLOT_LEN);
    assert(got[0] == 0 && got[1] == 0x60 && got[2] == 0x01 && got[4] == 0x11);
    /* Taken once: a second poll does not deliver it again. */
    peekv(3, SLOT, SLOT_LEN);
    assert(trailer_count(1, SLOT_LEN) == 0);
}

static void events_arrive_oldest_first(void)
{
    uint8_t got[1 + M64P_WATCH_MAX_LEN];

    assert(watch_one(SLOT, SLOT_LEN, 0, 0, 0) == 1);
    game_clears();
    m64p_watch_tick();
    game_writes(0x01, 0x21);
    m64p_watch_tick();
    game_writes(0x01, 0x22);
    m64p_watch_tick();
    game_writes(0x01, 0x23);
    m64p_watch_tick();

    peekv(4, SLOT, SLOT_LEN);
    assert(trailer_count(1, SLOT_LEN) == 3);
    trailer_event(1, SLOT_LEN, 0, got);
    assert(got[4] == 0x21);
    trailer_event(1, SLOT_LEN, 1, got);
    assert(got[4] == 0x22);
    trailer_event(1, SLOT_LEN, 2, got);
    assert(got[4] == 0x23);
}

/* Reading the slot for the first time says what was there, not that anything happened. */
static void the_first_look_is_a_baseline(void)
{
    game_writes(0x01, 0x31);
    assert(watch_one(SLOT, SLOT_LEN, 0, 0, 0) == 1);
    m64p_watch_tick();
    peekv(5, SLOT, SLOT_LEN);
    assert(trailer_count(1, SLOT_LEN) == 0);
}

/* The game finishing with a slot is not an event, and would otherwise double every count. */
static void clearing_the_slot_is_not_an_event(void)
{
    assert(watch_one(SLOT, SLOT_LEN, 0, 0, 0) == 1);
    game_clears();
    m64p_watch_tick(); /* baseline */
    game_writes(0x01, 0x41);
    m64p_watch_tick();
    game_clears();
    m64p_watch_tick();
    peekv(6, SLOT, SLOT_LEN);
    assert(trailer_count(1, SLOT_LEN) == 1); /* the write, not the clear */
}

static void the_same_value_read_again_is_not_a_second_event(void)
{
    assert(watch_one(SLOT, SLOT_LEN, 0, 0, 0) == 1);
    game_clears();
    m64p_watch_tick();
    game_writes(0x01, 0x51);
    m64p_watch_tick();
    m64p_watch_tick();
    m64p_watch_tick();
    peekv(7, SLOT, SLOT_LEN);
    assert(trailer_count(1, SLOT_LEN) == 1);
}

/* A queue full of events the host would never act on is a queue that delays the ones it would. */
static void a_filter_keeps_out_what_the_host_would_not_act_on(void)
{
    static const uint8_t keep[] = { 0x01, 0x02 };
    uint8_t got[1 + M64P_WATCH_MAX_LEN];

    assert(watch_one(SLOT, SLOT_LEN, 1, keep, 2) == 1);
    game_clears();
    m64p_watch_tick();
    game_writes(0x03, 0x61); /* a type the host never matches */
    m64p_watch_tick();
    game_writes(0x02, 0x62);
    m64p_watch_tick();

    peekv(8, SLOT, SLOT_LEN);
    assert(trailer_count(1, SLOT_LEN) == 1);
    trailer_event(1, SLOT_LEN, 0, got);
    assert(got[2] == 0x02 && got[4] == 0x62);
}

static void a_full_queue_drops_the_oldest_and_says_so(void)
{
    uint8_t got[1 + M64P_WATCH_MAX_LEN];
    int i;

    assert(watch_one(SLOT, SLOT_LEN, 0, 0, 0) == 1);
    game_clears();
    m64p_watch_tick();
    for (i = 0; i < M64P_WATCH_QUEUE + 2; i++) {
        game_writes(0x01, (uint8_t)i);
        m64p_watch_tick();
    }
    peekv(9, SLOT, SLOT_LEN);
    assert(trailer_count(1, SLOT_LEN) == M64P_WATCH_QUEUE);
    assert(trailer_dropped(1, SLOT_LEN) == 2);
    trailer_event(1, SLOT_LEN, 0, got);
    assert(got[4] == 2 && "the two oldest went, not the two newest");
    assert(m64p_get_watch_dropped() == 2);
}

/**
 * More events than one response has room for: the rest ride the next one, still in order.
 *
 * A near-full response is what makes this happen, so the regions here are sized to leave the
 * trailer a few bytes: 32 x 247 bytes is a request at the limits a poll really uses.
 */
static void events_that_do_not_fit_wait_for_the_next_response(void)
{
    uint8_t got[1 + M64P_WATCH_MAX_LEN];
    const int n = M64P_MAX_REGIONS;
    const uint16_t len = 247;
    /* app header + n x (len header + bytes), against the cap the reply buffer promises. */
    const int room = M64P_APP_CAP - (5 + 3 + n * (2 + (int)len));
    const int fits = (room - 3) / (2 + SLOT_LEN);
    int i;

    assert(fits > 0 && fits < 6);
    assert(watch_one(SLOT, SLOT_LEN, 0, 0, 0) == 1);
    game_clears();
    m64p_watch_tick();
    for (i = 1; i <= 6; i++) {
        game_writes(0x01, (uint8_t)i);
        m64p_watch_tick();
    }

    peekv_many(10, n, 0, len);
    assert(trailer_count(n, len) == fits);
    trailer_event(n, len, 0, got);
    assert(got[4] == 1 && "oldest first, even when only some fit");

    peekv(11, SLOT, SLOT_LEN);
    assert(trailer_count(1, SLOT_LEN) == 6 - fits);
    trailer_event(1, SLOT_LEN, 0, got);
    assert(got[4] == (uint8_t)(fits + 1) && "the next one continues where that left off");
}

/* A response with no room at all keeps its events rather than losing them. */
static void a_response_with_no_room_carries_no_trailer(void)
{
    uint8_t got[1 + M64P_WATCH_MAX_LEN];
    const int n = M64P_MAX_REGIONS;
    const uint16_t len = (uint16_t)(M64P_MAX_TOTAL_BYTES / M64P_MAX_REGIONS);

    assert(watch_one(SLOT, SLOT_LEN, 0, 0, 0) == 1);
    game_clears();
    m64p_watch_tick();
    game_writes(0x01, 0xC1);
    m64p_watch_tick();

    peekv_many(12, n, 0, len);
    assert(s_last_len == 5 + 3 + n * (2 + (int)len) && "the response is full to the cap");

    peekv(13, SLOT, SLOT_LEN);
    assert(trailer_count(1, SLOT_LEN) == 1);
    trailer_event(1, SLOT_LEN, 0, got);
    assert(got[4] == 0xC1);
}

/* A host that never asked must see exactly the response it has always seen. */
static void without_a_watch_a_response_carries_no_trailer(void)
{
    watch_none();
    game_writes(0x01, 0x71);
    m64p_watch_tick();
    peekv(12, SLOT, SLOT_LEN);
    assert(s_last_len == 5 + 3 + 2 + SLOT_LEN);
    assert(m64p_get_watching() == 0);
}

/* A new host has not said what to watch, and the last one's events are not its to read. */
static void hello_forgets_what_the_last_host_was_watching(void)
{
    assert(watch_one(SLOT, SLOT_LEN, 0, 0, 0) == 1);
    game_clears();
    m64p_watch_tick();
    game_writes(0x01, 0x81);
    m64p_watch_tick();

    memcpy(s_req, "M64P", 4);
    s_req[4] = M64P_MSG_HELLO;
    assert(m64p_handle(s_req, 5) == 1);
    assert(m64p_get_watching() == 0);
    peekv(13, SLOT, SLOT_LEN);
    assert(s_last_len == 5 + 3 + 2 + SLOT_LEN && "no watch, no trailer");
}

static void a_watch_the_agent_cannot_take_is_refused(void)
{
    static const uint8_t vals[M64P_WATCH_MAX_VALUES + 1] = { 0 };

    assert(watch_one(SLOT, M64P_WATCH_MAX_LEN + 1, 0, 0, 0) == -(int)M64P_ERR_TOO_LARGE);
    assert(watch_one(SLOT, 0, 0, 0, 0) == -(int)M64P_ERR_TOO_LARGE);
    assert(watch_one(SLOT, SLOT_LEN, 0, vals, M64P_WATCH_MAX_VALUES + 1) ==
           -(int)M64P_ERR_TOO_LARGE);
    assert(watch_one(sizeof s_ram - 2, SLOT_LEN, 0, 0, 0) == -(int)M64P_ERR_RANGE);
    assert(watch_one(SLOT, SLOT_LEN, SLOT_LEN, vals, 1) == -(int)M64P_ERR_RANGE);
}

/* A rejected request must not disturb what the host set before it. */
static void a_refused_watch_leaves_the_slots_alone(void)
{
    assert(watch_one(SLOT, SLOT_LEN, 0, 0, 0) == 1);
    assert(watch_one(SLOT, M64P_WATCH_MAX_LEN + 1, 0, 0, 0) == -(int)M64P_ERR_TOO_LARGE);
    assert(m64p_get_watching() == 1);
    game_clears();
    m64p_watch_tick();
    game_writes(0x01, 0x91);
    m64p_watch_tick();
    peekv(14, SLOT, SLOT_LEN);
    assert(trailer_count(1, SLOT_LEN) == 1);
}

static void too_many_slots_is_refused(void)
{
    int k = 8;
    int i;

    memcpy(s_req, "M64P", 4);
    s_req[4] = M64P_MSG_WATCH;
    s_req[5] = 0;
    s_req[6] = 15;
    s_req[7] = (uint8_t)(M64P_WATCH_SLOTS + 1);
    for (i = 0; i < M64P_WATCH_SLOTS + 1; i++) {
        s_req[k++] = 0;
        s_req[k++] = 0;
        s_req[k++] = 0;
        s_req[k++] = (uint8_t)(SLOT + i * SLOT_LEN);
        s_req[k++] = SLOT_LEN;
        s_req[k++] = 0;
        s_req[k++] = 0;
    }
    assert(m64p_handle(s_req, (size_t)k) == 1);
    assert(s_last[4] == M64P_MSG_ERR && s_last[7] == M64P_ERR_TOO_MANY);
}

/* Several slots share one queue, and each event says which slot it came from. */
static void every_slot_reports_through_the_same_queue(void)
{
    uint8_t got[1 + M64P_WATCH_MAX_LEN];
    int k = 8;
    int i;

    memcpy(s_req, "M64P", 4);
    s_req[4] = M64P_MSG_WATCH;
    s_req[5] = 0;
    s_req[6] = 17;
    s_req[7] = 2;
    for (i = 0; i < 2; i++) {
        s_req[k++] = 0;
        s_req[k++] = 0;
        s_req[k++] = 0;
        s_req[k++] = (uint8_t)(SLOT + i * SLOT_LEN);
        s_req[k++] = SLOT_LEN;
        s_req[k++] = 0;
        s_req[k++] = 0;
    }
    assert(m64p_handle(s_req, (size_t)k) == 1);
    assert(s_last[4] == M64P_MSG_WATCH_ACK && s_last[7] == 2);

    memset(&s_ram[SLOT], 0, SLOT_LEN * 2);
    m64p_watch_tick();
    s_ram[SLOT + SLOT_LEN] = 0xAB; /* the second slot changes */
    m64p_watch_tick();

    peekv(18, SLOT, SLOT_LEN);
    assert(trailer_count(1, SLOT_LEN) == 1);
    trailer_event(1, SLOT_LEN, 0, got);
    assert(got[0] == 1 && "slot index 1");
    assert(got[1] == 0xAB);
}

int main(void)
{
    hello_says_how_many_slots_it_watches();
    a_value_written_and_gone_between_polls_still_reaches_the_host();
    events_arrive_oldest_first();
    the_first_look_is_a_baseline();
    clearing_the_slot_is_not_an_event();
    the_same_value_read_again_is_not_a_second_event();
    a_filter_keeps_out_what_the_host_would_not_act_on();
    a_full_queue_drops_the_oldest_and_says_so();
    events_that_do_not_fit_wait_for_the_next_response();
    a_response_with_no_room_carries_no_trailer();
    without_a_watch_a_response_carries_no_trailer();
    hello_forgets_what_the_last_host_was_watching();
    a_watch_the_agent_cannot_take_is_refused();
    a_refused_watch_leaves_the_slots_alone();
    too_many_slots_is_refused();
    every_slot_reports_through_the_same_queue();
    printf("watch: all cases passed\n");
    return 0;
}
