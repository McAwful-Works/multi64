/*
 * Link the M64P agent as one flat image at a fixed RAM address, for a game with no
 * buildable decomp. AGENT_VRAM is passed with --defsym. The loadable image is
 * .text + .rodata + .data (copied from ROM by the stub); .bss is zeroed by the stub.
 */
OUTPUT_ARCH(mips)
ENTRY(agent_tick)

SECTIONS
{
    . = AGENT_VRAM;
    agent_VRAM = .;

    .text : { *(.text .text.*) }
    .rodata : { *(.rodata .rodata.* .rodata1) }
    agent_RO_END = .;       /* what the stub's image check covers: .text and .rodata */
    .data : {
        KEEP(*host.o(.data))
        *(.data .data.* .sdata .sdata.*)
        . = ALIGN(4);
    }
    agent_LOAD_END = .;

    .bss (NOLOAD) : {
        agent_BSS_START = .;
        *(.sbss .sbss.* .bss .bss.* COMMON)
        . = ALIGN(4);
        agent_BSS_END = .;
    }

    /DISCARD/ : {
        *(.MIPS.abiflags) *(.reginfo) *(.pdr) *(.comment) *(.gnu.attributes)
        *(.note .note.*) *(.mdebug.*) *(.eh_frame)
    }
}
