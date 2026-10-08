/**
 * The stages that run without a host: identify the cart, exercise the driver's building blocks, and
 * repeat the blocks under game conditions. Each writes its results into g_report.
 */
#ifndef MULTI64_BRINGUP_STAGES_H
#define MULTI64_BRINGUP_STAGES_H

#include <stdint.h>

/** Stage 1. Raw reads before any unlock, then each driver's own init until one answers. */
void stage_identify(void);

/** Stage 2, into slot 0 (no load) or 1 (under load): register read costs and buffer moves. */
void stage_blocks(unsigned slot);

/** Stage 4's offline half: stage 2 again under moderate background load, into slot 1. */
void stage_conditions(void);

/** The variant that drives a cart's link by default (VARIANT_NONE when there is none). */
uint32_t default_variant(uint32_t cart);

/** Whether `variant` can drive `cart` at all. */
int variant_fits(uint32_t variant, uint32_t cart);

#endif
