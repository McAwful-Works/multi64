/*
 * Per-frame hook stub for Donkey Kong 64 (US) as the DK64 randomizer builds it, written
 * into dead Controller Pak code at 0x8000D180 and reached by retargeting one existing jal.
 * Measurements and the full record: #309.
 *
 * Hook site: func_global_asm_805FC2B0 (the frame loop; the randomizer calls it
 * regularframeloop) calls func_global_asm_8070DD44 at 0x805FC4CC. It takes no arguments,
 * and the caller overwrites v0 before reading it, so a stub in front of it has nothing to
 * forward and nothing to preserve. Counted over 37,800 video frames on the interpreter:
 * 18,113 calls against the loop entry's 18,113, so it is unconditional. The loop runs at
 * ~0.48 per video frame (about 29 Hz), like Mario Kart 64's and Diddy Kong Racing's.
 *
 * The randomizer hooks this same loop at 0x805FC3FC (jal 0x805FC668 -> j 0x805C2070, its
 * earlyframecode). Do not hook there.
 *
 * ROM copy: func_global_asm_805FB750 is the game's synchronous DMA, (romOffset, size,
 * dest). It writes back D-cache, runs osPiStartDma through the PI manager, blocks on its
 * queue (0x807655F0, shared with every other game loader, all of which use it the same
 * way), then invalidates D-cache. It adds a base it keeps at 0x8000DDCC (0x101C50 on the
 * seed measured) to the offset, so the stub reads that base and subtracts it. Nothing is
 * assumed about its value.
 *
 * Not dmaFileTransfer (0x80000450), which the randomizer uses at boot. It starts a raw PI
 * DMA outside the PI manager. That is fine before the manager is running, but during play
 * its completion interrupt could be taken as the end of the game's next DMA.
 *
 * I-CACHE: the copy handles the D-cache only, so the stub invalidates the I-cache over
 * the agent itself. Skipping that would run stale lines, which emulators do not model
 * and a console does.
 *
 * The copy is not assumed to land: the magic word is re-read afterwards, and BSS is zeroed
 * and the agent run only once it reads back. Until then the stub returns and tries again.
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
        CALL    0x8070DD44              /* the call this jal replaced */
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

        /* Not loaded: func_global_asm_805FB750(AGENT_ROM - base, AGENT_LOAD_SIZE, AGENT_VRAM). */
        lui     $t0, 0x8001
        lw      $t0, -0x2234($t0)       /* the copy's ROM base, at 0x8000DDCC */
        lui     $a0, %hi(AGENT_ROM)
        addiu   $a0, $a0, %lo(AGENT_ROM)
        subu    $a0, $a0, $t0
        lui     $a1, %hi(AGENT_LOAD_SIZE)
        addiu   $a1, $a1, %lo(AGENT_LOAD_SIZE)
        lui     $a2, %hi(AGENT_VRAM)
        addiu   $a2, $a2, %lo(AGENT_VRAM)
        CALL    0x805FB750
        nop

        /* osInvalICache(AGENT_VRAM, AGENT_LOAD_SIZE): the copy does D-cache only. */
        lui     $a0, %hi(AGENT_VRAM)
        addiu   $a0, $a0, %lo(AGENT_VRAM)
        lui     $a1, %hi(AGENT_LOAD_SIZE)
        addiu   $a1, $a1, %lo(AGENT_LOAD_SIZE)
        CALL    0x80005260
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
        CALL    agent_tick
        nop
done:
        lw      $ra, 20($sp)
        jr      $ra
        addiu   $sp, $sp, 24
