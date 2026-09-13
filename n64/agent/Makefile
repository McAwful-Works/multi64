# Build the game-resident M64P agent as one relocatable object.
#
#   make                          build/m64p_agent.o   (link into your own build)
#   make PREFIX=mips64-ultra-elf- same, with a libultra toolchain
#   make symbols                  exported symbols, and what the object leaves undefined
#
# For a game with no buildable source, link a flat image at a fixed address instead:
# templates/link-flat.sh. See docs/integration/cart-agent.md.
#
# The protocol handler and the types header come from ../test-rom, so the agent and
# the test ROM always build the same M64P code. A host whose toolchain has no
# <stdint.h>/<stddef.h> replaces m64p_types.h: put its own copy first on the include
# path with EXTRA_CPPFLAGS=-I<dir>.

# libdragon's toolchain, pinned in ../toolchain.lock ($N64_INST/bin on PATH).
PREFIX ?= mips64-elf-
CC      = $(PREFIX)gcc
LD      = $(PREFIX)ld
NM      = $(PREFIX)nm
SIZE    = $(PREFIX)size

PROTO   = ../test-rom
BUILD   = build

# o32, no $gp-relative data, no PIC: the object runs inside a game whose $gp, ABI
# and relocation model are its own. Override CFLAGS to match a host build whose
# objects this must link beside.
CFLAGS ?= -O1 -fno-reorder-blocks -march=vr4300 -mtune=vr4300 -mabi=32 -mno-gpopt -G0 \
          -mno-abicalls -fno-pic -mdivide-breaks -mexplicit-relocs -ffreestanding \
          -Wall -Wextra
CPPFLAGS = $(EXTRA_CPPFLAGS) -I. -I$(PROTO)

OBJECTS = $(BUILD)/agent.o $(BUILD)/sc64.o $(BUILD)/mem_proto.o

all: $(BUILD)/m64p_agent.o
.PHONY: all clean symbols

$(BUILD):
	mkdir -p $(BUILD)

$(BUILD)/agent.o: agent.c agent.h sc64.h $(PROTO)/mem_proto.h $(PROTO)/m64p_types.h | $(BUILD)
	$(CC) $(CFLAGS) $(CPPFLAGS) -c $< -o $@

$(BUILD)/sc64.o: sc64.c sc64.h $(PROTO)/m64p_types.h | $(BUILD)
	$(CC) $(CFLAGS) $(CPPFLAGS) -c $< -o $@

$(BUILD)/mem_proto.o: $(PROTO)/mem_proto.c $(PROTO)/mem_proto.h $(PROTO)/m64p_types.h | $(BUILD)
	$(CC) $(CFLAGS) $(CPPFLAGS) -c $< -o $@

# Partial link into a single object with all three files resolved against each other.
$(BUILD)/m64p_agent.o: $(OBJECTS)
	$(LD) -i -o $@ $(OBJECTS)

symbols: $(BUILD)/m64p_agent.o
	@$(SIZE) -A $< | grep -E '^\.(text|rodata|data|sdata|bss|sbss) '
	@echo "--- exported"
	@$(NM) $< | grep -E ' [TDB] '
	@echo "--- undefined (must be empty)"
	@$(NM) -u $<

clean:
	rm -rf $(BUILD)
