/*
 * Per-frame hook stub for Banjo-Tooie (US) seeds of Archipelago's banjo_tooie world.
 *
 * Everything here is in the randomizer's appended region, not the game's code: 99.6% of
 * Banjo-Tooie's ROM is compressed, so there is no instruction in it to retarget. The
 * randomizer appends 143,452 uncompressed bytes at rom 0x2000000 and its patched boot
 * segment DMAs them to 0x80400000, so vram = rom + 0x7E400000 for that region, and this
 * stub arrives in RAM on the randomizer's own loader without needing one of its own.
 *
 * Hook site: 0x8040ED04 (rom 0x200ED04), whose jal targets 0x80413628 -- a function that
 * is `jr ra; nop`, a deliberate no-op hook point the randomizer left. The stub calls it
 * anyway, so the frame runs exactly as before and the sha1 pin on it catches the day it
 * stops being a no-op.
 *
 * Rate: measured at 0.80 per frame in the worst 300-frame window and ~7 per frame on
 * average, across 21,000 frames and several mode changes. Worth stating both, because the
 * average is not the number that matters. A control site in the same region read
 * 0.34/frame and then fired once in 1,740 frames, so it is gated and useless; this one
 * never recorded a zero window. The floor is roughly twice DKR's working average.
 *
 * The jal's delay slot is `addiu a0, sp, 48`, an argument setup rather than something the
 * game needs -- and the target ignores a0, and the next call site reloads a0 from the
 * stack, so this stub clobbering a0-a3 for the DMA below is harmless.
 *
 * The copy uses the routine at 0x80001A40 in the uncompressed boot segment, which is
 * dma(a0 = direction, a1 = rom offset, a2 = dram address, a3 = length): a2 goes through
 * osVirtualToPhysical into PI_DRAM_ADDR, a1 is OR'd with the cart base held at 0x80000308
 * into PI_CART_ADDR, and a3 - 1 goes to PI_WR_LEN for a read (a0 = 0) or PI_RD_LEN for a
 * write. It polls PI_STATUS until the bus is idle before starting, so it will not tread on
 * a transfer already in flight -- which matters more here than usual, because this is the
 * only ROM copy Banjo-Tooie exposes in code the ROM actually contains.
 *
 * Symbols from the build (see build.sh): AGENT_VRAM, AGENT_ROM, AGENT_LOAD_SIZE,
 * agent_BSS_START, agent_BSS_END, AGENT_MAGIC_ADDR, agent_tick, and AGENT_MIN_RAM.
 *
 * The copy is never assumed to be synchronous -- that routine starts the DMA and returns
 * without waiting -- so after it returns the magic word is checked again, and the agent's
 * BSS is zeroed and the agent run only once it reads back. Until then the stub returns and
 * tries again next pass.
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
        CALL    0x80413628              /* the call this jal replaced; a no-op today */
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

        /* Not loaded: dma(OS_READ, rom_offset, dst, size). */
        move    $a0, $zero              /* direction 0 = cart -> RDRAM */
        lui     $a1, %hi(AGENT_ROM)
        addiu   $a1, $a1, %lo(AGENT_ROM)
        lui     $a2, %hi(AGENT_VRAM)
        addiu   $a2, $a2, %lo(AGENT_VRAM)
        lui     $a3, %hi(AGENT_LOAD_SIZE)
        addiu   $a3, $a3, %lo(AGENT_LOAD_SIZE)
        CALL    0x80001A40
        nop

        lui     $t1, 0x4D36
        ori     $t1, $t1, 0x3450
        lui     $t0, %hi(AGENT_MAGIC_ADDR)
        lw      $t0, %lo(AGENT_MAGIC_ADDR)($t0)
        bne     $t0, $t1, done          /* copy not visible yet: next pass */
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
