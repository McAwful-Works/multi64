/*
 * The game logic GoldenEye's Archipelago world runs every frame from its BizHawk Lua, done on
 * the console instead (#395).
 *
 * NotFamilyJules's goldeneye_ap.lua (main 21302eb1, lines 259-705) keeps two callbacks besides
 * its connection to the client. On event.onframestart it resets the world's mailboxes when a
 * mission starts, holds the debrief screens, counts the hand-timer pulses that say a mission
 * has finished starting, shows or hides the mission's tank, and turns on-screen weapon
 * pickups into the world's token item, consuming the token before the game's next frame.
 * On event.onframeend it clears stale key-item mailboxes and runs the pickups again. The
 * world's ROM code relies on all of it, and an AP64 stand-in that only answers the client's
 * requests would do none of it, so it is ported here, function for function, and called from
 * the hook stub: game_frame_start() once per pass of the main loop before the game's update
 * (bossMainloop's joyConsumeSamplesWrapper call), game_frame_end() after lvlRender (its
 * dynSwapBuffers call). The Lua runs once per video frame; this runs once per game frame,
 * which is when the state it reads can change.
 *
 * Every address is physical RDRAM, as the Lua's mainmemory reads are; pointers the game
 * stores are KSEG0 and go through to_rdram() first, as there. Each function names the Lua
 * lines it ports. Where the Lua has an unbounded table, the limits below are the C side's,
 * and nothing past them is tracked.
 *
 * When the world's Lua changes, this has to be compared against it again: the profile pins
 * the world's ROM, not its Lua.
 */
#include <stdint.h>

void game_frame_start(void);
void game_frame_end(void);

/* ---- addresses and offsets (Lua 260-332) ------------------------------------------------ */

#define BONDDATA_PTR_ADDR 0x07A0B0u
#define SCREEN_ID_ADDR 0x02A8C0u
#define MISSION_ID_ADDR 0x02A8F8u
#define SCREEN_GAMEPLAY 0x0Bu
#define SCREEN_MAP 0x07u
#define SCREEN_DEBRIEF_1 0x0Cu
#define SCREEN_DEBRIEF_2 0x0Du

#define TARGET_EC_VALUE 1u
#define TARGET_PULSE_COUNT 4u
#define STARTUP_READY_MAILBOX_ADDR 0x7F020u

#define KEY_ITEM_FLAGS_MAILBOX_ADDR 0x7F200u
#define KEY_ITEM_STATE_MAILBOX_ADDR 0x7F204u

#define FREESTANDING_RESULT_MAILBOX_ADDR 0x7F1C0u
#define FREESTANDING_TARGET_COUNT_ADDR 0x7F1C4u
#define FREESTANDING_TARGET_LIST_ADDR 0x7F150u
#define FREESTANDING_TARGET_BITS 28u
#define TANK_OWNED_ADDR 0x7F20Au
#define TANK_DESCRIPTOR_ADDR 0x7F210u
#define ITEM_TOKEN 88u
#define INV_ITEM_NONE (-1)
#define INV_ITEM_WEAPON 1
#define INV_ITEM_PROP 2
#define INV_ITEM_SIZE 0x14u

#define OFF_HANDS_BASE 0x870u
#define HAND_OFF_TIMER_EC 0x1Cu
#define HAND_OFF_WEAPONNUM 0x00u
#define HAND_OFF_WEAPONNUM_WATCH 0x04u
#define HAND_OFF_CURRENT_ANIM 0x2Cu
#define HAND_OFF_AMMO_IN_MAG 0x30u
#define HAND_OFF_NEXT_WEAPON 0x40u
#define HAND_OFF_ANIM_TRIGGER 0x48u
#define OFF_HAND_INVISIBLE 0x7F8u
#define OFF_HAND_ITEM 0x800u
#define OFF_EQUIP_CUR 0x11F0u
#define OFF_FIELD_2A44 0x2A44u
#define OFF_LOCK_HAND_MODEL 0x2A4Cu
#define OFF_PLAYER_PROP 0xA8u

