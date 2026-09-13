/**
 * Fixed-width types for the M64P handler (mem_proto) and code built alongside it.
 *
 * This is the only place the module touches the C library. mem_proto.h includes it
 * with quotes, so the copy sitting next to mem_proto.h is the one used: a build with
 * no <stdint.h> or <stddef.h> -- a -nostdinc decompilation project, say -- replaces
 * this one file and leaves mem_proto.c and mem_proto.h untouched.
 *
 * A replacement must provide exactly these, at these widths:
 *
 *   uint8_t   unsigned, 8 bits
 *   uint16_t  unsigned, 16 bits
 *   uint32_t  unsigned, 32 bits
 *   size_t    the type of sizeof
 *
 * and keep the MULTI64_M64P_TYPES_H guard, so it is read once however many headers
 * pull it in. For example, on top of libultra's PR/ultratypes.h, which already
 * defines size_t:
 *
 *   #include "PR/ultratypes.h"
 *   typedef u8 uint8_t;
 *   typedef u16 uint16_t;
 *   typedef u32 uint32_t;
 */
#ifndef MULTI64_M64P_TYPES_H
#define MULTI64_M64P_TYPES_H

#include <stddef.h>
#include <stdint.h>

#endif
