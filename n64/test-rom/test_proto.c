/**
 * L3 stream reassembly + APPLICATION handling: M64T (test-l3-application-v0.md)
 * and M64P (memory-l3-application-v0.md), dispatched on the payload magic.
 */
#include "test_proto.h"
#include "mem_proto.h"
#include "save_hw.h"

#include <joypad.h>
#include <n64sys.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <usb.h>

#define MULTI64_L3 0x01

#define L3_MAGIC0 0x4DU
#define L3_MAGIC1 0x36U
#define L3_MAGIC2 0x34U
#define L3_MAGIC3 0x42U
#define L3_TYPE_DATA 0x10U
#define L3_TYPE_HEARTBEAT 0x20U
#define L3_CH_APPLICATION 0x00U
#define L3_CH_LOG 0x01U
#define L3_CH_CONTROL 0x02U

#define M64T_APP_MAX 8187

static uint8_t s_rx[TEST_RX_CAP];
static size_t s_rx_len;

static uint32_t s_total_m64t_handled;
static uint32_t s_rx_overflow_count;
static uint32_t s_rx_resync_bytes;
static uint32_t s_bad_header_drops;

static int s_joypad_port;

static uint32_t s_session_id;
static uint8_t s_session_challenge[8];
static uint32_t s_next_session_id = 1U;

static uint32_t s_rumble_frames;
static int s_rumble_active_port = -1;

#define RUMBLE_MAX_FRAMES 600U

static void rumble_deactivate_current(void)
{
    if (s_rumble_active_port < 0 || s_rumble_active_port > 3) {
        return;
    }
    joypad_port_t jp = (joypad_port_t)s_rumble_active_port;
    if (joypad_get_rumble_supported(jp)) {
        joypad_set_rumble_active(jp, false);
    }
}

static void rumble_stop_all(void)
{
    rumble_deactivate_current();
    s_rumble_frames = 0U;
    s_rumble_active_port = -1;
}

int test_proto_rumble_pulse(int port, uint32_t frames)
{
    if (port < 0 || port > 3) {
        return 2;
    }
    if (frames == 0U) {
        frames = 60U;
    }
    if (frames > RUMBLE_MAX_FRAMES) {
        frames = RUMBLE_MAX_FRAMES;
    }
    joypad_port_t jp = (joypad_port_t)port;
    if (!joypad_get_rumble_supported(jp)) {
        return 1;
    }
    if (s_rumble_frames > 0U && s_rumble_active_port >= 0 && s_rumble_active_port != port) {
        rumble_deactivate_current();
    }
    joypad_set_rumble_active(jp, true);
    s_rumble_active_port = port;
    s_rumble_frames = frames;
    return 0;
}

void test_proto_rumble_tick(void)
{
    if (s_rumble_frames == 0U || s_rumble_active_port < 0) {
        return;
    }
    s_rumble_frames--;
    if (s_rumble_frames == 0U) {
        joypad_port_t jp = (joypad_port_t)s_rumble_active_port;
        if (joypad_get_rumble_supported(jp)) {
            joypad_set_rumble_active(jp, false);
        }
        s_rumble_active_port = -1;
    }
}

static uint32_t read_be32(const uint8_t *p)
{
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | (uint32_t)p[3];
}

static void put_be32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)((v >> 24) & 0xff);
    p[1] = (uint8_t)((v >> 16) & 0xff);
    p[2] = (uint8_t)((v >> 8) & 0xff);
    p[3] = (uint8_t)(v & 0xff);
}

static uint16_t read_be16(const uint8_t *p)
{
    return (uint16_t)(((uint16_t)p[0] << 8) | (uint16_t)p[1]);
}

static void put_be16(uint8_t *p, uint16_t v)
{
    p[0] = (uint8_t)((v >> 8) & 0xff);
    p[1] = (uint8_t)(v & 0xff);
}

