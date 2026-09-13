/**
 * Game-resident M64P agent — the surface a host ROM calls.
 *
 * The whole contract is one call per frame to agent_tick(), from the game's own
 * thread and never from an interrupt, plus enough RAM for the image (about 21 KB,
 * most of it BSS). See docs/integration/cart-agent.md.
 */
#ifndef MULTI64_AGENT_H
#define MULTI64_AGENT_H

#include "m64p_types.h"

/** Service at most one M64P request from the cart. Cheap when the host sent nothing. */
void agent_tick(void);

/** Calls to agent_tick() since boot. */
uint32_t agent_get_ticks(void);

/** Frames that carried an M64P request. */
uint32_t agent_get_frames_handled(void);

/** 1 once the cart has identified itself; stays 0 when the agent has gone dormant. */
int agent_is_ready(void);

#endif
