/*
 * Per-frame hook stub for Mario Kart 64 (US), written into dead code at 0x8003DC50 and
 * reached by retargeting one existing jal.
 *
 * Hook site: thread5_game_loop's `jal display_and_vsync` at 0x800028B4, the last of four
 * calls in the loop body and the only one with no arguments. It is the third of three
 * sites measured unconditional -- 17,741 calls against a control's 17,741 over 36,210
 * frames -- and running last means the agent sees the frame's state already updated.
 *
 * The obvious site is not available. Archipelago's MK64 basepatch hooks the game loop
 * itself, replacing its back-edge `beq $zero,$zero,-21` at 0x800028BC with a
 * `j 0x80404180` into its own block. Do not try to hook there.
 *
 * Nor is the site next door a substitute: `jal update_gamestate` at 0x80002884 sits in
 * the same loop body, is named like a per-frame update, and fired 3 times in 36,210
 * frames because it is branched over. The loop itself runs at ~0.45-0.49 per video frame,
 * about 29 Hz, not once a frame.
 *
 * Symbols from the build (see build.sh): AGENT_VRAM, AGENT_ROM, AGENT_LOAD_SIZE,
 * agent_BSS_START, agent_BSS_END, AGENT_MAGIC_ADDR, agent_tick, and AGENT_MIN_RAM.
 *
 * I-CACHE: dma_copy invalidates the data cache only. MK64 loads its own executable
 * segments as dma_copy then osInvalICache then osInvalDCache -- see init_segment_racing
 * and init_segment_ending_sequences, which agree instruction for instruction -- so this
 * does the same. Skipping osInvalICache would copy the agent and then run whatever stale
 * lines the I-cache held, which emulators do not model and a console does.
 *
 * Argument order differs from other profiles: MK64's dma_copy is (dest, romAddr, size),
 * not (rom, dst, size).
 *
 * The copy is not assumed to land synchronously: the magic word is re-read afterwards,
 * and BSS is zeroed and the agent run only once it reads back. Until then the stub
 * returns and tries again next iteration.
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
        CALL    0x80000F34              /* display_and_vsync: the call this jal replaced */
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

        /* Not loaded: dma_copy(dest, romAddr, size). */
        lui     $a0, %hi(AGENT_VRAM)
        addiu   $a0, $a0, %lo(AGENT_VRAM)
        lui     $a1, %hi(AGENT_ROM)
        addiu   $a1, $a1, %lo(AGENT_ROM)
        lui     $a2, %hi(AGENT_LOAD_SIZE)
        addiu   $a2, $a2, %lo(AGENT_LOAD_SIZE)
        CALL    0x80001158
        nop

        /* osInvalICache(AGENT_VRAM, AGENT_LOAD_SIZE): dma_copy does D-cache only. */
        lui     $a0, %hi(AGENT_VRAM)
        addiu   $a0, $a0, %lo(AGENT_VRAM)
        lui     $a1, %hi(AGENT_LOAD_SIZE)
        addiu   $a1, $a1, %lo(AGENT_LOAD_SIZE)
        CALL    0x800CDB00
        nop

        /* osInvalDCache(AGENT_VRAM, AGENT_LOAD_SIZE), as the game does after its own. */
        lui     $a0, %hi(AGENT_VRAM)
        addiu   $a0, $a0, %lo(AGENT_VRAM)
        lui     $a1, %hi(AGENT_LOAD_SIZE)
        addiu   $a1, $a1, %lo(AGENT_LOAD_SIZE)
        CALL    0x800CDB80
        nop

        lui     $t1, 0x4D36
        ori     $t1, $t1, 0x3450
        lui     $t0, %hi(AGENT_MAGIC_ADDR)
        lw      $t0, %lo(AGENT_MAGIC_ADDR)($t0)
        bne     $t0, $t1, done          /* copy not visible yet: next iteration */
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