#define ONSCREEN_PROP_LIST 0x071620u
#define ONSCREEN_PROP_COUNT 0x071DF4u
#define PROP_POOL 0x069C38u
#define PROP_SIZE 0x34u
#define PROP_MAX 600u
#define PROP_OFF_OBJ 0x04u
#define PROP_OFF_PARENT 0x1Cu
#define OBJ_OFF_TYPE 0x03u
#define OBJ_OFF_MODEL_PAD 0x04u
#define OBJ_OFF_FLAGS 0x08u
#define OBJ_OFF_PROP 0x10u
#define WEP_OFF_WEAPONNUM 0x80u
#define PROPFLAG_00080000 0x00080000u
#define PROPFLAG_UNCOLLECTABLE 0x00100000u
#define PROPFLAG_CANNOT_ACTIVATE 0x02000000u
#define OFF_INV_HEAD 0x11E0u
#define OFF_INV_POOL 0x11E4u
#define OFF_INV_MAX 0x11E8u

/* The mailboxes a mission start clears (Lua 633-649). */
#define MISSION_ATTEMPT_ADDR 0x7F218u

/* The C side's limits. The Lua caps the published targets at 32 (line 450) and walks at most
   128 inventory entries (line 403); it keeps every matching object per target, which here
   stops at MAX_OBJECTS. */
#define MAX_TARGETS 32u
#define MAX_OBJECTS 12u
#define MAX_PACKED ((MAX_TARGETS * FREESTANDING_TARGET_BITS + 7u) / 8u)
#define MAX_INV 128u

/* ---- memory (Lua 334-357) --------------------------------------------------------------- */

static inline uint32_t r32(uint32_t a) { return *(volatile uint32_t *)(0x80000000u | a); }
static inline int32_t rs32(uint32_t a) { return *(volatile int32_t *)(0x80000000u | a); }
static inline uint8_t r8(uint32_t a) { return *(volatile uint8_t *)(0x80000000u | a); }
static inline void w32(uint32_t a, uint32_t v) { *(volatile uint32_t *)(0x80000000u | a) = v; }
static inline void w8(uint32_t a, uint8_t v) { *(volatile uint8_t *)(0x80000000u | a) = v; }

/* A pointer the game stored, as a physical address; 0 when it is not one (Lua 346-357). */
static uint32_t to_rdram(uint32_t v)
{
    if (v == 0u) {
        return 0u;
    }
    if (v < 0x800000u) {
        return v;
    }
    if (v >= 0x80000000u && v < 0x80800000u) {
        return v - 0x80000000u;
    }
    return 0u;
}

/* ---- state (Lua 269-279, 313, 331-332, 292) -------------------------------------------- */

struct tracked {
    uint32_t obj;
    uint32_t prop;
    uint8_t was_live;
    uint8_t collected;
};

struct target {
    uint8_t index;
    uint8_t kind;
    uint8_t npc_weapon;
    uint8_t bound;
    uint16_t model;
    uint16_t pad;
    uint8_t nobjects;
    struct tracked objects[MAX_OBJECTS];
};

struct inv_entry {
    uint32_t ptr;
    int32_t type;
    uint32_t value;
};

static uint32_t s_last_screen = 0xFFFFFFFFu - 1u; /* Lua: -1, which no read equals */
static uint32_t s_last_mission = 0xFFFFFFFFu - 1u;
static uint32_t s_last_bond = 0xFFFFFFFFu - 1u;
static uint32_t s_prev_screen = 0xFFFFFFFFu - 1u;
static int s_have_last_timer;
static uint32_t s_last_timer_ec;
static uint32_t s_ec_pulse_count;
static int s_startup_ready_sent;
static uint32_t s_mission_attempt;

static struct target s_targets[MAX_TARGETS];
static uint32_t s_ntargets;
static int s_have_signature;
static uint32_t s_sig_count;
static uint8_t s_sig_bytes[MAX_PACKED];

static int s_have_tank;
static uint32_t s_tank_obj, s_tank_prop;
static uint8_t s_tank_enabled;

static struct inv_entry s_inv[MAX_INV];
static uint32_t s_ninv;

/* Lua 359-364. */
static void reset_state(void)
{
    s_have_last_timer = 0;
    s_ec_pulse_count = 0u;
    s_startup_ready_sent = 0;
}

static uint32_t get_bonddata_base(void) { return to_rdram(r32(BONDDATA_PTR_ADDR)); }

