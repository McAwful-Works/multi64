#!/bin/bash
# Build the M64P agent as a flat image at a fixed RAM address, plus a game's hook stub, once for
# each flash cart, and install them in profiles/<game>/ (agent.bin, stub.bin, layout.env,
# BUILD_REV): the SummerCart64 build in the directory itself, and the EverDrive-64 X7 and PRO
# builds in its ed64/ and ed64pro/ subdirectories. AP64 patches a seed with the one for the cart
# the player chose.
#
#   build.sh <game>          e.g. build.sh cv64
#
# <game>/game.env gives STUB_VRAM, STUB_MAX, AGENT_VRAM, AGENT_ROM and AGENT_MIN_RAM, and
# <game>/stub.S is the hook stub. The agent is this repo's own: n64/agent, with the M64P
# handler it shares with the test ROM from n64/test-rom. It is linked here as a flat image
# with this build's flags, so its sources are compiled directly, not through its Makefile.
# The cart is chosen the way that Makefile's CART= does: one driver per build, and for an
# EverDrive the define that turns on L3 reassembly. The EverDrive drivers have not been shown to
# work on a cart. Needs mips64-ultra-elf (WSL). Run a CR-stripped copy placed next to this file.
set -eu
GAME=${1:?game}
HERE=$(cd "$(dirname "$0")" && pwd)
GDIR="$HERE/$GAME"
[ -f "$GDIR/game.env" ] || { echo "no $GDIR/game.env"; exit 1; }
eval "$(tr -d '\r' < "$GDIR/game.env" | grep -E '^[A-Z_]+=')"
VRAM=$AGENT_VRAM
ROM=$AGENT_ROM
MINRAM=$AGENT_MIN_RAM

REPO=$(cd "$HERE/../../.." && pwd)
PROFILE_ROOT="$HERE/../profiles/$GAME"
# AGENT_REV names the revision where git cannot see the checkout (WSL on a Windows worktree).
if [ -n "${AGENT_REV:-}" ]; then
    REV=$AGENT_REV
else
    REV=$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)
    if [ -n "$(git -C "$REPO" status --porcelain -- n64/agent n64/test-rom 2>/dev/null)" ]; then
        REV="$REV-dirty"
    fi
fi

P=mips64-ultra-elf-
# The agent's own flags, plus -G0 so nothing lands in gp-relative sections: this image
# runs inside a game whose $gp means something else.
CFLAGS="-O1 -fno-reorder-blocks -march=vr4300 -mtune=vr4300 -mabi=32 -mno-gpopt -G0 \
        -mno-abicalls -fno-pic -mdivide-breaks -mexplicit-relocs -ffreestanding -Wall -Wextra"

