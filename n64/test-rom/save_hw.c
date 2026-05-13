#include "save_hw.h"

#include <dma.h>
#include <eeprom.h>
#include <string.h>

void save_hw_eeprom_get_info(save_hw_eeprom_info_t *out)
{
    out->eeprom_type = (uint8_t)eeprom_present();
    out->eeprom_blocks = (uint16_t)eeprom_total_blocks();
}

int save_hw_eeprom_read_bytes(uint8_t *dest, size_t start, size_t len)
{
    if (eeprom_present() == EEPROM_NONE) {
        return -1;
    }
    size_t total = eeprom_total_blocks() * EEPROM_BLOCK_SIZE;
    if (len == 0U) {
        return 0;
    }
    if (start + len > total || len > 256U) {
        return -1;
    }
    eeprom_read_bytes(dest, start, len);
    return 0;
}

int save_hw_eeprom_write_bytes(const uint8_t *src, size_t start, size_t len)
{
    if (eeprom_present() == EEPROM_NONE) {
        return -1;
    }
    size_t total = eeprom_total_blocks() * EEPROM_BLOCK_SIZE;
    if (len == 0U) {
        return 0;
    }
    if (start + len > total || len > 256U) {
        return -1;
    }
    eeprom_write_bytes(src, start, len);
    return 0;
}

uint32_t save_hw_sram_size_bytes(void)
{
#if TEST_SRAM_BYTES > 0
    return (uint32_t)TEST_SRAM_BYTES;
#else
    return 0U;
#endif
}

int save_hw_sram_read(void *dest, uint32_t offset, size_t len)
{
#if TEST_SRAM_BYTES <= 0
    (void)dest;
    (void)offset;
    (void)len;
    return -1;
#else
    if (len == 0U) {
        return 0;
    }
    if (len > 512U || (len & 1U) != 0U || (offset & 1U) != 0U) {
        return -1;
    }
    if ((uint64_t)offset + (uint64_t)len > (uint64_t)TEST_SRAM_BYTES) {
        return -1;
    }
    dma_read_raw_async(dest, SAVE_HW_PI_SRAM_BASE + offset, len);
    dma_wait();
    return 0;
#endif
}

int save_hw_sram_write(const void *src, uint32_t offset, size_t len)
{
#if TEST_SRAM_BYTES <= 0
    (void)src;
    (void)offset;
    (void)len;
    return -1;
#else
    if (len == 0U) {
        return 0;
    }
    if (len > 512U || (len & 1U) != 0U || (offset & 1U) != 0U) {
        return -1;
    }
    if ((uint64_t)offset + (uint64_t)len > (uint64_t)TEST_SRAM_BYTES) {
        return -1;
    }
    dma_write_raw_async(src, SAVE_HW_PI_SRAM_BASE + offset, len);
    dma_wait();
    return 0;
#endif
}
