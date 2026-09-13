#!/bin/bash
# Build the M64P agent as a flat image at a fixed RAM address, plus its hook stub, for
# a ROM with no buildable source. See docs/integration/placing-the-agent.md.
#
#   link-flat.sh AGENT_VRAM AGENT_ROM AGENT_MIN_RAM STUB_VRAM HOOK_ORIGINAL ROM_COPY [OUT]
#
# e.g. link-flat.sh 0x80480000 0xC00000 0x800000 0x80019C00 0x80002334 0x800177EC
#
# Writes OUT (default ../build/flat): agent.bin, stub.bin, agent.elf, stub.elf and
# layout.env, which also carries the counter offsets a probe needs. Environment:
#   PREFIX     toolchain prefix (default mips64-elf-, libdragon's)
#   STUB_MAX   refuse a stub larger than this many bytes (default 4096)
#   CART       sc64 (default; the only driver that has run on hardware), or ed64 / ed64pro for an
#              EverDrive-64 X7 / PRO (experimental, never run on a cart)
set -eu

[ $# -ge 6 ] || { sed -n '2,15p' "$0" >&2; exit 2; }
VRAM=$1 ROM=$2 MINRAM=$3 STUB_VRAM=$4 HOOK_ORIGINAL=$5 ROM_COPY=$6

HERE=$(cd "$(dirname "$0")" && pwd)
AGENT="$HERE/.."
PROTO="$HERE/../../test-rom"
OUT=${7:-"$AGENT/build/flat"}
P=${PREFIX:-mips64-elf-}
STUB_MAX=${STUB_MAX:-4096}
CART=${CART:-sc64}

case "$CART" in
    sc64)    DRIVERS="sc64";          DEFINE="" ;;
    ed64)    DRIVERS="ed64 pi_io";    DEFINE="-DAGENT_CART_ED64" ;;
    ed64pro) DRIVERS="ed64pro pi_io"; DEFINE="-DAGENT_CART_ED64PRO" ;;
    *) echo "CART must be sc64, ed64 or ed64pro" >&2; exit 2 ;;
esac

mkdir -p "$OUT"
CFLAGS="-O1 -fno-reorder-blocks -march=vr4300 -mtune=vr4300 -mabi=32 -mno-gpopt -G0 \
        -mno-abicalls -fno-pic -mdivide-breaks -mexplicit-relocs -ffreestanding -Wall -Wextra"
INC="-I$AGENT -I$PROTO"

${P}gcc $CFLAGS $INC -c "$HERE/segment_magic.c" -o "$OUT/segment_magic.o"
${P}gcc $CFLAGS $DEFINE $INC -c "$AGENT/agent.c" -o "$OUT/agent.o"
DRIVER_OBJS=()
for d in $DRIVERS; do
    ${P}gcc $CFLAGS $DEFINE $INC -c "$AGENT/$d.c" -o "$OUT/$d.o"
    DRIVER_OBJS+=("$OUT/$d.o")
done
${P}gcc $CFLAGS $INC -c "$PROTO/mem_proto.c" -o "$OUT/mem_proto.o"

${P}ld -T "$HERE/flat.ld" --defsym AGENT_VRAM=$VRAM -o "$OUT/agent.elf" \
    "$OUT/segment_magic.o" "$OUT/agent.o" "${DRIVER_OBJS[@]}" "$OUT/mem_proto.o"
${P}objcopy -O binary --only-section=.text --only-section=.rodata --only-section=.data \
    "$OUT/agent.elf" "$OUT/agent.bin"

UNDEF=$(${P}nm -u "$OUT/agent.elf" | wc -l)
[ "$UNDEF" -eq 0 ] || { echo "agent has undefined symbols:" >&2; ${P}nm -u "$OUT/agent.elf" >&2; exit 1; }

sym() { ${P}nm "$OUT/agent.elf" | awk -v s="$1" '$3 == s { print "0x" $1 }'; }
off() { printf '0x%X' $(( $(sym "$1") - VRAM )); }
LOAD_END=$(sym agent_LOAD_END)
BSS_START=$(sym agent_BSS_START)
BSS_END=$(sym agent_BSS_END)
MAGIC=$(sym gAgentSegmentMagic)
LOAD_SIZE=$(printf '0x%X' $(( LOAD_END - VRAM )))

${P}gcc -march=vr4300 -mabi=32 -G0 -mno-abicalls -fno-pic -c "$HERE/hook_stub.S" -o "$OUT/stub.o"
${P}ld -Ttext=$STUB_VRAM --just-symbols="$OUT/agent.elf" \
    --defsym HOOK_ORIGINAL=$HOOK_ORIGINAL --defsym ROM_COPY=$ROM_COPY \
    --defsym AGENT_ROM=$ROM --defsym AGENT_LOAD_SIZE=$LOAD_SIZE \
    --defsym AGENT_MAGIC_ADDR=$MAGIC --defsym AGENT_MIN_RAM=$MINRAM \
    -e agent_hook_stub -o "$OUT/stub.elf" "$OUT/stub.o"
${P}objcopy -O binary --only-section=.text "$OUT/stub.elf" "$OUT/stub.bin"
STUB_SIZE=$(stat -c %s "$OUT/stub.bin")
[ "$STUB_SIZE" -le "$STUB_MAX" ] || { echo "stub is $STUB_SIZE B, over STUB_MAX=$STUB_MAX" >&2; exit 1; }

cat > "$OUT/layout.env" <<EOF
AGENT_CART=$CART
AGENT_VRAM=$VRAM
AGENT_ROM=$ROM
AGENT_MIN_RAM=$MINRAM
AGENT_LOAD_SIZE=$LOAD_SIZE
AGENT_BSS_START=$BSS_START
AGENT_BSS_END=$BSS_END
AGENT_MAGIC_ADDR=$MAGIC
AGENT_TICK=$(sym agent_tick)
STUB_VRAM=$STUB_VRAM
STUB_SIZE=$STUB_SIZE
HOOK_ORIGINAL=$HOOK_ORIGINAL
ROM_COPY=$ROM_COPY
OFF_MAGIC=$(off gAgentSegmentMagic)
OFF_INIT_ATTEMPTS=$(off s_init_attempts)
OFF_READY=$(off s_ready)
OFF_FRAMES_HANDLED=$(off s_frames_handled)
OFF_TICKS=$(off s_ticks)
OFF_REQUESTS=$(off s_requests)
OFF_ERRORS=$(off s_errors)
EOF

echo "agent: $CART driver, RAM $VRAM-$BSS_END ($(( BSS_END - VRAM )) B, $(( BSS_END - BSS_START )) B of it BSS)," \
     "loads $LOAD_SIZE B from ROM $ROM"
echo "stub:  $STUB_SIZE B at $STUB_VRAM; calls $HOOK_ORIGINAL, loads with $ROM_COPY, guard osMemSize >= $MINRAM"
echo "wrote $OUT/{agent.bin,stub.bin,layout.env}"
