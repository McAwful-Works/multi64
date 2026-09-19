/*
 * Per-frame hook stub for Paper Mario (US 1.0) seeds of the Paper Mario Randomizer,
 * written over update_camera_unused_ahead (0x8002FCE0, ROM 0xB0E0, 800 B), which vanilla
 * never calls (CAM_UPDATE_UNUSED_AHEAD has no case in update_cameras), and reached by
 * retargeting gfxRetrace_Callback's `jal step_game_loop` (ROM 0x151C).
 *
 * That callback runs on nuGfxThread, not an interrupt, and calls step_game_loop once per
 * game frame (every other retrace). The stub calls it first, so the frame runs exactly as
 * before, then keeps the agent: loads it into Expansion Pak RAM on first use with the
 * game's own dma_copy(romStart, romEnd, vramDest), and ticks it.
 *
 * Hand-written rather than compiled from the decomp, so none of the game's code is carried.
 *
 * Symbols from the build (build.sh): AGENT_VRAM, AGENT_ROM, AGENT_LOAD_SIZE,
 * agent_BSS_START, agent_BSS_END, AGENT_MAGIC_ADDR, agent_tick, AGENT_MIN_RAM.
 *
 * The copy is never assumed to have finished: the magic word is checked again after
 * dma_copy returns, and BSS is zeroed and the agent run only once it reads back.
 *
 * Every call goes through jalr $t9 rather than jal: the agent's symbols arrive via
 * --just-symbols sign-extended to 64 bits, and an R_MIPS_26 against them does not fit.
 */
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
        CALL    0x80026740              /* step_game_loop(), the call this jal replaced */
        nop

        /* Expansion Pak guard: skip entirely below AGENT_MIN_RAM. */
        lui     $t0, 0x8000
        lw      $t0, 0x318($t0)         /* osMemSize */
        lui     $t1, %hi(AGENT_MIN_RAM)
        addiu   $t1, $t1, %lo(AGENT_MIN_RAM)
        sltu    $t0, $t0, $t1
        bnez    $t0, done
        nop

        lui     $t1, 0x4D36
        ori     $t1, $t1, 0x3450        /* 'M64P' */
        lui     $t0, %hi(AGENT_MAGIC_ADDR)
        lw      $t0, %lo(AGENT_MAGIC_ADDR)($t0)
        beq     $t0, $t1, tick
        nop

        /* Not loaded: dma_copy(romStart, romEnd, vramDest). */
        lui     $a0, %hi(AGENT_ROM)
        addiu   $a0, $a0, %lo(AGENT_ROM)
        lui     $a1, %hi(AGENT_LOAD_SIZE)
        addiu   $a1, $a1, %lo(AGENT_LOAD_SIZE)
        addu    $a1, $a1, $a0
        lui     $a2, %hi(AGENT_VRAM)
        addiu   $a2, $a2, %lo(AGENT_VRAM)
        CALL    0x8002973C              /* dma_copy */
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
        CALL    agent_tick
        nop
done:
        lw      $ra, 20($sp)
        jr      $ra
        addiu   $sp, $sp, 24