/** L3 v1 frame: 16-byte header + payload. */
static int build_l3_frame(uint8_t ftype, uint8_t channel, const uint8_t *payload, int payload_len, uint8_t *out,
                          int out_cap)
{
    int total = 16 + payload_len;
    if (total > out_cap || payload_len < 0) {
        return -1;
    }
    out[0] = L3_MAGIC0;
    out[1] = L3_MAGIC1;
    out[2] = L3_MAGIC2;
    out[3] = L3_MAGIC3;
    out[4] = ftype;
    out[5] = channel;
    out[6] = 0;
    out[7] = 0x01;
    out[8] = 0;
    out[9] = 0;
    out[10] = 0;
    out[11] = 0;
    out[12] = (uint8_t)(((uint32_t)payload_len >> 24) & 0xff);
    out[13] = (uint8_t)(((uint32_t)payload_len >> 16) & 0xff);
    out[14] = (uint8_t)(((uint32_t)payload_len >> 8) & 0xff);
    out[15] = (uint8_t)((uint32_t)payload_len & 0xff);
    if (payload_len > 0 && payload != NULL) {
        memcpy(out + 16, payload, (size_t)payload_len);
    }
    return total;
}

static void send_l3_wire_once(const uint8_t *wire, int len)
{
    if (len <= 0) {
        return;
    }
    usb_write(MULTI64_L3, wire, (size_t)len);
}

static void send_l3_wire_chunked(const uint8_t *wire, int total_len)
{
    int off = 0;
    while (off < total_len) {
        int chunk = total_len - off;
        if (chunk > (int)TEST_USB_FRAG_CHUNK) {
            chunk = (int)TEST_USB_FRAG_CHUNK;
        }
        usb_write(MULTI64_L3, wire + off, (size_t)chunk);
        off += chunk;
    }
}

static int send_l3_application_payload(const uint8_t *app, int app_len, uint8_t *out, int out_cap)
{
    return build_l3_frame(L3_TYPE_DATA, L3_CH_APPLICATION, app, app_len, out, out_cap);
}

static void send_l3_app(const uint8_t *app, int app_len)
{
    static uint8_t out[TEST_L3_OUT_CAP];
    int nw = send_l3_application_payload(app, app_len, out, (int)sizeof(out));
    if (nw <= 0) {
        return;
    }
    send_l3_wire_once(out, nw);
}

/*
 * Hooks mem_proto.c declares. Keeping them here is what lets that file stay free
 * of libdragon, so the same source can move into a game-resident agent.
 */
void m64p_transport_send(const uint8_t *app, int app_len)
{
    send_l3_app(app, app_len);
}

uint32_t m64p_rdram_size(void)
{
    return (uint32_t)get_memory_size();
}

volatile uint8_t *m64p_rdram_base(void)
{
    /* Cached KSEG0: the game touches these structures with the CPU, so cached
       access is what stays coherent. See memory-l3-application-v0.md 4.1. */
    return (volatile uint8_t *)0x80000000U;
}

static void m64t_send(uint8_t msg, const uint8_t *body, size_t blen)
{
    uint8_t app[5 + 520];
    if (blen + 5U > sizeof(app)) {
        return;
    }
    app[0] = M64T_MAGIC0;
    app[1] = M64T_MAGIC1;
    app[2] = M64T_MAGIC2;
    app[3] = M64T_MAGIC3;
    app[4] = msg;
    if (blen > 0U && body != NULL) {
        memcpy(app + 5, body, blen);
    }
    send_l3_app(app, (int)(5U + blen));
}

static void m64t_send_status(uint8_t msg, uint8_t code)
{
    m64t_send(msg, &code, 1U);
}

void test_proto_send_l3_heartbeat(void)
{
    static uint8_t out[64];
    int nw = build_l3_frame(L3_TYPE_HEARTBEAT, L3_CH_CONTROL, NULL, 0, out, (int)sizeof(out));
    if (nw <= 0) {
        return;
    }
    send_l3_wire_once(out, nw);
}

void test_proto_send_l3_log_ping(void)
{
    static const char msg[] = "log_ping";
    static uint8_t out[64];
    int nw = build_l3_frame(L3_TYPE_DATA, L3_CH_LOG, (const uint8_t *)msg, (int)(sizeof(msg) - 1U), out,
                            (int)sizeof(out));
    if (nw <= 0) {
        return;
    }
    send_l3_wire_once(out, nw);
}

void test_proto_set_joypad_port(int port)
{
    if (port < 0) {
        port = 0;
    }
    if (port > 3) {
        port = 3;
    }
    s_joypad_port = port;
}

