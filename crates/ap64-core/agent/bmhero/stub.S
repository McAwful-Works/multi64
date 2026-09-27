/*
 * Per-frame hook stub for Bomberman Hero (US), written over Debug_BackupMemTest
 * (src/boot/cont_pak.c, 0x80020550), which nothing calls, and reached by retargeting one
 * existing jal.
 *
 * Hook site: the frame loop's `jal func_8001D9E4` at 0x80000ACC, in func_80000964. Every
 * screen runs through that loop -- the title, the menus, every overlay and every level -- and
 * func_8001D9E4 builds each frame, calling the screen's routine through gDebugRoutine1. It
 * is its only caller, measured at exactly the function's own rate (48,536 of 57,210 frames;
 * the rest are loads and frames skipped while two display lists are pending). The jal's
 * delay slot sets $a0 to the frame buffer, which the stub leaves alone and passes on by
 * calling func_8001D9E4 first, so the game's frame runs exactly as before.
 *
 * It runs on the game's thread (thread 6), whose stack is 4 KB. The agent's deepest call
 * chain is well under 1 KB, every frame in it fixed in size (-fstack-usage), and it runs at
 * the depth func_8001D9E4 does, after that function's own calls have returned.
 *
 * Symbols from the build (see build.sh): AGENT_VRAM, AGENT_ROM, AGENT_LOAD_SIZE,
 * agent_BSS_START, agent_BSS_END, AGENT_MAGIC_ADDR, agent_tick, and AGENT_MIN_RAM.
 *
 * The agent is copied by the boot code's load_from_rom_to_addr(rom, dst, size) at
 * 0x8000068C, the routine that loads the game segment and every overlay: it writes back and
 * invalidates the caches and waits for each DMA. The copy is still never assumed to be
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
        CALL    0x8001D9E4              /* func_8001D9E4($a0), the call this jal replaced */
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

        /* Not loaded: load_from_rom_to_addr(rom_offset, dst, size). */
        lui     $a0, %hi(AGENT_ROM)
        addiu   $a0, $a0, %lo(AGENT_ROM)
        lui     $a1, %hi(AGENT_VRAM)
        addiu   $a1, $a1, %lo(AGENT_VRAM)
        lui     $a2, %hi(AGENT_LOAD_SIZE)
        addiu   $a2, $a2, %lo(AGENT_LOAD_SIZE)
        CALL    0x8000068C
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
