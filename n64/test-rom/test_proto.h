/**
 * L3 APPLICATION test profile (M64T) — see docs/spec/test-l3-application-v0.md
 */
#ifndef MULTI64_TEST_PROTO_H
#define MULTI64_TEST_PROTO_H

#include <stddef.h>
#include <stdint.h>

#define M64T_MAGIC0 0x4DU
#define M64T_MAGIC1 0x36U
#define M64T_MAGIC2 0x34U
#define M64T_MAGIC3 0x54U

#define M64T_MSG_PING 0x01U
#define M64T_MSG_ECHO 0x02U
#define M64T_MSG_REQ_VERSION 0x03U
#define M64T_MSG_REQ_CONTROLLER 0x04U
#define M64T_MSG_SESSION_OPEN 0x05U
#define M64T_MSG_SESSION_CLOSE 0x06U
#define M64T_MSG_REQ_EEPROM_INFO 0x07U
#define M64T_MSG_REQ_EEPROM_READ 0x08U
#define M64T_MSG_REQ_EEPROM_WRITE 0x09U
#define M64T_MSG_REQ_SRAM_INFO 0x0AU
#define M64T_MSG_REQ_SRAM_READ 0x0BU
#define M64T_MSG_REQ_SRAM_WRITE 0x0CU
#define M64T_MSG_REQ_RUMBLE 0x0DU
#define M64T_MSG_REQ_DISPLAY_TEXT 0x0EU

#define M64T_MSG_PONG 0x81U
#define M64T_MSG_ECHO_REPLY 0x82U
#define M64T_MSG_VERSION 0x83U
#define M64T_MSG_CONTROLLER 0x84U
#define M64T_MSG_SESSION_ACK 0x85U
#define M64T_MSG_SESSION_END 0x86U
#define M64T_MSG_EEPROM_INFO 0x87U
#define M64T_MSG_EEPROM_DATA 0x88U
#define M64T_MSG_EEPROM_STATUS 0x89U
#define M64T_MSG_SRAM_INFO 0x8AU
#define M64T_MSG_SRAM_DATA 0x8BU
#define M64T_MSG_SRAM_STATUS 0x8CU
#define M64T_MSG_RUMBLE_ACK 0x8DU
#define M64T_MSG_DISPLAY_TEXT_ACK 0x8EU
#define M64T_MSG_BENCH_TICK 0xF0U
/** Cart → host: left CONTROLLER_POLL mode (L+R held ~5s); body empty. */
#define M64T_MSG_CONTROLLER_POLL_EXIT 0xF1U
/** Cart → host: large body stress (pattern-filled). */
#define M64T_MSG_STRESS_LARGE 0xE1U

#define TEST_RX_CAP 16384
#define TEST_USB_CHUNK 8192
/** Single L3 wire buffer (header + max APPLICATION payload). */
#define TEST_L3_OUT_CAP 10240
/** Max bytes in one `usb_write` (libdragon / SC64 typical cap). */
#define TEST_USB_WRITE_MAX 8192
/** Chunk size for deliberate multi-write fragmentation. */
#define TEST_USB_FRAG_CHUNK 4096

/** `M64T_MSG_EEPROM_STATUS` / `M64T_MSG_SRAM_STATUS` body[0]. */
#define M64T_SAVE_STATUS_OK 0U
#define M64T_SAVE_ERR_SESSION 1U
#define M64T_SAVE_ERR_PARAM 2U
#define M64T_SAVE_ERR_NO_HW 3U

/** `M64T_MSG_RUMBLE_ACK` body[2] when body[2] != 0. */
#define M64T_RUMBLE_ERR_UNSUPPORTED 1U
#define M64T_RUMBLE_ERR_PARAM 2U

/** `M64T_MSG_DISPLAY_TEXT_ACK` body[0]. */
#define M64T_DISPLAY_TEXT_OK 0U
#define M64T_DISPLAY_TEXT_ERR_TOO_LONG 1U

/** Max bytes in `REQ_DISPLAY_TEXT` body (UTF-8); excludes M64T header. */
#define TEST_HOST_DISPLAY_MAX 120

#define TEST_ROM_VERSION_STR "multi64-test-rom 1.8"

/** Clears RX buffer, M64T total, and diag counters (R / mode change). */
void test_proto_reset_all(void);
/** Clears overflow/resync/bad-frame counters only. */
void test_proto_reset_diag(void);
void test_proto_rx_append(const uint8_t *p, int n);
/** Drain reassembled L3 stream; returns number of APPLICATION frames handled. */
unsigned test_proto_drain_stream(void);

uint32_t test_proto_get_frames_handled(void);
uint32_t test_proto_get_rx_overflow_count(void);
uint32_t test_proto_get_rx_resync_bytes(void);
uint32_t test_proto_get_bad_header_drops(void);

/** Session id from last `SESSION_OPEN` (`0` = none). */
uint32_t test_proto_get_session_id(void);

/** Active joypad port 0..3 for CONTROLLER snapshots. */
void test_proto_set_joypad_port(int port);
int test_proto_get_joypad_port(void);

/** Cart → host: unsolicited CONTROLLER snapshot (M64T `0x84`), includes port byte. */
void test_proto_send_controller_snapshot(void);

/** Cart → host: notify that CONTROLLER_POLL mode ended (`0xF1`, empty body). */
void test_proto_send_controller_poll_exit(void);

/** Cart → host: bench tick (M64T `0xF0` + uint32 BE counter). */
void test_proto_send_bench_tick(uint32_t tick);

/** Cart → host: ~8 KiB M64T STRESS_LARGE (`0xE1`). `fragmented`: split wire across multiple `usb_write`. */
void test_proto_send_stress_large(int fragmented);

/** L3 `HEARTBEAT` on Control channel (type 0x20, ch 0x02). */
void test_proto_send_l3_heartbeat(void);

/** L3 `DATA` on Log channel (type 0x10, ch 0x01) — non-APPLICATION path. */
void test_proto_send_l3_log_ping(void);

/** Per-frame rumble countdown (call from main loop). */
void test_proto_rumble_tick(void);

/**
 * Rumble on a controller port for `frames` VI frames (1..600). `frames == 0` uses 60.
 * Returns 0 on success, 1 if rumble unsupported, 2 if port invalid.
 */
int test_proto_rumble_pulse(int port, uint32_t frames);

/** NUL-terminated text for HUD (from `REQ_DISPLAY_TEXT`); may be empty. */
const char *test_proto_get_host_display_text(void);

#endif