int test_proto_get_joypad_port(void)
{
    return s_joypad_port;
}

void test_proto_send_controller_snapshot(void)
{
    joypad_port_t port = (joypad_port_t)s_joypad_port;
    joypad_inputs_t ji = joypad_get_inputs(port);
    uint8_t body[9];
    put_be32(body, (uint32_t)ji.btn.raw);
    body[4] = (uint8_t)ji.stick_x;
    body[5] = (uint8_t)ji.stick_y;
    body[6] = 0;
    body[7] = 0;
    body[8] = (uint8_t)(unsigned)s_joypad_port;

    uint8_t app[5 + 9];
    app[0] = M64T_MAGIC0;
    app[1] = M64T_MAGIC1;
    app[2] = M64T_MAGIC2;
    app[3] = M64T_MAGIC3;
    app[4] = M64T_MSG_CONTROLLER;
    memcpy(app + 5, body, sizeof(body));
    send_l3_app(app, (int)sizeof(app));
}

void test_proto_send_controller_poll_exit(void)
{
    uint8_t app[5];
    app[0] = M64T_MAGIC0;
    app[1] = M64T_MAGIC1;
    app[2] = M64T_MAGIC2;
    app[3] = M64T_MAGIC3;
    app[4] = M64T_MSG_CONTROLLER_POLL_EXIT;
    send_l3_app(app, 5);
}

void test_proto_send_stress_large(int fragmented)
{
    static uint8_t app[5 + M64T_APP_MAX];
    size_t i;
    app[0] = M64T_MAGIC0;
    app[1] = M64T_MAGIC1;
    app[2] = M64T_MAGIC2;
    app[3] = M64T_MAGIC3;
    app[4] = M64T_MSG_STRESS_LARGE;
    for (i = 0; i < (size_t)M64T_APP_MAX; i++) {
        app[5 + i] = (uint8_t)(i & 0xFF);
    }

    static uint8_t wire[TEST_L3_OUT_CAP];
    int nw = send_l3_application_payload(app, (int)(5 + M64T_APP_MAX), wire, (int)sizeof(wire));
    if (nw <= 0) {
        return;
    }
    if (fragmented) {
        send_l3_wire_chunked(wire, nw);
    } else {
        if (nw > (int)TEST_USB_WRITE_MAX) {
            send_l3_wire_chunked(wire, nw);
        } else {
            send_l3_wire_once(wire, nw);
        }
    }
}

static char s_host_display[TEST_HOST_DISPLAY_MAX + 1];

static void host_display_clear(void)
{
    s_host_display[0] = '\0';
}

static void host_display_set(const uint8_t *p, size_t n)
{
    size_t i;
    if (n > (size_t)TEST_HOST_DISPLAY_MAX) {
        n = (size_t)TEST_HOST_DISPLAY_MAX;
    }
    for (i = 0; i < n; i++) {
        uint8_t c = p[i];
        if (c == '\n' || c == '\t') {
            s_host_display[i] = (char)c;
        } else if (c < 0x20u || c == 0x7fu) {
            s_host_display[i] = '?';
        } else {
            s_host_display[i] = (char)c;
        }
    }
    s_host_display[n] = '\0';
}

const char *test_proto_get_host_display_text(void)
{
    return s_host_display;
}

