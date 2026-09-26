/*
 * Per-frame hook stub for Kirby 64: The Crystal Shards (US), written into a run of zeros
 * in the `main` segment's data at 0x8003BD0C and reached by retargeting one existing jal.
 *
 * Hook site: `jal gtlScheduleGfxEnd` at 0x800059C0, which closes out the frame's graphics
 * task, so the agent runs after the frame's work. Both the site and the stub are in
 * `main`, resident for the whole run.
 *
 * It was measured, not reasoned about. tools/count-calls.lua puts an execution hook on
 * each candidate and counts it against the frame counter: this site ran 897 times in 1140
 * frames, while overlay 1's game loop -- which looked like the frame loop, 39 branches
 * returning to its head -- ran once in three thousand. A loop is not the frame loop, and
 * two of gtlScheduleGfxEnd's three callers never run at all, so counting the function
 * rather than the call site would have been just as wrong.
 *
 * Kirby 64's ROM copy takes a cart-window address rather than a file offset -- the game
 * calls it as dma_read(0xB0000B70, dst, 0x100) at boot. AGENT_ROM stays the plain offset
 * the splicer writes the image at, and the 0xB0000000 is added here, so profile.toml and
 * layout.env keep agreeing about where the agent lives.
 *
 * Symbols from the build (see build.sh): AGENT_VRAM, AGENT_ROM, AGENT_LOAD_SIZE,
 * agent_BSS_START, agent_BSS_END, AGENT_MAGIC_ADDR, agent_tick, and AGENT_MIN_RAM.
 *
 * The copy is never assumed to be synchronous: after dma_read returns, the magic word is
 * checked again, and the agent's BSS is zeroed and the agent run only once it reads back.
 * Until then the stub returns and tries again next frame.
 *
 * Every call goes through jalr $t9 rather than jal: the agent's symbols arrive via
 * --just-symbols sign-extended to 64 bits, and an R_MIPS_26 against them does not fit.
 */
#include "image_check.inc"
        .macro  CALL target
        lui     $t9, %hi(\target)
        addiu   $t9, $t9, %lo(\target)
        jalr    $t9
        .endm
        .set    noreorder
        .set    noat
        .text
        .globl  agent_hook_stub
agent_hook_stub:
        addiu   $sp, $sp, -24
        sw      $ra, 20($sp)
        CALL    0x80005910              /* gtlScheduleGfxEnd, the call this jal replaced */
        nop

        /* Expansion Pak guard: skip entirely below AGENT_MIN_RAM. */
        lui     $t0, 0x8000
        lw      $t0, 0x318($t0)         /* osMemSize */
        lui     $t1, %hi(AGENT_MIN_RAM)
        addiu   $t1, $t1, %lo(AGENT_MIN_RAM)
        sltu    $t0, $t0, $t1
        bnez    $t0, done
        nop
        IMAGE_STOOD_DOWN done           /* the image check stood the stub down */

        lui     $t1, 0x4D36
        ori     $t1, $t1, 0x3450        /* 'M64P' */
        lui     $t0, %hi(AGENT_MAGIC_ADDR)
        lw      $t0, %lo(AGENT_MAGIC_ADDR)($t0)
        beq     $t0, $t1, tick
        nop

        /* Not loaded: dma_read(cart_addr, dst, size), cart window not file offset. */
        lui     $a0, %hi(AGENT_ROM)
        addiu   $a0, $a0, %lo(AGENT_ROM)
        lui     $t2, 0xB000
        or      $a0, $a0, $t2
        lui     $a1, %hi(AGENT_VRAM)
        addiu   $a1, $a1, %lo(AGENT_VRAM)
        lui     $a2, %hi(AGENT_LOAD_SIZE)
        addiu   $a2, $a2, %lo(AGENT_LOAD_SIZE)
        CALL    0x80002E48
        nop

        lui     $t1, 0x4D36
        ori     $t1, $t1, 0x3450
        lui     $t0, %hi(AGENT_MAGIC_ADDR)
        lw      $t0, %lo(AGENT_MAGIC_ADDR)($t0)
        bne     $t0, $t1, done          /* copy not visible yet: next frame */
        nop

        /* Loaded just now: zero BSS once. */
        lui     $t0, %hi(agent_BSS_START)
        addiu   $t0, $t0, %lo(agent_BSS_START)
        lui     $t1, %hi(agent_BSS_END)
        addiu   $t1, $t1, %lo(agent_BSS_END)
zero:
        beq     $t0, $t1, tick
        nop
        sw      $zero, 0($t0)
        b       zero
        addiu   $t0, $t0, 4

tick:
        IMAGE_CHECK done                /* the next slice of the agent's code */
        CALL    agent_tick
        nop
done:
        lw      $ra, 20($sp)
        jr      $ra
        addiu   $sp, $sp, 24

        IMAGE_CHECK_STATE