/* Lua 374-379. */
static void set_result_bit(uint32_t index)
{
    uint32_t result = r32(FREESTANDING_RESULT_MAILBOX_ADDR);
    if (!((result >> index) & 1u)) {
        w32(FREESTANDING_RESULT_MAILBOX_ADDR, result | (1u << index));
    }
}

/* Lua 381-396. */
static int unlink_inventory_entry(uint32_t bond, uint32_t entry)
{
    uint32_t next = to_rdram(r32(entry + 0x0Cu));
    uint32_t prev = to_rdram(r32(entry + 0x10u));
    uint32_t head = to_rdram(r32(bond + OFF_INV_HEAD));
    uint32_t off;
    if (next == 0u || prev == 0u) {
        return 0;
    }
    if (next == entry && prev == entry) {
        w32(bond + OFF_INV_HEAD, 0u);
    } else {
        w32(prev + 0x0Cu, 0x80000000u + next);
        w32(next + 0x10u, 0x80000000u + prev);
        if (head == entry) {
            w32(bond + OFF_INV_HEAD, 0x80000000u + next);
        }
    }
    w32(entry, (uint32_t)INV_ITEM_NONE);
    for (off = 4u; off <= INV_ITEM_SIZE - 4u; off += 4u) {
        w32(entry + off, 0u);
    }
    return 1;
}

/* Lua 398-412: Bond's inventory, a circular list, into s_inv. */
static void inventory_entries(uint32_t bond)
{
    uint32_t head = to_rdram(r32(bond + OFF_INV_HEAD));
    uint32_t pool = to_rdram(r32(bond + OFF_INV_POOL));
    uint32_t max_items = r32(bond + OFF_INV_MAX);
    uint32_t current, i;
    s_ninv = 0u;
    if (head == 0u || pool == 0u || max_items == 0u || max_items > 128u) {
        return;
    }
    current = head;
    while (current > 0u && s_ninv < MAX_INV) {
        for (i = 0u; i < s_ninv; i++) {
            if (s_inv[i].ptr == current) {
                return; /* seen: the list has come round */
            }
        }
        s_inv[s_ninv].ptr = current;
        s_inv[s_ninv].type = rs32(current);
        s_inv[s_ninv].value = r32(current + 4u);
        s_ninv++;
        current = to_rdram(r32(current + 0x0Cu));
    }
}

/* Lua 414-423. */
static int prop_is_onscreen(uint32_t prop)
{
    uint32_t count = r32(ONSCREEN_PROP_COUNT);
    uint32_t i;
    if (count > 512u) {
        return 0;
    }
    for (i = 0u; i < count; i++) {
        if (to_rdram(r32(ONSCREEN_PROP_LIST + i * 4u)) == prop) {
            return 1;
        }
    }
    return 0;
}

/* Lua 425-431. */
static void enable_proxy_collection(uint32_t obj)
{
    uint32_t flags = r32(obj + OBJ_OFF_FLAGS);
    flags &= ~PROPFLAG_00080000;
    flags &= ~PROPFLAG_UNCOLLECTABLE;
    flags &= ~PROPFLAG_CANNOT_ACTIVATE;
    w32(obj + OBJ_OFF_FLAGS, flags);
}

/* Lua 433-446. */
static void clear_token_hand(uint32_t bond)
{
    uint32_t hand = bond + OFF_HANDS_BASE;
    w32(hand + HAND_OFF_WEAPONNUM, 0u);
    w32(hand + HAND_OFF_WEAPONNUM_WATCH, (uint32_t)-1);
    w32(hand + HAND_OFF_CURRENT_ANIM, 0u);
    w32(hand + HAND_OFF_AMMO_IN_MAG, 0u);
    w32(hand + HAND_OFF_NEXT_WEAPON, 0u);
    w32(hand + HAND_OFF_ANIM_TRIGGER, 0u);
    w32(bond + OFF_HAND_INVISIBLE, 0u);
    w32(bond + OFF_HAND_ITEM, 0u);
    w32(bond + OFF_FIELD_2A44, (uint32_t)INV_ITEM_NONE);
    w32(bond + OFF_LOCK_HAND_MODEL, 0u);
    w32(bond + OFF_EQUIP_CUR, 0u);
}

/* Lua 448-484: the world's ROM publishes up to 32 pickup targets as 28-bit descriptors,
   packed little-end first; decode them only when the published list changes. */