static void handle_m64t(const uint8_t *p, size_t plen)
{
    if (plen < 5U) {
        return;
    }
    if (p[0] != M64T_MAGIC0 || p[1] != M64T_MAGIC1 || p[2] != M64T_MAGIC2 || p[3] != M64T_MAGIC3) {
        return;
    }

    uint8_t msg = p[4];
    size_t body_len = plen - 5U;
    const uint8_t *body = p + 5;

    if (msg == M64T_MSG_PING) {
        uint8_t app[5];
        app[0] = M64T_MAGIC0;
        app[1] = M64T_MAGIC1;
        app[2] = M64T_MAGIC2;
        app[3] = M64T_MAGIC3;
        app[4] = M64T_MSG_PONG;
        send_l3_app(app, 5);
        return;
    }

    if (msg == M64T_MSG_ECHO) {
        static uint8_t app[5 + M64T_APP_MAX];
        if (5U + body_len > sizeof(app)) {
            return;
        }
        app[0] = M64T_MAGIC0;
        app[1] = M64T_MAGIC1;
        app[2] = M64T_MAGIC2;
        app[3] = M64T_MAGIC3;
        app[4] = M64T_MSG_ECHO_REPLY;
        if (body_len > 0U) {
            memcpy(app + 5, body, body_len);
        }
        send_l3_app(app, (int)(5U + body_len));
        return;
    }

    if (msg == M64T_MSG_REQ_VERSION) {
        const char *ver = TEST_ROM_VERSION_STR;
        size_t vlen = strlen(ver);
        uint8_t app[5 + 64];
        if (5 + vlen > sizeof(app)) {
            return;
        }
        app[0] = M64T_MAGIC0;
        app[1] = M64T_MAGIC1;
        app[2] = M64T_MAGIC2;
        app[3] = M64T_MAGIC3;
        app[4] = M64T_MSG_VERSION;
        memcpy(app + 5, ver, vlen);
        send_l3_app(app, (int)(5 + vlen));
        return;
    }

    if (msg == M64T_MSG_REQ_CONTROLLER) {
        test_proto_send_controller_snapshot();
        return;
    }

    if (msg == M64T_MSG_SESSION_OPEN) {
        uint8_t echo[8];
        memset(echo, 0, sizeof(echo));
        if (body_len >= 8U) {
            memcpy(echo, body, 8U);
        } else if (body_len > 0U) {
            memcpy(echo, body, body_len);
        }
        if (s_next_session_id == 0U) {
            s_next_session_id = 1U;
        }
        s_session_id = s_next_session_id++;
        memcpy(s_session_challenge, echo, sizeof(echo));
        {
            uint8_t ack[16];
            memcpy(ack, echo, 8U);
            put_be32(ack + 8U, s_session_id);
            put_be32(ack + 12U, 1U);
            m64t_send(M64T_MSG_SESSION_ACK, ack, sizeof(ack));
        }
        return;
    }

    if (msg == M64T_MSG_SESSION_CLOSE) {
        s_session_id = 0U;
        memset(s_session_challenge, 0, sizeof(s_session_challenge));
        m64t_send(M64T_MSG_SESSION_END, NULL, 0);
        return;
    }

    if (msg == M64T_MSG_REQ_EEPROM_INFO) {
        save_hw_eeprom_info_t inf;
        uint8_t b[4];
        save_hw_eeprom_get_info(&inf);
        b[0] = inf.eeprom_type;
        put_be16(b + 1U, inf.eeprom_blocks);
        b[3] = 0;
        m64t_send(M64T_MSG_EEPROM_INFO, b, sizeof(b));
        return;
    }

    if (msg == M64T_MSG_REQ_EEPROM_READ) {
        static uint8_t __attribute__((aligned(8))) ebuf[256];
        uint16_t off;
        uint16_t elen;
        if (body_len < 4U) {
            m64t_send_status(M64T_MSG_EEPROM_STATUS, M64T_SAVE_ERR_PARAM);
            return;
        }
        off = read_be16(body);
        elen = read_be16(body + 2U);
        if (elen > 256U) {
            elen = 256U;
        }
        if (save_hw_eeprom_read_bytes(ebuf, (size_t)off, (size_t)elen) != 0) {
            m64t_send_status(M64T_MSG_EEPROM_STATUS, M64T_SAVE_ERR_NO_HW);
            return;
        }
        {
            uint8_t out[4 + 256];
            put_be16(out, off);
            put_be16(out + 2U, elen);
            memcpy(out + 4U, ebuf, (size_t)elen);
            m64t_send(M64T_MSG_EEPROM_DATA, out, 4U + (size_t)elen);
        }
        return;
    }

    if (msg == M64T_MSG_REQ_EEPROM_WRITE) {
        uint16_t off;
        uint16_t wlen;
        if (s_session_id == 0U) {
            m64t_send_status(M64T_MSG_EEPROM_STATUS, M64T_SAVE_ERR_SESSION);
            return;
        }
        if (body_len < 4U) {
            m64t_send_status(M64T_MSG_EEPROM_STATUS, M64T_SAVE_ERR_PARAM);
            return;
        }
        off = read_be16(body);
        wlen = read_be16(body + 2U);
        if (body_len < 4U + (size_t)wlen) {
            m64t_send_status(M64T_MSG_EEPROM_STATUS, M64T_SAVE_ERR_PARAM);
            return;
        }
        if (save_hw_eeprom_write_bytes(body + 4U, (size_t)off, (size_t)wlen) != 0) {
            m64t_send_status(M64T_MSG_EEPROM_STATUS, M64T_SAVE_ERR_NO_HW);
            return;
        }
        m64t_send_status(M64T_MSG_EEPROM_STATUS, M64T_SAVE_STATUS_OK);
        return;
    }

    if (msg == M64T_MSG_REQ_SRAM_INFO) {
        uint8_t b[8];
        put_be32(b, save_hw_sram_size_bytes());
        put_be32(b + 4U, SAVE_HW_PI_SRAM_BASE);
        m64t_send(M64T_MSG_SRAM_INFO, b, sizeof(b));
        return;
    }

    if (msg == M64T_MSG_REQ_SRAM_READ) {
        static uint8_t __attribute__((aligned(8))) sbuf[512];
        uint32_t soff;
        uint16_t slen;
        if (body_len < 6U) {
            m64t_send_status(M64T_MSG_SRAM_STATUS, M64T_SAVE_ERR_PARAM);
            return;
        }
        soff = read_be32(body);
        slen = read_be16(body + 4U);
        if (save_hw_sram_read(sbuf, soff, (size_t)slen) != 0) {
            m64t_send_status(M64T_MSG_SRAM_STATUS, M64T_SAVE_ERR_NO_HW);
            return;
        }
        {
            uint8_t out[6 + 512];
            put_be32(out, soff);
            put_be16(out + 4U, slen);
            memcpy(out + 6U, sbuf, (size_t)slen);
            m64t_send(M64T_MSG_SRAM_DATA, out, 6U + (size_t)slen);
        }
        return;
    }

    if (msg == M64T_MSG_REQ_SRAM_WRITE) {
        uint32_t soff;
        uint16_t slen;
        if (s_session_id == 0U) {
            m64t_send_status(M64T_MSG_SRAM_STATUS, M64T_SAVE_ERR_SESSION);
            return;
        }
        if (body_len < 6U) {
            m64t_send_status(M64T_MSG_SRAM_STATUS, M64T_SAVE_ERR_PARAM);
            return;
        }
        soff = read_be32(body);
        slen = read_be16(body + 4U);
        if (body_len < 6U + (size_t)slen) {
            m64t_send_status(M64T_MSG_SRAM_STATUS, M64T_SAVE_ERR_PARAM);
            return;
        }
        if (save_hw_sram_write(body + 6U, soff, (size_t)slen) != 0) {
            m64t_send_status(M64T_MSG_SRAM_STATUS, M64T_SAVE_ERR_NO_HW);
            return;
        }
        m64t_send_status(M64T_MSG_SRAM_STATUS, M64T_SAVE_STATUS_OK);
        return;
    }

    if (msg == M64T_MSG_REQ_RUMBLE) {
        uint8_t ack[3];
        if (body_len < 2U) {
            ack[0] = 0;
            ack[1] = 0;
            ack[2] = M64T_RUMBLE_ERR_PARAM;
            m64t_send(M64T_MSG_RUMBLE_ACK, ack, sizeof(ack));
            return;
        }
        {
            int port = (int)body[0];
            uint8_t dur = body[1];
            uint32_t frames = dur ? (uint32_t)dur : 60U;
            if (frames > RUMBLE_MAX_FRAMES) {
                frames = RUMBLE_MAX_FRAMES;
            }
            int rc = test_proto_rumble_pulse(port, frames);
            ack[0] = body[0];
            if (rc == 0) {
                ack[1] = (uint8_t)(frames > 255U ? 255U : frames);
                ack[2] = 0;
            } else if (rc == 1) {
                ack[1] = 0;
                ack[2] = M64T_RUMBLE_ERR_UNSUPPORTED;
            } else {
                ack[1] = 0;
                ack[2] = M64T_RUMBLE_ERR_PARAM;
            }
            m64t_send(M64T_MSG_RUMBLE_ACK, ack, sizeof(ack));
        }
        return;
    }

    if (msg == M64T_MSG_REQ_DISPLAY_TEXT) {
        uint8_t st = M64T_DISPLAY_TEXT_OK;
        if (body_len > (size_t)TEST_HOST_DISPLAY_MAX) {
            st = M64T_DISPLAY_TEXT_ERR_TOO_LONG;
        } else if (body_len == 0U) {
            host_display_clear();
        } else {
            host_display_set(body, body_len);
        }
        m64t_send(M64T_MSG_DISPLAY_TEXT_ACK, &st, 1U);
        return;
    }
}

