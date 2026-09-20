/*
 * Per-frame hook stub for Ocarina of Time (NTSC 1.0) seeds of Archipelago's OoT world.
 *
 * The game calls the OoTR payload's before_game_state_update_hook once per frame, from
 * code at 0x8009CAD4, with the game state in a0. That jal is retargeted here. The stub
 * calls the hook first, a0 untouched, so the frame runs exactly as before, then ticks
 * the agent.
 *
 * Nothing is loaded here: the game's payload loader was pointed at a file that holds the
 * payload, this stub and the agent with its BSS as zeros, so all of it is in RAM from boot.
 * The load marker is still read every frame, as the stubs that do their own copying read
 * it (AGENT_MAGIC_ADDR, n64/agent/templates/segment_magic.c). Being loaded at boot says
 * the agent arrived; it does not say it is still there. Nothing reserves that RAM from
 * the game, and without this check a frame that found something else in it would call
 * into whatever that is, which on a console is a hard lock with no way back. With it,
 * the agent simply stops answering and AP64 says so.
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
        CALL    0x80401D88              /* before_game_state_update_hook, a0 intact */
        nop

        /* Run the agent only while its image is intact. */
        lui     $t1, 0x4D36
        ori     $t1, $t1, 0x3450        /* 'M64P' */
        lui     $t0, %hi(AGENT_MAGIC_ADDR)
        lw      $t0, %lo(AGENT_MAGIC_ADDR)($t0)
        bne     $t0, $t1, done
        nop

        CALL    agent_tick
        nop
done:
        lw      $ra, 20($sp)
        jr      $ra
        addiu   $sp, $sp, 24
