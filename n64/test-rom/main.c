/**
 * multi64 official test ROM — SummerCart64 + libdragon.
 *
 * Modes (pick from MODE MENU at boot or via menu hotkey):
 *   0 RAW L3 echo — verbatim MULTI64_L3 loopback (sc64-l3-framing-e2e, sc64-echo-test)
 *   1 M64T protocol — L3 DATA APPLICATION (test-l3-application-v0.md)
 *   2 Bench — M64T RX + BENCH_TICK every N frames (D-up/D-down) + extras below
 *   3 Controller poll — host sends REQ_CONTROLLER at its chosen rate; C-left/right = port 0–3.
 *      Hold L+R ~5s to quit to MODE MENU (host sees M64T 0xF1 CONTROLLER_POLL_EXIT).
 *
 * MODE MENU: D-up/down = move, A = enter mode (B and L ignored in menu).
 *   When running: L opens menu (except in CTRL_POLL). In CTRL_POLL, only L+R opens the menu (after hold).
 *
 * M64T / BENCH controls (not in mode 3):
 *   B = STRESS_LARGE one usb_write (or chunked if C-down held).
 *   C-left/right = active port 0–3. C-up = L3 DATA on Log channel. D-up/down = bench interval ±15 (15..600).
 *   Start = L3 HEARTBEAT (Control).
 *
 * R = reset stream, diag, session, host text (not in CTRL_POLL). Host: M64T per test-l3-application-v0.md.
 */
#include <libdragon.h>
#include <joypad.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <timer.h>
#include <usb.h>

#include "test_proto.h"

#define MULTI64_L3 0x01

#define USB_READ_CHUNK TEST_USB_CHUNK

enum run_mode {
    MODE_RAW_ECHO = 0,
    MODE_TEST_PROTO = 1,
    MODE_BENCH = 2,
    MODE_CONTROLLER_POLL = 3,
    MODE_COUNT = 4
};

static uint8_t s_pkt[USB_READ_CHUNK];

static uint32_t s_rx_bytes;
static uint32_t s_tx_bytes;
static uint32_t s_frame;
static enum run_mode s_mode = MODE_RAW_ECHO;
static uint32_t s_bench_interval_frames = 60u;

static uint32_t s_tick_min = 0xFFFFFFFFu;
static uint32_t s_tick_max;

/** MODE MENU overlay (1 = visible). */
static int s_menu_visible = 1;
/** Selected row 0..MODE_COUNT-1 */
static int s_menu_cursor;

/** ~5s at ~60 Hz VI — used for L+R exit from CONTROLLER_POLL. */
#define LR_EXIT_HOLD_FRAMES 300u

static uint32_t s_lr_exit_hold_frames;

#define BENCH_INTERVAL_MIN 15u
#define BENCH_INTERVAL_MAX 600u

static const char *mode_name(enum run_mode m)
{
    switch (m) {
    case MODE_RAW_ECHO:
        return "RAW_ECHO";
    case MODE_TEST_PROTO:
        return "M64T_PROTO";
    case MODE_BENCH:
        return "BENCH";
    case MODE_CONTROLLER_POLL:
        return "CTRL_POLL";
    default:
        return "?";
    }
}

static void read_exact(uint8_t *dst, size_t cap, int total)
{
    int left = total;
    while (left > 0) {
        int chunk = left > (int)cap ? (int)cap : left;
        usb_read(dst, chunk);
        left -= chunk;
    }
}

static void reset_stats(void)
{
    s_rx_bytes = 0;
    s_tx_bytes = 0;
    test_proto_reset_all();
}

