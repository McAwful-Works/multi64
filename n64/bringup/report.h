/**
 * The bring-up report: everything the ROM measured, in RDRAM where a host reads it with M64P PEEKV.
 *
 * docs/spec/cart-bringup-report-v0.md is normative for this layout; change the two together. Every
 * field is a 32-bit word, stored big-endian as the CPU stores it, so the layout has no padding and
 * the offsets the spec lists are the ones _Static_assert checks below.
 */
#ifndef MULTI64_BRINGUP_REPORT_H
#define MULTI64_BRINGUP_REPORT_H

#include <stdint.h>

/** Spec revision of this layout (report.format). */
#define REPORT_FORMAT 0u

/** This ROM's own version (report.rom_version): major << 16 | minor. */
#define BRINGUP_ROM_VERSION 0x00010001u

/* report.cart */
#define CART_NONE 0u
#define CART_SC64 1u
#define CART_X_SERIES 2u /* EverDrive X7 or 3.0 */
#define CART_PRO 3u
#define CART_OTHER 4u /* 64drive: answers, but no agent driver exists */

/* Variant ids: one agent build each, linked side by side (report.variant, report.ctl_variant). */
#define VARIANT_NONE 0u
#define VARIANT_SC64 1u    /* sc64.c */
#define VARIANT_X7_IO 2u   /* ed64.c, pi_io.c moving words by CPU load and store */
#define VARIANT_X7_DMA 3u  /* ed64.c, pi_io.c built with PI_IO_DMA */
#define VARIANT_PRO 4u     /* ed64pro.c */
#define VARIANT_COUNT 4u

/* report.probe_ok: which raw reads in the identify stage completed. */
#define PROBE_ROM_HEADER (1u << 0)
#define PROBE_D64_MAGIC (1u << 1)
#define PROBE_SC64_IDENT (1u << 2)
#define PROBE_ED_REG14 (1u << 3)
#define PROBE_ED_REG04 (1u << 4)
#define PROBE_PRO_SYSSTAT (1u << 5)

/* report.stages */
#define STAGE_IDENTIFY (1u << 0)
#define STAGE_BLOCKS (1u << 1)
#define STAGE_CONDITIONS (1u << 2)
#define STAGE_LINK (1u << 3)

/* timing[].id: which register a cost was measured on, and which spin limit it is multiplied by. */
#define TIMING_PI_STATUS 1u    /* PI_STATUS, read directly; pi_io.c PI_WAIT_SPINS */
#define TIMING_X7_USBCFG 2u    /* 0x1F800004 through pi_io_read; ed64.c ED_BUSY_SPINS */
#define TIMING_SC64_SR_CMD 3u  /* 0x1FFF0000 through pi_io_read; sc64.c SC64_CMD_SPINS */
#define TIMING_PRO_SYSSTAT 4u  /* 0x1F800008 through pi_io_read; ed64pro.c MCU_SPINS */
#define TIMING_PRO_FIFOSTAT 5u /* 0x1F800004 through pi_io_read; ed64pro.c STATUS_SPINS */
#define TIMING_SLOTS 4u
/** Reads each timing entry averages over. */
#define TIMING_READS 1024u

/* buffer[].result */
#define BUF_NOT_RUN 0u
#define BUF_MATCH 1u
#define BUF_MISMATCH 2u
#define BUF_WRITE_FAILED 3u /* pi_io reported the PI busy: nothing to compare */
#define BUF_READ_FAILED 4u
#define BUF_NOT_APPLICABLE 5u /* this cart has no buffer the test can use */

/* buffer[] order: how the 512 bytes were written, then how they were read back. */
#define BUF_IO_IO 0u
#define BUF_IO_DMA 1u
#define BUF_DMA_IO 2u
#define BUF_DMA_DMA 3u
#define BUF_COMBOS 4u
#define BUF_BYTES 512u

/* Load levels (report.load, report.ctl_load): background ROM DMA standing in for a game. */
#define LOAD_OFF 0u
#define LOAD_MODERATE 1u /* 4 KiB every 2 ms */
#define LOAD_HEAVY 2u    /* 16 KiB every 2 ms: more than the PI can finish, so it is never idle long */
#define LOAD_LEVELS 3u

/** Host-owned scratch at the end of the report, for POKEV/PEEKV echo checks. */
#define ECHO_BYTES 4096u

struct timing {
    uint32_t id;          /* TIMING_*, 0 for an unused slot */
    uint32_t spins;       /* the driver's spin limit for a wait polling this register */
    uint32_t ticks_total; /* Count ticks for TIMING_READS reads */
    uint32_t ticks_max;   /* slowest single read */
};

struct buffer_test {
    uint32_t result;      /* BUF_* */
    uint32_t first_bad;   /* byte offset of the first mismatch, 0xFFFFFFFF if none */
    uint32_t bad_bytes;   /* bytes that differ */
    uint32_t ticks_write;
    uint32_t ticks_read;
};

