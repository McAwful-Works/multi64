/*
 * Per-frame hook stub for Castlevania: Legacy of Darkness (US), written into the tail of
 * the dead debug block at 0x8001E9C0 and reached by retargeting one existing jal.
 *
 * Hook site: the main thread's loop at 0x80017C78 opens with `jal osRecvMesg` at
 * 0x80017C84, blocking until the next message; the loop then switches on the message type
 * and branches back. Retargeting that one call runs the agent once per message received,
 * on the game's own thread, from the first message onwards. The stub calls osRecvMesg
 * first with a0-a2 untouched, so the game's frame proceeds exactly as before.
 *
 * Unlike CV64's stub, this one saves v0: the call it replaces returns a value, and the
 * ROM copy on the load path would otherwise clobber it. This caller happens to ignore v0
 * -- it reads the message out of its own stack slot -- but a stub that quietly changes a
 * function's return value is not worth the saving of two instructions.
 *
 * Symbols from the build (see build.sh): AGENT_VRAM, AGENT_ROM, AGENT_LOAD_SIZE,
 * agent_BSS_START, agent_BSS_END, AGENT_MAGIC_ADDR, agent_tick, and AGENT_MIN_RAM.
 *
 * The copy is never assumed to be synchronous: after the copy returns, the magic word is
 * checked again, and the agent's BSS is zeroed and the agent run only once it reads back.
 * Until then the stub returns and tries again next time round the loop.
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
        addiu   $sp, $sp, -32
        sw      $ra, 28($sp)
        CALL    0x80096CE0              /* osRecvMesg, the call this jal replaced */
        nop
        sw      $v0, 24($sp)            /* its return value, restored on every path */

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

        /* Not loaded: the game's ROM copy routine, (rom_offset, dst, size). */
        lui     $a0, %hi(AGENT_ROM)
        addiu   $a0, $a0, %lo(AGENT_ROM)
        lui     $a1, %hi(AGENT_VRAM)
        addiu   $a1, $a1, %lo(AGENT_VRAM)
        lui     $a2, %hi(AGENT_LOAD_SIZE)
        addiu   $a2, $a2, %lo(AGENT_LOAD_SIZE)
        CALL    0x8001A42C
        nop

        lui     $t1, 0x4D36
        ori     $t1, $t1, 0x3450
        lui     $t0, %hi(AGENT_MAGIC_ADDR)
        lw      $t0, %lo(AGENT_MAGIC_ADDR)($t0)
        bne     $t0, $t1, done          /* copy not visible yet: next time round */
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
        lw      $v0, 24($sp)
        lw      $ra, 28($sp)
        jr      $ra
        addiu   $sp, $sp, 32