static void hud_redraw(uint32_t frame_ticks)
{
    int port = test_proto_get_joypad_port();

    console_clear();
    printf("multi64 test ROM\n");

    if (s_menu_visible) {
        printf("---- MODE MENU ----\n");
        for (int i = 0; i < MODE_COUNT; i++) {
            printf("%s %d  %s\n", (i == s_menu_cursor) ? ">" : " ", i, mode_name((enum run_mode)i));
        }
        printf("-------------------\n");
        printf("D-up/down  A=enter\n");
        printf("running: %s\n", mode_name(s_mode));
        console_render();
        return;
    }

    if (s_mode == MODE_CONTROLLER_POLL) {
        printf("L+R ~5s -> MODE MENU\n");
    } else {
        printf("L=menu  R=reset\n");
    }
    printf("--------------------\n");
    {
        const char *host_txt = test_proto_get_host_display_text();
        if (host_txt[0] != '\0') {
            printf("host:\n%s\n", host_txt);
            printf("--------------------\n");
        }
    }
    printf("f    %u\n", (unsigned)s_frame);
    printf("mode %s\n", mode_name(s_mode));
    printf("tick last/min/max %lu / %lu / %lu\n", (unsigned long)frame_ticks, (unsigned long)s_tick_min,
           (unsigned long)s_tick_max);
    printf("rx   %lu\n", (unsigned long)s_rx_bytes);
    printf("ovfl %lu rsync %lu bad %lu\n", (unsigned long)test_proto_get_rx_overflow_count(),
           (unsigned long)test_proto_get_rx_resync_bytes(), (unsigned long)test_proto_get_bad_header_drops());
    if (s_mode == MODE_RAW_ECHO) {
        printf("tx   %lu\n", (unsigned long)s_tx_bytes);
        printf("     (other modes: M64T/BENCH/CTRL_POLL)\n");
    } else if (s_mode == MODE_CONTROLLER_POLL) {
        printf("m64t %lu\n", (unsigned long)test_proto_get_frames_handled());
        printf("ses  %lu\n", (unsigned long)test_proto_get_session_id());
        printf("port %d  (host REQ_CONTROLLER)\n", port);
        printf("C    port +/-\n");
        printf("L+R  ~5s -> MODE MENU\n");
    } else {
        printf("m64t %lu\n", (unsigned long)test_proto_get_frames_handled());
        printf("ses  %lu\n", (unsigned long)test_proto_get_session_id());
        printf("port %d  bench %uf\n", port, (unsigned)s_bench_interval_frames);
        printf("B    STRESS large\n");
        printf("     +C-down=chunk\n");
        printf("C-up LOG  Start HB\n");
        printf("D-up/down bench +/-\n");
    }
    console_render();
}

static void run_raw_echo(void)
{
    uint32_t hdr = usb_poll();
    if (hdr == 0) {
        return;
    }

    uint32_t type = USBHEADER_GETTYPE(hdr);
    int n = (int)USBHEADER_GETSIZE(hdr);

    if (n <= 0) {
        return;
    }

    read_exact(s_pkt, USB_READ_CHUNK, n);
    s_rx_bytes += (uint32_t)n;

    if (type == MULTI64_L3 && n <= (int)USB_READ_CHUNK) {
        usb_write(MULTI64_L3, s_pkt, n);
        s_tx_bytes += (uint32_t)n;
    }
}

static void run_m64t_usb_rx(void)
{
    uint32_t hdr = usb_poll();
    if (hdr != 0) {
        uint32_t type = USBHEADER_GETTYPE(hdr);
        int n = (int)USBHEADER_GETSIZE(hdr);

        if (n > 0) {
            read_exact(s_pkt, USB_READ_CHUNK, n);
            s_rx_bytes += (uint32_t)n;

            if (type == MULTI64_L3) {
                test_proto_rx_append(s_pkt, n);
                (void)test_proto_drain_stream();
            }
        }
    }
}

static void run_test_or_bench(void)
{
    run_m64t_usb_rx();

    if (s_mode == MODE_BENCH && s_bench_interval_frames > 0u && (s_frame % s_bench_interval_frames) == 0u) {
        test_proto_send_bench_tick(s_frame);
    }
}

static void run_controller_poll_mode(void)
{
    run_m64t_usb_rx();
}

static void proto_controller_poll(joypad_buttons_t pressed)
{
    if (pressed.c_left) {
        int p = test_proto_get_joypad_port() - 1;
        if (p < 0) {
            p = 3;
        }
        test_proto_set_joypad_port(p);
    }
    if (pressed.c_right) {
        int p = test_proto_get_joypad_port() + 1;
        if (p > 3) {
            p = 0;
        }
        test_proto_set_joypad_port(p);
    }
}

