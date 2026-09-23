/*
 * Per-frame hook stub for Banjo-Tooie (US) seeds of Archipelago's banjo_tooie world.
 *
 * Everything here is in the randomizer's appended region, not the game's code: 99.6% of
 * Banjo-Tooie's ROM is compressed, so there is no instruction in it to retarget. The
 * randomizer appends 143,452 uncompressed bytes at rom 0x2000000 and its patched boot
 * segment DMAs 0x100001 of them to 0x80400000, so vram = rom + 0x7E400000 for that region.
 *
 * Hook site: 0x8040ED04 (rom 0x200ED04), whose jal targets 0x80413628 -- a function that
 * is `jr ra; nop`, a deliberate no-op hook point the randomizer left. The stub calls it
 * anyway, so the frame runs exactly as before and the sha1 pin on it catches the day it
 * stops being a no-op.
 *
 * Rate: 0.80 per frame in the worst 300-frame window and ~9 per frame on average, across
 * 41,400 frames and every mode played. Worth stating both, because the average is not the
 * number that matters. A control site in the same region read 0.34/frame and then fired
 * once in 1,740 frames, so it is gated and useless; this one never recorded a zero window.
 *
 * NOTHING IS COPIED HERE, and that is the whole design. The obvious stub -- call the
 * game's ROM copy routine at 0x80001A40 the way CV64's stub does -- was written, built,
 * and black-screened a console. A bisect said so precisely: the stub bytes alone booted,
 * the hook with a pass-through stub booted, and the same stub with the copy in it did not.
 * That routine fires exactly once in a 1,200-frame measurement, at boot: it is boot-time
 * code, and starting a raw PI transfer from a frame hook while the game has its own in
 * flight does not survive.
 *
 * So the agent rides the randomizer's boot DMA instead. AGENT_ROM sits inside that DMA's
 * source range, so the agent is in RAM before the first frame, exactly as profiles/oot
 * relies on the game's own payload loader. The profile pins that DMA's instructions,
 * because if its length or target moves the agent is not loaded at all.
 *
 * What the boot DMA cannot do is zero BSS: it copies ROM, so BSS arrives holding whatever
 * the ROM holds past the agent image. The stub has to zero it once per boot, and cannot
 * use the load marker to decide when -- the DMA restores that too, so it reads 'M64P' from
 * the first frame. Every other candidate flag lives in the BSS being zeroed.
 *
 * s_bss_zeroed below is the answer: a word inside the stub's own blob, so it is part of
 * the same DMA and is restored to 0 at every boot, including a reset. Zero means BSS still
 * holds ROM bytes; the stub clears BSS, sets it, and ticks from then on.
 *
 * Symbols from the build (see build.sh): AGENT_VRAM, AGENT_LOAD_SIZE, agent_BSS_START,
 * agent_BSS_END, AGENT_MAGIC_ADDR, agent_tick, and AGENT_MIN_RAM. AGENT_ROM is not used
 * here, because nothing in this stub copies.
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

        /* Run the agent only while its image is intact. The boot DMA put it there; that
           says it arrived, not that it is still there, and nothing reserves that RAM. */
        lui     $t1, 0x4D36
        ori     $t1, $t1, 0x3450        /* 'M64P' */
        lui     $t0, %hi(AGENT_MAGIC_ADDR)
        lw      $t0, %lo(AGENT_MAGIC_ADDR)($t0)
        bne     $t0, $t1, done
        nop

        /* Zeroed already this boot? The flag is in this blob, so a reset clears it. */
        lui     $t0, %hi(s_bss_zeroed)
        lw      $t0, %lo(s_bss_zeroed)($t0)
        bnez    $t0, tick
        nop

        lui     $t0, %hi(agent_BSS_START)
        addiu   $t0, $t0, %lo(agent_BSS_START)
        lui     $t1, %hi(agent_BSS_END)
        addiu   $t1, $t1, %lo(agent_BSS_END)
zero:
        beq     $t0, $t1, zeroed
        nop
        sw      $zero, 0($t0)
        b       zero
        addiu   $t0, $t0, 4
zeroed:
        lui     $t0, %hi(s_bss_zeroed)
        addiu   $t1, $zero, 1
        sw      $t1, %lo(s_bss_zeroed)($t0)

tick:
        CALL    agent_tick
        nop
done:
        lw      $ra, 20($sp)
        jr      $ra
        addiu   $sp, $sp, 24

/* In .text so objcopy keeps it in the blob, and so the boot DMA restores it to 0 every
   time it reloads the region. Not in the agent's BSS, which is the thing it guards. */
        .globl  s_bss_zeroed
s_bss_zeroed:
        .word   0
