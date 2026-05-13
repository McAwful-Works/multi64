/**
 * EEPROM (joybus) + PI SRAM helpers for the test ROM.
 * SRAM size is compile-time (see Makefile TEST_SRAM_BYTES); 0 = not built for SRAM.
 */
#ifndef MULTI64_SAVE_HW_H
#define MULTI64_SAVE_HW_H

#include <stddef.h>
#include <stdint.h>

#ifndef TEST_SRAM_BYTES
#define TEST_SRAM_BYTES 0
#endif

typedef struct {
    uint8_t eeprom_type;
    uint16_t eeprom_blocks;
} save_hw_eeprom_info_t;

void save_hw_eeprom_get_info(save_hw_eeprom_info_t *out);

/** Returns 0 on success, -1 if no EEPROM or bad range. */
int save_hw_eeprom_read_bytes(uint8_t *dest, size_t start, size_t len);

/** Returns 0 on success, -1 if no EEPROM or bad range. */
int save_hw_eeprom_write_bytes(const uint8_t *src, size_t start, size_t len);

/** PI SRAM window size for this ROM build (0 if none). */
uint32_t save_hw_sram_size_bytes(void);

/** PI base used for SRAM DMA (documentation / host). */
#define SAVE_HW_PI_SRAM_BASE 0x08000000U

/**
 * DMA read from cart SRAM. `offset` and `len` must be even; `len` <= 512.
 * `dest` must be 8-byte aligned.
 */
int save_hw_sram_read(void *dest, uint32_t offset, size_t len);

/** DMA write to cart SRAM. Same alignment rules as read. */
int save_hw_sram_write(const void *src, uint32_t offset, size_t len);

#endif
