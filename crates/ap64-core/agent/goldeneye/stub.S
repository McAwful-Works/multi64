/*
 * Per-frame hook stub for GoldenEye 007 (US) as its Archipelago world builds it, written over
 * a libultra Controller Pak file function at 0x7001F38C, which nothing calls (see game.env),
 * and reached by retargeting one existing jal.
 *
 * It is in the boot/lib segment because that segment stays resident: the game segment at
 * 0x7F000000 is demand-paged through the TLB, so a stub there loses what it writes. The
 * image check's state is written through the KSEG0 alias of its words (0x8001F...), which
 * is the same physical RAM as the 0x7001F... address the code runs at, so no TLB mapping
 * decides whether the store lands.
 *
 * Hook site: bossMainloop's `jal joyConsumeSamplesWrapper` at 0x700064F0, the first call of
 * each pass of the main loop after a retrace, ahead of the game's update. joyConsumeSamples-
 * Wrapper (0x7000BD0C) takes no arguments, and the loop calls permit_stderr straight after,
 * so nothing reads its result. Measured: 5,919 calls over 9,390 frames, exactly the rate of
 * the loop's lvlRender call, so it is unconditional; 0.63 per video frame, which is
 * GoldenEye's variable frame rate. The stub calls joyConsumeSamplesWrapper first, so the
 * game's frame runs exactly as before. The world does not touch bossMainloop.
 *
 * It runs on the game's main thread, at the loop's own depth.
 *
 * Symbols from the build (see build.sh): AGENT_VRAM, AGENT_ROM, AGENT_LOAD_SIZE,
 * agent_BSS_START, agent_BSS_END, AGENT_MAGIC_ADDR, agent_tick, and AGENT_MIN_RAM.
 *
 * The agent is copied by the game's romCopy(target, source, size) at 0x70005C1C, which
 * invalidates the D-cache over the target, starts the DMA through the PI manager and waits
 * for it on its own queue. It does not touch the I-cache, so the stub invalidates the
 * I-cache over the agent itself before running it: emulators do not model the I-cache, and
 * a console runs stale lines. The copy is still never assumed to land: the magic word is
 * checked again afterwards, and the agent's BSS is zeroed and the agent run only once it
 * reads back.
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
/* Two entries at fixed offsets, so the profile can name them without the build's layout:
   stub+0 for the frame hook's jal, stub+8 for the j that replaces the world's loader. */
agent_hook_stub:
        b       hook_main
        nop
world_loader_entry:
        b       world_loader
        nop
        .if     (world_loader_entry - agent_hook_stub) - 8
        .error  "the world loader's entry must be stub+8: the profile's j targets it"
        .endif

hook_main:
        addiu   $sp, $sp, -24
        sw      $ra, 20($sp)
        CALL    0x7000BD0C              /* joyConsumeSamplesWrapper, the call this jal replaced */
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

        /* Not loaded: romCopy(target, rom_offset, size). */
        lui     $a1, %hi(AGENT_ROM)
        addiu   $a1, $a1, %lo(AGENT_ROM)
        lui     $a0, %hi(AGENT_VRAM)
        addiu   $a0, $a0, %lo(AGENT_VRAM)
        lui     $a2, %hi(AGENT_LOAD_SIZE)
        addiu   $a2, $a2, %lo(AGENT_LOAD_SIZE)
        CALL    0x70005C1C
        nop

        lui     $t1, 0x4D36
        ori     $t1, $t1, 0x3450
        lui     $t0, %hi(AGENT_MAGIC_ADDR)
        lw      $t0, %lo(AGENT_MAGIC_ADDR)($t0)
        bne     $t0, $t1, done          /* copy not visible yet: next frame */
        nop

        /* I-cache over the agent: 32-byte lines. */
        lui     $t0, %hi(AGENT_VRAM)
        addiu   $t0, $t0, %lo(AGENT_VRAM)
        lui     $t1, %hi(agent_BSS_START)
        addiu   $t1, $t1, %lo(agent_BSS_START)
icache:
        cache   0x10, 0($t0)            /* Hit_Invalidate_I */
        sltu    $t2, $t0, $t1
        bnez    $t2, icache
        addiu   $t0, $t0, 32

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

/*
 * The world's payload loader, done safely. The world's own (0x7F009A34, in the paged game
 * segment) is entered with `j` from each of its hooks, with the real target in $t9 and the
 * hooked call's arguments still in $a0-$a3 and on the caller's stack. Once, it copies the
 * world's 2,944-byte payload from ROM 0xBFB000 to 0x8007F400 (inside the game's BSS) with
 * CPU loads from the cartridge, then jumps to $t9. On a console those loads can collide
 * with a PI DMA the game has in flight (audio streams from ROM), and the stores went
 * through the D-cache straight into a jump. The profile turns the loader's first two
 * instructions into `j` here instead.
 *
 * Same contract: if the payload's first word is still zero (the game's BSS clear zeroed
 * it), copy it with the game's romCopy, which invalidates the D-cache over the target and
 * DMAs through the PI manager, then invalidate the I-cache over it, since it is code about
 * to run. Every register the hooked call might still need is put back, and the stack is
 * where it was, before the jump to $t9.
 */
world_loader:
        lui     $t0, 0x8008
        lw      $t0, -0x0C00($t0)       /* 0x8007F400: the payload's first word */
        bnez    $t0, 2f
        nop
        addiu   $sp, $sp, -40
        sw      $a0, 16($sp)
        sw      $a1, 20($sp)
        sw      $a2, 24($sp)
        sw      $a3, 28($sp)
        sw      $ra, 32($sp)
        sw      $t9, 36($sp)
        lui     $a0, 0x8008
        addiu   $a0, $a0, -0x0C00       /* target 0x8007F400 */
        lui     $a1, 0x00C0
        addiu   $a1, $a1, -0x5000       /* ROM 0xBFB000 */
        CALL    0x70005C1C              /* romCopy(target, rom, size) */
        addiu   $a2, $zero, 0x0B80      /* 2,944 bytes */
        lui     $t0, 0x8008
        addiu   $t0, $t0, -0x0C00
        addiu   $t1, $t0, 0x0B80
1:      cache   0x10, 0($t0)            /* Hit_Invalidate_I, 32-byte lines */
        sltu    $t2, $t0, $t1
        bnez    $t2, 1b
        addiu   $t0, $t0, 32
        lw      $a0, 16($sp)
        lw      $a1, 20($sp)
        lw      $a2, 24($sp)
        lw      $a3, 28($sp)
        lw      $ra, 32($sp)
        lw      $t9, 36($sp)
        addiu   $sp, $sp, 40
2:      jr      $t9
        nop

/* The image check's state, IMAGE_CHECK_STATE's three words, addressed through KSEG0. */
        .align  2
image_check_words:
        .word   0, 0, 0
        .set    image_check, image_check_words + 0x10000000
