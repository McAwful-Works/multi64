/*
 * Load marker for an agent image copied into RAM from ROM on first use.
 *
 * The loader compares this word before deciding whether to copy, and again after the
 * copy before running anything, so RAM that holds something else -- or a copy that has
 * not finished -- is never executed. It survives a soft reset along with the rest of
 * RAM the game does not clear, which is what makes the agent reload only when needed.
 */
#include "m64p_types.h"

uint32_t gAgentSegmentMagic = 0x4D363450u; /* 'M64P' */