/** One agent build's traffic. Counters only grow; maxima reset when the variant or load changes. */
struct link_stats {
    uint32_t init_calls;     /* the agent's own cart init, through the driver */
    uint32_t init_ok;
    uint32_t recv_calls;     /* ed64_receive / ed64pro_receive, or sc64_poll on an SC64 */
    uint32_t recv_data;      /* calls that delivered bytes */
    uint32_t recv_bytes;
    uint32_t recv_lost;      /* calls that reported part of the stream lost (EverDrives) */
    uint32_t read_calls;     /* sc64_read (SC64 only) */
    uint32_t read_failed;
    uint32_t send_calls;
    uint32_t send_ok;
    uint32_t send_bytes;
    uint32_t pio_calls;      /* pi_io calls from the driver and PEEKROM */
    uint32_t pio_failed;     /* ... that reported the PI busy */
    uint32_t recv_ticks_max;
    uint32_t send_ticks_max;
    uint32_t tick_ticks_max; /* one whole agent_tick */
    uint32_t tick_ticks_total;
    uint32_t agent_ticks;    /* agent_get_ticks() */
    uint32_t agent_frames;   /* agent_get_frames_handled(): frames that carried an M64P request */
    uint32_t agent_ready;    /* agent_is_ready() */
    uint32_t m64p_requests;
    uint32_t m64p_errors;
    uint32_t m64p_last_error;
    uint32_t reserved;
};

struct report {
    /* 0x000 header */
    uint32_t magic[2];       /* "CARTBRUP", assembled at run time so the image holds no copy */
    uint32_t format;         /* REPORT_FORMAT */
    uint32_t size;           /* sizeof(struct report) */
    uint32_t self;           /* physical address of this struct */
    uint32_t rom_version;    /* BRINGUP_ROM_VERSION */
    uint32_t frame;          /* main-loop iterations since boot */
    uint32_t count_hz;       /* rate of the Count ticks every *_ticks field is in */

    /* 0x020 control: written by the host, acted on once a frame */
    uint32_t ctl_load;       /* LOAD_* */
    uint32_t ctl_variant;    /* VARIANT_*; 0 keeps the current one */
    uint32_t ctl_rerun;      /* the blocks and conditions stages run again when this changes */
    uint32_t ctl_reserved;

    /* 0x030 state */
    uint32_t cart;           /* CART_* */
    uint32_t variant;        /* VARIANT_* driving the link now */
    uint32_t load;           /* LOAD_* in effect */
    uint32_t rerun_done;     /* the ctl_rerun value last acted on */
    uint32_t stages;         /* STAGE_* completed */
    uint32_t mem_size;       /* RDRAM bytes */
    uint32_t reserved0[2];

    /* 0x050 identify: raw reads before any unlock, then each driver's own init */
    uint32_t probe_ok;            /* PROBE_* */
    uint32_t d64_magic;           /* 0x180002EC: 'UDEV' on a 64drive */
    uint32_t sc64_ident_locked;   /* 0x1FFF000C before the SC64 unlock */
    uint32_t ed_reg14_locked;     /* 0x1F800014 before any key: X-series VERSION, PRO EDID */
    uint32_t ed_reg04_locked;     /* 0x1F800004: X-series USBCFG, PRO FIFOSTAT */
    uint32_t pro_sysstat[2];      /* 0x1F800008 twice: a PRO flips bit 3 on every read */
    uint32_t init_tried;          /* 1 << VARIANT_* for each driver init tried, in PRO, X7, SC64 order */
    uint32_t init_ok;             /* ... and each that succeeded */
    uint32_t ed_reg14_unlocked;   /* 0x1F800014 after the X7 driver's init */
    uint32_t ed_usbcfg_after;     /* 0x1F800004 after the X7 driver's init */
    uint32_t sc64_ident_unlocked; /* 0x1FFF000C after the SC64 driver's init */
    uint32_t rom_header[16];      /* the first 64 bytes of cartridge ROM */

    /* 0x0C0 blocks: [0] with no load, [1] under report.cond_load */
    struct timing timing[2][TIMING_SLOTS];
    uint32_t buf_addr;            /* PI address the buffer tests used, 0 if none */
    uint32_t cond_load;           /* load level the conditions stage ran under */
    struct buffer_test buffer[2][BUF_COMBOS];

    /* background load, since the level last changed */
    uint32_t load_bytes;          /* per DMA */
    uint32_t load_period_ticks;   /* between timer callbacks */
    uint32_t load_started;        /* DMAs started */
    uint32_t load_skipped;        /* callbacks that found the PI busy and started none */
    uint32_t irq_gap_max;         /* longest gap between two timer callbacks */
    uint32_t reserved1[3];

    /* per variant, index VARIANT_* - 1 */
    struct link_stats link[VARIANT_COUNT];

    /* host scratch */
    uint8_t echo[ECHO_BYTES];
};

#define REPORT_OFFSET_CHECK(field, off) \
    _Static_assert(__builtin_offsetof(struct report, field) == (off), #field " moved: update the spec")
REPORT_OFFSET_CHECK(ctl_load, 0x020);
REPORT_OFFSET_CHECK(cart, 0x030);
REPORT_OFFSET_CHECK(probe_ok, 0x050);
REPORT_OFFSET_CHECK(rom_header, 0x080);
REPORT_OFFSET_CHECK(timing, 0x0C0);
REPORT_OFFSET_CHECK(buf_addr, 0x140);
REPORT_OFFSET_CHECK(buffer, 0x148);
REPORT_OFFSET_CHECK(load_bytes, 0x1E8);
REPORT_OFFSET_CHECK(link, 0x208);
REPORT_OFFSET_CHECK(echo, 0x388);
_Static_assert(sizeof(struct report) == 0x1388, "report size changed: update the spec");

/** The one report, 16-byte aligned somewhere in the program's BSS. */
extern struct report g_report;

#endif