static void rebuild_freestanding_targets(void)
{
    uint32_t count = r32(FREESTANDING_TARGET_COUNT_ADDR);
    uint32_t nbytes, i, bit;
    uint8_t packed[MAX_PACKED];
    if (count > MAX_TARGETS) {
        count = 0u;
    }
    nbytes = (count * FREESTANDING_TARGET_BITS + 7u) / 8u;
    for (i = 0u; i < nbytes; i++) {
        packed[i] = r8(FREESTANDING_TARGET_LIST_ADDR + i);
    }
    if (s_have_signature && s_sig_count == count) {
        for (i = 0u; i < nbytes && packed[i] == s_sig_bytes[i]; i++) {
        }
        if (i == nbytes) {
            return;
        }
    }
    s_have_signature = 1;
    s_sig_count = count;
    for (i = 0u; i < nbytes; i++) {
        s_sig_bytes[i] = packed[i];
    }
    s_ntargets = 0u;
    for (i = 0u; i < count; i++) {
        uint32_t d = 0u;
        struct target *t = &s_targets[s_ntargets++];
        for (bit = 0u; bit < FREESTANDING_TARGET_BITS; bit++) {
            uint32_t pb = i * FREESTANDING_TARGET_BITS + bit;
            d |= (uint32_t)((packed[pb / 8u] >> (pb % 8u)) & 1u) << bit;
        }
        t->index = (uint8_t)i;
        t->kind = (uint8_t)(((d >> 26) & 3u) + 1u);
        t->npc_weapon = (uint8_t)((d >> 25) & 1u);
        t->model = (uint16_t)((d >> 16) & 0x1FFu);
        t->pad = (uint16_t)(d & 0xFFFFu);
        t->bound = 0;
        t->nobjects = 0;
    }
}

/* Lua 486-519: find the props each unbound target came from; a target is bound once any
   matched, after the whole scan, so a grouped source keeps every match. */
static void bind_preplaced_freestanding_objects(void)
{
    uint32_t i, j;
    int all_bound = 1;
    for (j = 0u; j < s_ntargets; j++) {
        if (!s_targets[j].bound) {
            all_bound = 0;
        }
    }
    if (all_bound) {
        return;
    }
    for (i = 0u; i < PROP_MAX; i++) {
        uint32_t prop = PROP_POOL + i * PROP_SIZE;
        uint32_t obj = to_rdram(r32(prop + PROP_OFF_OBJ));
        uint8_t prop_type = r8(prop);
        uint32_t model_pad, model, pad, weapon_number;
        int guard_gun;
        if (obj == 0u || (prop_type != 1u && prop_type != 4u)
            || to_rdram(r32(obj + OBJ_OFF_PROP)) != prop) {
            continue;
        }
        model_pad = r32(obj + OBJ_OFF_MODEL_PAD);
        model = (model_pad >> 16) & 0xFFFFu;
        pad = model_pad & 0xFFFFu;
        weapon_number = r8(obj + WEP_OFF_WEAPONNUM);
        guard_gun = r8(obj + OBJ_OFF_TYPE) == 8u
            && (weapon_number < 33u || weapon_number == ITEM_TOKEN)
            && (r32(obj + OBJ_OFF_FLAGS) & 0x4000u) != 0u;
        for (j = 0u; j < s_ntargets; j++) {
            struct target *t = &s_targets[j];
            if (!t->bound && model == t->model && pad == t->pad
                && guard_gun == (int)t->npc_weapon && t->nobjects < MAX_OBJECTS) {
                struct tracked *o = &t->objects[t->nobjects++];
                o->obj = obj;
                o->prop = prop;
                o->was_live = 0;
                o->collected = 0;
            }
        }
    }
    for (j = 0u; j < s_ntargets; j++) {
        if (s_targets[j].nobjects > 0u) {
            s_targets[j].bound = 1;
        }
    }
}