static void proto_m64t_bench(joypad_buttons_t pressed, joypad_buttons_t held)
{
    if (pressed.c_left) {
        int p = test_proto_get_joypad_port() - 1;
        if (p < 0) {
            p = 3;
        }
        test_proto_set_joypad_port(p);
    }
    if (pressed.c_right) {
        int p = test_proto_get_joypad_port() + 1;
        if (p > 3) {
            p = 0;
        }
        test_proto_set_joypad_port(p);
    }

    if (s_mode == MODE_BENCH) {
        if (pressed.d_up) {
            if (s_bench_interval_frames + 15u <= BENCH_INTERVAL_MAX) {
                s_bench_interval_frames += 15u;
            }
        }
        if (pressed.d_down) {
            if (s_bench_interval_frames > BENCH_INTERVAL_MIN + 15u) {
                s_bench_interval_frames -= 15u;
            } else {
                s_bench_interval_frames = BENCH_INTERVAL_MIN;
            }
        }
    }

    if (pressed.c_up) {
        test_proto_send_l3_log_ping();
    }
    if (pressed.start) {
        test_proto_send_l3_heartbeat();
    }

    if (pressed.b) {
        int frag = held.c_down ? 1 : 0;
        test_proto_send_stress_large(frag);
    }
}

static void menu_handle_input(joypad_buttons_t pressed)
{
    if (pressed.d_up) {
        s_menu_cursor--;
        if (s_menu_cursor < 0) {
            s_menu_cursor = MODE_COUNT - 1;
        }
    }
    if (pressed.d_down) {
        s_menu_cursor++;
        if (s_menu_cursor >= MODE_COUNT) {
            s_menu_cursor = 0;
        }
    }
    if (pressed.a) {
        s_mode = (enum run_mode)s_menu_cursor;
        reset_stats();
        s_menu_visible = 0;
    }
}

int main(void)
{
    console_init();
    console_set_render_mode(RENDER_AUTOMATIC);
    timer_init();
    joypad_init();

    printf("multi64 test ROM\n");
    printf("MODE MENU: D-pad + A to start\n");

    if (!usb_initialize()) {
        printf("usb init failed\n");
        while (1) {
        }
    }

    if (usb_getcart() != CART_SC64) {
        printf("Need SummerCart64\n");
        while (1) {
        }
    }

    printf("ready (menu open)\n");

    console_set_render_mode(RENDER_MANUAL);
    test_proto_set_joypad_port(0);
    s_menu_cursor = 0;

    for (;;) {
        uint32_t t0 = timer_ticks();

        joypad_poll();
        s_frame++;

        joypad_buttons_t pressed = joypad_get_buttons_pressed(JOYPAD_PORT_1);
        joypad_buttons_t held = joypad_get_buttons(JOYPAD_PORT_1);

        if (s_menu_visible) {
            menu_handle_input(pressed);
        } else {
            if (s_mode == MODE_CONTROLLER_POLL) {
                if (held.l && held.r) {
                    if (s_lr_exit_hold_frames < LR_EXIT_HOLD_FRAMES) {
                        s_lr_exit_hold_frames++;
                    }
                    if (s_lr_exit_hold_frames >= LR_EXIT_HOLD_FRAMES) {
                        test_proto_send_controller_poll_exit();
                        s_mode = MODE_TEST_PROTO;
                        s_lr_exit_hold_frames = 0;
                        reset_stats();
                        s_menu_visible = 1;
                        s_menu_cursor = (int)MODE_CONTROLLER_POLL;
                    }
                } else {
                    s_lr_exit_hold_frames = 0;
                }
            } else {
                s_lr_exit_hold_frames = 0;
            }

            if (s_mode != MODE_CONTROLLER_POLL) {
                if (pressed.l) {
                    s_menu_visible = 1;
                    s_menu_cursor = (int)s_mode;
                }
            }

            if (pressed.r && s_mode != MODE_CONTROLLER_POLL) {
                reset_stats();
            }

            if (!s_menu_visible) {
                if (s_mode == MODE_CONTROLLER_POLL) {
                    proto_controller_poll(pressed);
                } else if (s_mode == MODE_TEST_PROTO || s_mode == MODE_BENCH) {
                    proto_m64t_bench(pressed, held);
                }
            }
        }

        test_proto_rumble_tick();

        switch (s_mode) {
        case MODE_RAW_ECHO:
            run_raw_echo();
            break;
        case MODE_TEST_PROTO:
        case MODE_BENCH:
            run_test_or_bench();
            break;
        case MODE_CONTROLLER_POLL:
            run_controller_poll_mode();
            break;
        default:
            break;
        }

        uint32_t t1 = timer_ticks();
        uint32_t dt = t1 - t0;
        if (s_frame > 1u) {
            if (dt < s_tick_min) {
                s_tick_min = dt;
            }
            if (dt > s_tick_max) {
                s_tick_max = dt;
            }
        }

        hud_redraw(dt);
    }
}