# build_cart <cart> <driver> <define> <install dir>
build_cart() {
    local CART=$1 DRV=$2 DEF=$3 PROFILE=$4
    local B="$GDIR/build/$CART"
    rm -rf "$B"; mkdir -p "$B/src"
    for f in agent.c agent.h "$DRV.c" "$DRV.h" cart_rom.c pi_io.c pi_io.h; do
        tr -d '\r' < "$REPO/n64/agent/$f" > "$B/src/$f"
    done
    for f in mem_proto.c mem_proto.h m64p_types.h; do
        tr -d '\r' < "$REPO/n64/test-rom/$f" > "$B/src/$f"
    done
    tr -d '\r' < "$HERE/common/host.c" > "$B/src/host.c"
    tr -d '\r' < "$HERE/common/agent.ld" > "$B/agent.ld"
    tr -d '\r' < "$GDIR/stub.S" > "$B/stub.S"
    tr -d '\r' < "$HERE/common/image_check.inc" > "$B/image_check.inc"

    for f in agent "$DRV" cart_rom pi_io mem_proto host; do
        ${P}gcc $CFLAGS $DEF -I"$B/src" -c "$B/src/$f.c" -o "$B/$f.o"
    done

    ${P}ld -T "$B/agent.ld" --defsym AGENT_VRAM=$VRAM -o "$B/agent.elf" \
        "$B/host.o" "$B/agent.o" "$B/$DRV.o" "$B/cart_rom.o" "$B/pi_io.o" "$B/mem_proto.o"
    ${P}objcopy -O binary --only-section=.text --only-section=.rodata --only-section=.data \
        "$B/agent.elf" "$B/agent.bin"

    sym() { ${P}nm "$B/agent.elf" | awk -v s="$1" '$3 == s { print "0x" $1 }'; }
    off() { printf '0x%X' $(( $(sym "$1") - VRAM )); }
    local LOAD_END BSS_START BSS_END MAGIC TICK LOAD_SIZE BIN_SIZE RO_END CHECK_BYTES CHECK_END
    local SUM CHECK_SUM STUB_SIZE w
    LOAD_END=$(sym agent_LOAD_END)
    BSS_START=$(sym agent_BSS_START)
    BSS_END=$(sym agent_BSS_END)
    MAGIC=$(sym gAgentSegmentMagic)
    TICK=$(sym agent_tick)
    LOAD_SIZE=$(printf '0x%X' $(( LOAD_END - VRAM )))
    BIN_SIZE=$(stat -c %s "$B/agent.bin")
    # The stub's image check (common/image_check.inc): the whole words of .text and .rodata,
    # and their big-endian sum modulo 2^32, taken from the agent.bin that ships.
    RO_END=$(sym agent_RO_END)
    CHECK_BYTES=$(( (RO_END - VRAM) / 4 * 4 ))
    CHECK_END=$(printf '0x%X' $(( VRAM + CHECK_BYTES )))
    SUM=0
    for w in $(od -An -v -tx4 --endian=big -N "$CHECK_BYTES" "$B/agent.bin"); do
        SUM=$(( (SUM + 0x$w) & 0xFFFFFFFF ))
    done
    CHECK_SUM=$(printf '0x%08X' "$SUM")

    ${P}gcc -march=vr4300 -mabi=32 -G0 -mno-abicalls -fno-pic -c "$B/stub.S" -o "$B/stub.o"
    ${P}ld -Ttext=$STUB_VRAM --just-symbols="$B/agent.elf" \
        --defsym AGENT_ROM=$ROM --defsym AGENT_LOAD_SIZE=$LOAD_SIZE \
        --defsym AGENT_MAGIC_ADDR=$MAGIC --defsym AGENT_MIN_RAM=$MINRAM \
        --defsym AGENT_CHECK_END=$CHECK_END --defsym AGENT_CHECK_SUM=$CHECK_SUM \
        -e agent_hook_stub -o "$B/stub.elf" "$B/stub.o"
    ${P}objcopy -O binary --only-section=.text "$B/stub.elf" "$B/stub.bin"
    STUB_SIZE=$(stat -c %s "$B/stub.bin")

    echo "--- $GAME agent image, $CART"
    ${P}size -A "$B/agent.elf" | grep -E '^\.(text|rodata|data|bss) '
    echo "undefined symbols: $(${P}nm -u "$B/agent.elf" | wc -l)"
    ${P}nm -u "$B/agent.elf" || true
    echo "AGENT_VRAM=$VRAM LOAD_END=$LOAD_END LOAD_SIZE=$LOAD_SIZE (agent.bin $BIN_SIZE B)"
    echo "BSS $BSS_START-$BSS_END ($(( BSS_END - BSS_START )) B)  RAM total $(( BSS_END - VRAM )) B"
    echo "gAgentSegmentMagic=$MAGIC agent_tick=$TICK"
    echo "image check: $CHECK_BYTES B to $CHECK_END, sum $CHECK_SUM"
    echo "--- stub at $STUB_VRAM: $STUB_SIZE B (limit $STUB_MAX)"
    [ "$STUB_SIZE" -le "$STUB_MAX" ] || { echo "STUB TOO LARGE"; exit 1; }
    ${P}objdump -d "$B/stub.elf" | grep -E '^ *8[0-9a-f]+:' | cut -c1-70
    cat > "$B/layout.env" <<EOF
AGENT_VRAM=$VRAM
AGENT_ROM=$ROM
AGENT_MIN_RAM=$MINRAM
AGENT_LOAD_SIZE=$LOAD_SIZE
AGENT_BSS_START=$BSS_START
AGENT_BSS_END=$BSS_END
AGENT_MAGIC_ADDR=$MAGIC
AGENT_TICK=$TICK
AGENT_CHECK_END=$CHECK_END
AGENT_CHECK_SUM=$CHECK_SUM
STUB_VRAM=$STUB_VRAM
STUB_SIZE=$STUB_SIZE
OFF_MAGIC=$(off gAgentSegmentMagic)
OFF_INIT_ATTEMPTS=$(off s_init_attempts)
OFF_READY=$(off s_ready)
OFF_FRAMES_HANDLED=$(off s_frames_handled)
OFF_TICKS=$(off s_ticks)
OFF_REQUESTS=$(off s_requests)
OFF_ERRORS=$(off s_errors)
OFF_LAST_ERROR=$(off s_last_error)
EOF

    mkdir -p "$PROFILE"
    cp "$B/agent.bin" "$B/stub.bin" "$B/layout.env" "$PROFILE/"
    {
        echo "n64/agent from $REV"
        echo "built by crates/ap64-core/agent/build.sh $GAME, cart $CART ($(${P}gcc --version | head -1))"
        echo "agent.bin sha1 $(sha1sum < "$B/agent.bin" | cut -d' ' -f1)"
        echo "stub.bin  sha1 $(sha1sum < "$B/stub.bin" | cut -d' ' -f1)"
    } > "$PROFILE/BUILD_REV"
    echo "--- installed in $PROFILE"
    cat "$PROFILE/BUILD_REV"
}

rm -rf "$GDIR/build"
build_cart sc64 sc64 "" "$PROFILE_ROOT"
build_cart ed64 ed64 "-DAGENT_CART_ED64" "$PROFILE_ROOT/ed64"
build_cart ed64pro ed64pro "-DAGENT_CART_ED64PRO" "$PROFILE_ROOT/ed64pro"
