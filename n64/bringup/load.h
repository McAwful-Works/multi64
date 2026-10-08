/**
 * Background PI load, standing in for a game streaming from ROM.
 *
 * A timer interrupt every 2 ms starts a ROM-to-RDRAM DMA, when the PI is idle, from interrupt
 * context, which is where a game's PI manager starts its own. The drivers under test then meet a
 * busy PI the way they would in a game, and their masked spans hold the timer off the way they would
 * hold off a game's interrupts: report.irq_gap_max records the longest gap between two callbacks.
 */
#ifndef MULTI64_BRINGUP_LOAD_H
#define MULTI64_BRINGUP_LOAD_H

#include <stdint.h>

/** Allocate the DMA target and start the timer subsystem's use. Call once, after timer_init(). */
void load_init(void);

/** Set the level (LOAD_*), resetting the load counters in the report. */
void load_set(uint32_t level);

/** The level in effect. */
uint32_t load_level(void);

#endif
