# The compiler flags every agent object is built with, in one place so that n64/bringup builds the
# drivers the same way. o32, no $gp-relative data, no PIC: the object runs inside a game whose $gp,
# ABI and relocation model are its own.
#
# n64/bringup links these sources into a libdragon program, which is o64; it swaps -mabi=32 for
# -mabi=o64 and keeps the rest, because o32 code saves only the low half of the registers an o64
# caller expects preserved.
AGENT_CFLAGS = -O1 -fno-reorder-blocks -march=vr4300 -mtune=vr4300 -mabi=32 -mno-gpopt -G0 \
               -mno-abicalls -fno-pic -mdivide-breaks -mexplicit-relocs -ffreestanding \
               -Wall -Wextra