void test_proto_send_bench_tick(uint32_t tick)
{
    uint8_t app[9];
    app[0] = M64T_MAGIC0;
    app[1] = M64T_MAGIC1;
    app[2] = M64T_MAGIC2;
    app[3] = M64T_MAGIC3;
    app[4] = M64T_MSG_BENCH_TICK;
    put_be32(app + 5, tick);
    send_l3_app(app, (int)sizeof(app));
}

static void rx_drop(size_t n)
{
    if (n >= s_rx_len) {
        s_rx_len = 0;
        return;
    }
    memmove(s_rx, s_rx + n, s_rx_len - n);
    s_rx_len -= n;
}

void test_proto_reset_diag(void)
{
    s_rx_overflow_count = 0;
    s_rx_resync_bytes = 0;
    s_bad_header_drops = 0;
}

void test_proto_reset_all(void)
{
    rumble_stop_all();
    host_display_clear();
    s_rx_len = 0;
    s_total_m64t_handled = 0;
    s_session_id = 0U;
    memset(s_session_challenge, 0, sizeof(s_session_challenge));
    m64p_reset_stats();
    test_proto_reset_diag();
}

uint32_t test_proto_get_session_id(void)
{
    return s_session_id;
}

uint32_t test_proto_get_rx_overflow_count(void)
{
    return s_rx_overflow_count;
}