/* Lua 530-560: show the mission's tank only once the player owns it. */
static void update_tank_visibility(void)
{
    uint32_t descriptor = r32(TANK_DESCRIPTOR_ADDR);
    uint8_t flags, enabled, desired;
    uint32_t i;
    if (descriptor == 0u) {
        return;
    }
    if (!s_have_tank) {
        for (i = 0u; i < PROP_MAX; i++) {
            uint32_t prop = PROP_POOL + i * PROP_SIZE;
            uint32_t obj = to_rdram(r32(prop + PROP_OFF_OBJ));
            if (obj > 0u && r8(obj + OBJ_OFF_TYPE) == 45u && r32(obj + OBJ_OFF_MODEL_PAD) == descriptor
                && to_rdram(r32(obj + OBJ_OFF_PROP)) == prop) {
                s_have_tank = 1;
                s_tank_obj = obj;
                s_tank_prop = prop;
                s_tank_enabled = r8(prop + 1u) & 4u;
                break;
            }
        }
    }
    if (!s_have_tank) {
        return;
    }
    if (to_rdram(r32(s_tank_obj + OBJ_OFF_PROP)) != s_tank_prop
        || to_rdram(r32(s_tank_prop + PROP_OFF_OBJ)) != s_tank_obj) {
        s_have_tank = 0;
        return;
    }
    flags = r8(s_tank_prop + 1u);
    enabled = r8(TANK_OWNED_ADDR) == 1u ? s_tank_enabled : 0u;
    desired = (uint8_t)((flags & ~4u) | enabled);
    if (flags != desired) {
        w8(s_tank_prop + 1u, desired);
    }
}

/* Lua 562-620. */
static void update_freestanding_pickups(uint32_t bond)
{
    uint32_t token_entry = 0u, i, j, k;
    int token_in_hand;
    rebuild_freestanding_targets();
    bind_preplaced_freestanding_objects();
    inventory_entries(bond);
    for (i = 0u; i < s_ninv; i++) {
        if (s_inv[i].type == INV_ITEM_WEAPON && s_inv[i].value == ITEM_TOKEN) {
            token_entry = s_inv[i].ptr; /* the last one, as the Lua's loop leaves it */
        }
    }
    token_in_hand = r32(bond + OFF_HANDS_BASE + HAND_OFF_WEAPONNUM) == ITEM_TOKEN
        || r32(bond + OFF_EQUIP_CUR) == ITEM_TOKEN;

    for (j = 0u; j < s_ntargets; j++) {
        struct target *t = &s_targets[j];
        for (k = 0u; k < t->nobjects; k++) {
            struct tracked *o = &t->objects[k];
            uint32_t live_prop = to_rdram(r32(o->obj + OBJ_OFF_PROP));
            if (o->obj == 0u) {
                continue;
            }
            if (live_prop > 0u) {
                /* Native armor and ammunition keep their own hooks; only weapons use the
                   token. A configured NPC keeps its weapon until it drops it. */
                int weapon_source = r8(o->obj + OBJ_OFF_TYPE) == 8u;
                if (weapon_source && prop_is_onscreen(live_prop)
                    && (!t->npc_weapon || r32(live_prop + PROP_OFF_PARENT) == 0u)) {
                    w8(o->obj + WEP_OFF_WEAPONNUM, (uint8_t)ITEM_TOKEN);
                    enable_proxy_collection(o->obj);
                    o->was_live = 1;
                }
            }
            if (t->kind == 2u && o->prop > 0u && !o->collected) {
                uint32_t player_prop = to_rdram(r32(bond + OFF_PLAYER_PROP));
                uint32_t parent_prop = to_rdram(r32(o->prop + PROP_OFF_PARENT));
                for (i = 0u; i < s_ninv; i++) {
                    if (s_inv[i].type == INV_ITEM_PROP && to_rdram(s_inv[i].value) == o->prop) {
                        set_result_bit(t->index);
                        unlink_inventory_entry(bond, s_inv[i].ptr);
                        o->collected = 1;
                    }
                }
                /* Native key and mission-item pickups parent the exact setup prop to Bond
                   even when no inventory node survives to this point. */
                if (!o->collected && player_prop > 0u && parent_prop == player_prop) {
                    set_result_bit(t->index);
                    o->collected = 1;
                }
            } else if (o->was_live && live_prop == 0u && (token_entry || token_in_hand)) {
                set_result_bit(t->index);
                o->was_live = 0;
            }
        }
    }
    if (token_entry) {
        unlink_inventory_entry(bond, token_entry);
    }
    if (token_in_hand) {
        clear_token_hand(bond);
    }
}

