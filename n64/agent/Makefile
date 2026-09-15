# Build the game-resident M64P agent as one relocatable object.
#
#   make                          build/m64p_agent.o   (link into your own build)
#   make PREFIX=mips64-ultra-elf- same, with a libultra toolchain
#   make symbols                  exported symbols, and what the object leaves undefined
#   make CART=ed64                build/ed64/m64p_agent.o, for an EverDrive-64 X7 (experimental)
#   make CART=ed64pro             build/ed64pro/m64p_agent.o, for an EverDrive-64 PRO (experimental)
#   make host-test                the EverDrive builds' L3 reassembly, compiled and run on the PC
#   make host-test KNOWN_BUG_151=1  same, plus the case for open bug #151 (fails until it is fixed)
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

# Which cart the agent drives. sc64 is the default and the only driver that has run on hardware;
# its build is unchanged by the other two. ed64 (EverDrive-64 X7) and ed64pro (EverDrive-64 PRO)
# have never run on a cart. The drivers do not detect each other: build for the cart in use.
CART ?= sc64

ifeq ($(CART),sc64)
  BUILD        = build
  CART_OBJECTS = $(BUILD)/sc64.o
  CART_DEFINE  =
else ifeq ($(CART),ed64)
  BUILD        = build/ed64
  CART_OBJECTS = $(BUILD)/ed64.o $(BUILD)/pi_io.o
  CART_DEFINE  = -DAGENT_CART_ED64
else ifeq ($(CART),ed64pro)
  BUILD        = build/ed64pro
  CART_OBJECTS = $(BUILD)/ed64pro.o $(BUILD)/pi_io.o
  CART_DEFINE  = -DAGENT_CART_ED64PRO
else
  $(error CART must be sc64, ed64 or ed64pro)
endif

# o32, no $gp-relative data, no PIC: the object runs inside a game whose $gp, ABI
# and relocation model are its own. Override CFLAGS to match a host build whose
# objects this must link beside.
CFLAGS ?= -O1 -fno-reorder-blocks -march=vr4300 -mtune=vr4300 -mabi=32 -mno-gpopt -G0 \
          -mno-abicalls -fno-pic -mdivide-breaks -mexplicit-relocs -ffreestanding \
          -Wall -Wextra
CPPFLAGS = $(EXTRA_CPPFLAGS) $(CART_DEFINE) -I. -I$(PROTO)

OBJECTS = $(BUILD)/agent.o $(CART_OBJECTS) $(BUILD)/mem_proto.o

all: $(BUILD)/m64p_agent.o
.PHONY: all clean symbols host-test

$(BUILD):
	mkdir -p $(BUILD)

$(BUILD)/agent.o: agent.c agent.h sc64.h ed64.h ed64pro.h $(PROTO)/mem_proto.h $(PROTO)/m64p_types.h | $(BUILD)
	$(CC) $(CFLAGS) $(CPPFLAGS) -c $< -o $@

$(BUILD)/sc64.o: sc64.c sc64.h $(PROTO)/m64p_types.h | $(BUILD)
	$(CC) $(CFLAGS) $(CPPFLAGS) -c $< -o $@

$(BUILD)/ed64.o: ed64.c ed64.h pi_io.h $(PROTO)/m64p_types.h | $(BUILD)
	$(CC) $(CFLAGS) $(CPPFLAGS) -c $< -o $@

$(BUILD)/ed64pro.o: ed64pro.c ed64pro.h pi_io.h $(PROTO)/m64p_types.h | $(BUILD)
	$(CC) $(CFLAGS) $(CPPFLAGS) -c $< -o $@

$(BUILD)/pi_io.o: pi_io.c pi_io.h $(PROTO)/m64p_types.h | $(BUILD)
	$(CC) $(CFLAGS) $(CPPFLAGS) -c $< -o $@

$(BUILD)/mem_proto.o: $(PROTO)/mem_proto.c $(PROTO)/mem_proto.h $(PROTO)/m64p_types.h | $(BUILD)
	$(CC) $(CFLAGS) $(CPPFLAGS) -c $< -o $@

# Partial link into a single object with every file resolved against the others.
$(BUILD)/m64p_agent.o: $(OBJECTS)
	$(LD) -i -o $@ $(OBJECTS)

symbols: $(BUILD)/m64p_agent.o
	@$(SIZE) -A $< | grep -E '^\.(text|rodata|data|sdata|bss|sbss) '
	@echo "--- exported"
	@$(NM) $< | grep -E ' [TDB] '
	@echo "--- undefined (must be empty)"
	@$(NM) -u $<

# The EverDrive builds' L3 reassembly, compiled for the PC and run under ASan and UBSan, once per
# cart, with tests/reassembly_test.c standing in for the driver. Needs a host gcc or clang, not
# the N64 toolchain. CI runs it; it says nothing about either driver on a cart.
HOSTCC ?= cc
HOST_CFLAGS ?= -O1 -g -fsanitize=address,undefined -fno-sanitize-recover=all -Wall -Wextra
HOST_TEST_SOURCES = tests/reassembly_test.c agent.c $(PROTO)/mem_proto.c

# make host-test KNOWN_BUG_151=1 also builds the case for bug #151 (a request missing a lost piece
# is completed by the next request's bytes and handled). It fails until #151 is fixed, so it is off
# by default to keep CI green; the fix for #151 must turn it on by default, in the same change.
KNOWN_BUG_151 ?= 0
HOST_TEST_DEFINES =
ifeq ($(KNOWN_BUG_151),1)
  HOST_TEST_DEFINES += -DTEST_KNOWN_BUG_151
endif

host-test:
	mkdir -p build/host
	$(HOSTCC) $(HOST_CFLAGS) $(HOST_TEST_DEFINES) -DAGENT_CART_ED64 -I. -I$(PROTO) $(HOST_TEST_SOURCES) -o build/host/reassembly-ed64
	build/host/reassembly-ed64
	$(HOSTCC) $(HOST_CFLAGS) $(HOST_TEST_DEFINES) -DAGENT_CART_ED64PRO -I. -I$(PROTO) $(HOST_TEST_SOURCES) -o build/host/reassembly-ed64pro
	build/host/reassembly-ed64pro

clean:
	rm -rf build