uint32_t test_proto_get_rx_resync_bytes(void)
{
    return s_rx_resync_bytes;
}

uint32_t test_proto_get_bad_header_drops(void)
{
    return s_bad_header_drops;
}

void test_proto_rx_append(const uint8_t *p, int n)
{
    if (n <= 0) {
        return;
    }
    if (s_rx_len + (size_t)n > TEST_RX_CAP) {
        s_rx_overflow_count++;
        s_rx_len = 0;
        return;
    }
    memcpy(s_rx + s_rx_len, p, (size_t)n);
    s_rx_len += (size_t)n;
}

unsigned test_proto_drain_stream(void)
{
    unsigned handled = 0;

    while (s_rx_len >= 4U) {
        size_t scan = 0;
        while (scan + 4U <= s_rx_len) {
            if (s_rx[scan] == L3_MAGIC0 && s_rx[scan + 1U] == L3_MAGIC1 && s_rx[scan + 2U] == L3_MAGIC2 &&
                s_rx[scan + 3U] == L3_MAGIC3) {
                break;
            }
            scan++;
        }
        if (scan > 0U) {
            s_rx_resync_bytes += (uint32_t)scan;
            rx_drop(scan);
        }
        if (s_rx_len < 16U) {
            return handled;
        }
        if (s_rx[4] != L3_TYPE_DATA || s_rx[5] != L3_CH_APPLICATION) {
            s_bad_header_drops++;
            rx_drop(1U);
            continue;
        }
        uint32_t payload_len = read_be32(&s_rx[12]);
        if (payload_len > 1048576U) {
            s_bad_header_drops++;
            rx_drop(1U);
            continue;
        }
        if (s_rx_len < 16U + (size_t)payload_len) {
            return handled;
        }

        const uint8_t *payload = s_rx + 16;
        if (payload_len >= 5U && payload[0] == M64T_MAGIC0 && payload[1] == M64T_MAGIC1 && payload[2] == M64T_MAGIC2 &&
            payload[3] == M64T_MAGIC3) {
            handle_m64t(payload, (size_t)payload_len);
            handled++;
            s_total_m64t_handled++;
        } else if (m64p_handle(payload, (size_t)payload_len)) {
            handled++;
            s_total_m64t_handled++;
        }

        rx_drop(16U + (size_t)payload_len);
    }
    return handled;
}

uint32_t test_proto_get_frames_handled(void)
{
    return s_total_m64t_handled;
}
