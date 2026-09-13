/*
 * Per-frame hook stub for an agent linked as a flat image (templates/flat.ld).
 *
 * Reached by retargeting ONE existing `jal` that the game makes once per frame on its
 * own thread, to this stub. The stub makes the call it replaced first, so the game's
 * frame runs exactly as before, then services the agent. See
 * docs/integration/placing-the-agent.md.
 *
 * Symbols, all supplied by templates/link-flat.sh:
 *   HOOK_ORIGINAL     address the retargeted jal used to call
 *   ROM_COPY          the game's own ROM-to-RAM copy routine, taking
 *                     (rom_offset, dst, size) in a0, a1, a2 -- check this against
 *                     the game: routines that take a virtual ROM address or a file
 *                     index need a different argument setup here
 *   AGENT_ROM         ROM offset of the agent image
 *   AGENT_VRAM        RAM address the image was linked at
 *   AGENT_LOAD_SIZE   bytes of .text + .rodata + .data
 *   AGENT_MAGIC_ADDR  gAgentSegmentMagic
 *   AGENT_MIN_RAM     skip everything below this osMemSize (0x800000 when the agent
 *                     lives in the Expansion Pak, 0 when it does not)
 *   agent_BSS_START, agent_BSS_END, agent_tick   from the agent image
 *
 * Assumptions to check for your hook site: the arguments of HOOK_ORIGINAL arrive
 * intact (they are loaded before the jal, or in its delay slot), and the caller treats
 * the call as clobbering caller-saved registers, as any jal does.
 *
 * The copy is never assumed to be synchronous: after ROM_COPY returns, the magic word
 * is checked again, and BSS is zeroed and the agent run only once it reads back.
 * Until then the stub returns and tries again next frame.
 *
 * Every call goes through jalr $t9 rather than jal: symbols imported with
 * --just-symbols arrive sign-extended to 64 bits, and an R_MIPS_26 against them does
 * not fit ("relocation truncated to fit").
 */
        .set    noreorder
        .set    noat

        .macro  CALL target
        lui     $t9, %hi(\target)
        addiu   $t9, $t9, %lo(\target)
        jalr    $t9
        .endm

        .text
        .globl  agent_hook_stub
agent_hook_stub:
        addiu   $sp, $sp, -24
        sw      $ra, 20($sp)
        CALL    HOOK_ORIGINAL           /* the call the jal replaced; args intact */
        nop

        /* Memory guard: skip entirely below AGENT_MIN_RAM. */
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

        /* Not loaded: ROM_COPY(rom_offset, dst, size). */
        lui     $a0, %hi(AGENT_ROM)
        addiu   $a0, $a0, %lo(AGENT_ROM)
        lui     $a1, %hi(AGENT_VRAM)
        addiu   $a1, $a1, %lo(AGENT_VRAM)
        lui     $a2, %hi(AGENT_LOAD_SIZE)
        addiu   $a2, $a2, %lo(AGENT_LOAD_SIZE)
        CALL    ROM_COPY
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