/* Lua 622-687: event.onframestart. */
void game_frame_start(void)
{
    uint32_t screen = r32(SCREEN_ID_ADDR);
    uint32_t mission = r32(MISSION_ID_ADDR);
    uint32_t bond = get_bonddata_base();
    uint32_t timer_ec;

    /* If the screen, the mission or Bond's data ever changes, start over. */
    if (screen != s_last_screen || mission != s_last_mission || bond != s_last_bond) {
        reset_state();
        s_ntargets = 0u;
        s_have_signature = 0;
        s_have_tank = 0;
        if (screen == SCREEN_GAMEPLAY && mission != 0u && bond > 0u) {
            /* Publish only after native RAM is initialized, like the other mailboxes. */
            w32(STARTUP_READY_MAILBOX_ADDR, 0u);
            w32(FREESTANDING_RESULT_MAILBOX_ADDR, 0u);
            s_mission_attempt++;
            w32(MISSION_ATTEMPT_ADDR, s_mission_attempt);
            w32(0x7F21Cu, 0u);
            w32(0x7F220u, 0u);
            w32(0x7F224u, 0u); /* Auto Fail belongs only to this mission attempt. */
            w32(KEY_ITEM_FLAGS_MAILBOX_ADDR, 0u);
            w32(KEY_ITEM_STATE_MAILBOX_ADDR, 0u);
            w8(0x7F20Bu, 0u); /* Intro skipping defaults off until slot options arrive. */
            w8(0x7F209u, 0u); /* Native item acquisition stays off until slot options arrive. */
            w32(0x7F20Cu, 0u); /* Guard ammunition acquisitions belong only to this attempt. */
            w8(TANK_OWNED_ADDR, 0u);
            w32(TANK_DESCRIPTOR_ADDR, 0u);
        }
    }
    s_last_screen = screen;
    s_last_mission = mission;
    s_last_bond = bond;

    /* Let the player read both debrief pages before returning to the map; leaving either
       must not advance straight into a mission the world has locked. */
    if ((s_prev_screen == SCREEN_DEBRIEF_1 || s_prev_screen == SCREEN_DEBRIEF_2)
        && screen != SCREEN_DEBRIEF_1 && screen != SCREEN_DEBRIEF_2 && screen != SCREEN_MAP) {
        w32(SCREEN_ID_ADDR, SCREEN_MAP);
    }
    s_prev_screen = screen;

    if (screen != SCREEN_GAMEPLAY || mission == 0u || bond == 0u) {
        return;
    }

    timer_ec = r32(bond + OFF_HANDS_BASE + HAND_OFF_TIMER_EC);
    if ((!s_have_last_timer || s_last_timer_ec != TARGET_EC_VALUE) && timer_ec == TARGET_EC_VALUE) {
        s_ec_pulse_count++;
    }
    if (s_ec_pulse_count >= TARGET_PULSE_COUNT && !s_startup_ready_sent) {
        w32(STARTUP_READY_MAILBOX_ADDR, 1u);
        s_startup_ready_sent = 1;
    }

    /* Pickup inventory evidence is transient, so consume it before the game's frame. */
    update_tank_visibility();
    update_freestanding_pickups(bond);
    s_last_timer_ec = timer_ec;
    s_have_last_timer = 1;
}

/* Lua 689-704: event.onframeend. */
void game_frame_end(void)
{
    uint32_t screen = r32(SCREEN_ID_ADDR);
    uint32_t mission = r32(MISSION_ID_ADDR);
    uint32_t bond = get_bonddata_base();
    if (screen == SCREEN_GAMEPLAY && mission != 0u && bond > 0u) {
        uint32_t key_flags = r32(KEY_ITEM_FLAGS_MAILBOX_ADDR);
        uint32_t key_state = r32(KEY_ITEM_STATE_MAILBOX_ADDR);
        uint32_t key_mission = (key_state >> 8) & 0xFFu;
        if ((key_flags != 0u || key_state != 0u) && key_mission != mission) {
            w32(KEY_ITEM_FLAGS_MAILBOX_ADDR, 0u);
            w32(KEY_ITEM_STATE_MAILBOX_ADDR, 0u);
        }
        /* The token is equipped during the game's frame, before this point. */
        update_freestanding_pickups(bond);
    }
}
