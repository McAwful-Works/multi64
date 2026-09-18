/**
 * Cartridge ROM for M64P's PEEKROM (memory-l3-application-v0.md 4.2): the mem_proto.c hooks
 * m64p_cart_rom_size() and m64p_cart_rom_read().
 *
 * The same on every cart. The console sees the ROM on the PI bus at 0x10000000, whatever
 * cart serves it, so this reads it with CPU loads through pi_io.c under the rules every
 * driver here keeps: never touch the PI control registers, check the PI is idle and mask
 * interrupts around each short burst of loads, and bound every wait. A game streaming from
 * ROM by DMA keeps its bus; a read that finds it busy for too long fails, and the host is
 * told to try again.
 */
#include "m64p_types.h"
#include "mem_proto.h"
#include "pi_io.h"

/** PI address of ROM offset 0 (cart domain 1, address 2). */
#define CART_ROM_BASE 0x10000000u

/**
 * The window PEEKROM may address: 64 MiB, the largest N64 ROM. Nothing on the console
 * records the size of the image that booted, so this is an upper bound; past the end of a
 * smaller image the cart returns whatever it maps there.
 */
#define CART_ROM_BYTES 0x04000000u

/** Words loaded per pi_io call: the stack buffer below, 64 bytes. */
#define CHUNK_WORDS 16u

uint32_t m64p_cart_rom_size(void)
{
    return CART_ROM_BYTES;
}

int m64p_cart_rom_read(uint32_t off, uint8_t *dst, uint32_t len)
{
    uint32_t words[CHUNK_WORDS];
    uint32_t end = off + len;
    uint32_t pos = off & ~3u; /* the PI reads whole, aligned words */

    while (pos < end) {
        uint32_t n = (end - pos + 3u) / 4u;
        uint32_t i;

        if (n > CHUNK_WORDS) {
            n = CHUNK_WORDS;
        }
        if (!pi_io_load_words(words, CART_ROM_BASE + pos, n)) {
            return 0;
        }
        /* Big-endian: byte 0 of a word is its most significant. */
        for (i = 0u; i < n * 4u; i++) {
            uint32_t at = pos + i;
            if (at >= off && at < end) {
                dst[at - off] = (uint8_t)(words[i / 4u] >> (24u - 8u * (i % 4u)));
            }
        }
        pos += n * 4u;
    }
    return 1;
}
