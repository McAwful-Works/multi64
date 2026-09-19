/*
 * Host glue for the M64P agent linked as a flat image and spliced into a seed.
 *
 * The agent is linked at a fixed RAM address and copied in from ROM by the game's hook
 * stub (<game>/stub.S). This word, placed first in .data by agent.ld, tells a loaded
 * image from RAM that holds anything else; the stub checks it again after the copy, so
 * a copy that has not finished is never executed.
 */
#include "m64p_types.h"

uint32_t gAgentSegmentMagic = 0x4D363450u; /* 'M64P' */
