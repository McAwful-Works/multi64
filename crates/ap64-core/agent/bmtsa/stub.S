/*
 * Per-frame hook stub for Bomberman 64: The Second Attack! (US), written over
 * bmLoadBitmapTile (0x8000BC7C), which nothing calls, and reached by retargeting one
 * existing jal.
 *
 * Hook site: systemmain's `jal CallProcess` at 0x80001454, whose delay slot sets $a0 to 1.
 * CallProcess (0x800077B4) runs every Hudson process once. The main loop reaches it once
 * per game frame, every second retrace, measured at exactly the function's own rate
 * (20,885 calls over 45,420 frames). The only thing that skips it is a fatal-error flag. The
 * stub leaves $a0 alone and calls CallProcess first, so the game's frame runs exactly as
 * before. CallProcess switches between processes with setjmp and longjmp, but only within
 * its own frame and the processes' own stacks, and it returns here normally.
 *
 * It runs on the main thread, whose stack is 16 KB (systemmain is created with its top at
 * 0x800A0100). The agent's deepest call chain is well under 1 KB.
 *
 * Symbols from the build (see build.sh): AGENT_VRAM, AGENT_ROM, AGENT_LOAD_SIZE,
 * agent_BSS_START, agent_BSS_END, AGENT_MAGIC_ADDR, agent_tick, and AGENT_MIN_RAM.
 *
 * The agent is copied by the boot code's dmaRead(dst, size, rom) at 0x80000698, the routine
 * the game loads its files with: it takes the game's own DMA lock, writes back and
 * invalidates the caches and waits for the DMA. The copy is still never assumed to be
 * synchronous: the magic word is checked again after it returns, and the agent's BSS is
 * zeroed and the agent run only once it reads back.
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
        CALL    0x800077B4              /* CallProcess($a0), the call this jal replaced */
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

        /* Not loaded: dmaRead(dst, size, rom_offset). */
        lui     $a0, %hi(AGENT_VRAM)
        addiu   $a0, $a0, %lo(AGENT_VRAM)
        lui     $a1, %hi(AGENT_LOAD_SIZE)
        addiu   $a1, $a1, %lo(AGENT_LOAD_SIZE)
        lui     $a2, %hi(AGENT_ROM)
        addiu   $a2, $a2, %lo(AGENT_ROM)
        CALL    0x80000698
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
