/*
 * The agent image check, shared by every stub (issue #292).
 *
 * Profile pins cover ROM bytes, but the agent runs from RAM, and a randomizer update that
 * starts using more RAM can overwrite it with every pin still passing. Most of the image
 * is not the magic word the stub already checks, so such an overwrite used to be
 * executed. Each tick, before calling agent_tick, the stub adds up the next CHECK_SLICE
 * bytes of the agent's .text and .rodata. At the end of each pass it compares the total
 * with AGENT_CHECK_SUM, which build.sh works out from agent.bin. On a mismatch the stub
 * stands down for good: it never calls the agent again and never reloads it, since
 * whatever overwrote the image now owns that RAM. The link to the cart falls silent.
 *
 * .data and .bss are left out: they change as the agent runs. A slice keeps the cost of a
 * tick small (256 loads, about 50 us), and a pass over the ~8.5 KB takes about nine ticks.
 *
 * Symbols from build.sh: AGENT_VRAM, AGENT_CHECK_END (AGENT_VRAM plus the bytes checked,
 * a multiple of 4) and AGENT_CHECK_SUM (their big-endian words added, modulo 2^32).
 * Uses $t0-$t6. Needs `.set noreorder` and `.set noat`.
 */
        .set    CHECK_SLICE, 1024

/* Return to `out` if the stub has stood down. Put it before anything that touches the
   agent's RAM, including a reload. */
        .macro  IMAGE_STOOD_DOWN out
        lui     $t0, %hi(image_check + 8)
        lw      $t0, %lo(image_check + 8)($t0)
        bnez    $t0, \out
        nop
        .endm

/* Check the next slice. Falls through to call the agent; jumps to `out` on a mismatch. */
        .macro  IMAGE_CHECK out
        lui     $t3, %hi(image_check)
        addiu   $t3, $t3, %lo(image_check)
        lw      $t0, 0($t3)             /* where this slice starts; 0 starts a pass */
        lw      $t1, 4($t3)             /* the pass's total so far */
        lui     $t5, %hi(AGENT_CHECK_END)
        bnez    $t0, 1f
        addiu   $t5, $t5, %lo(AGENT_CHECK_END)
        lui     $t0, %hi(AGENT_VRAM)
        addiu   $t0, $t0, %lo(AGENT_VRAM)
1:      addiu   $t4, $t0, CHECK_SLICE
        sltu    $t6, $t5, $t4
        beqz    $t6, 2f
        nop
        or      $t4, $t5, $zero         /* the last slice stops at the end */
2:      lw      $t6, 0($t0)
        addiu   $t0, $t0, 4
        bne     $t0, $t4, 2b
        addu    $t1, $t1, $t6
        bne     $t0, $t5, 3f            /* the pass goes on next tick */
        sw      $t0, 0($t3)
        lui     $t6, %hi(AGENT_CHECK_SUM)
        addiu   $t6, $t6, %lo(AGENT_CHECK_SUM)
        sw      $zero, 0($t3)           /* the next pass starts over */
        beq     $t1, $t6, 3f
        or      $t1, $zero, $zero
        addiu   $t6, $zero, 1
        sw      $t6, 8($t3)             /* stand down */
        b       \out
        nop
3:      sw      $t1, 4($t3)
        .endm

/* The check's state: where the pass is, its total, and whether the stub stood down. In the
   stub's own space, which is RAM that nothing else uses, so it survives between ticks. */
        .macro  IMAGE_CHECK_STATE
        .balign 4
        .globl  image_check
image_check:
        .word   0, 0, 0
        .endm
