# Build the game-resident M64P agent as one relocatable object.
#
#   make                          build/m64p_agent.o   (link into your own build)
#   make PREFIX=mips64-ultra-elf- same, with a libultra toolchain
#   make symbols                  exported symbols, and what the object leaves undefined
#   make CART=ed64                build/ed64/m64p_agent.o, for an EverDrive-64 X7 (experimental)
#   make CART=ed64pro             build/ed64pro/m64p_agent.o, for an EverDrive-64 PRO (experimental)
#   make CART=ed64 PI_IO=dma      build/ed64-dma/m64p_agent.o: pi_io.c moves words by PI DMA, a
#                                 diagnostic build that breaks the agent's PI rules (see pi_io.c)
#   make host-test                the EverDrive builds' L3 reassembly, the SC64 driver's failure
#                                 handling, the X7 driver's receive path, PEEKROM, WATCH and
#                                 repeated writes, compiled and run on the PC
#
# For a game with no buildable source, link a flat image at a fixed address instead:
# templates/link-flat.sh. See docs/integration/cart-agent.md.
#
# The protocol handler and the types header come from ../test-rom, so the agent and
# the test ROM always build the same M64P code, but for one switch the agent turns on
# (POKE_ONCE below). A host whose toolchain has no
# <stdint.h>/<stddef.h> replaces m64p_types.h: put its own copy first on the include
# path with EXTRA_CPPFLAGS=-I<dir>.

# libdragon's toolchain, pinned in ../toolchain.lock ($N64_INST/bin on PATH).
PREFIX ?= mips64-elf-
CC      = $(PREFIX)gcc
LD      = $(PREFIX)ld
NM      = $(PREFIX)nm
SIZE    = $(PREFIX)size

PROTO   = ../test-rom

# Which cart the agent drives. sc64 is the default and the only driver a game has been played
# through; its build is unchanged by the other two. ed64 (EverDrive-64 X7) has worked on one cart
# in n64/bringup's ROM and has answered inside one game, Ocarina of Time, as it stood before it
# kept its place in the stream between calls and timed its waits (ed64.h); no cart has run it
# since. ed64pro (EverDrive-64 PRO) has never run on a cart. The drivers do not detect each other:
# build for the cart in use.
CART ?= sc64

ifeq ($(CART),sc64)
  BUILD        = build
  CART_OBJECTS = $(BUILD)/sc64.o
  CART_DEFINE  =
else ifeq ($(CART),ed64)
  BUILD        = build/ed64
  CART_OBJECTS = $(BUILD)/ed64.o
  CART_DEFINE  = -DAGENT_CART_ED64
else ifeq ($(CART),ed64pro)
  BUILD        = build/ed64pro
  CART_OBJECTS = $(BUILD)/ed64pro.o
  CART_DEFINE  = -DAGENT_CART_ED64PRO
else
  $(error CART must be sc64, ed64 or ed64pro)
endif

# Every agent build recognizes a POKEV the host sent again, acknowledging it without writing it a
# second time, and says so in HELLO_ACK (memory-l3-application-v0.md 4.4). mem_proto.c leaves that
# out unless asked: the test ROM and n64/bringup's ROM build the same file without it, and keep the
# images their hardware records are for. Builds that compile these sources some other way pass the
# same define (templates/link-flat.sh, crates/ap64-core/agent/build.sh).
POKE_ONCE = -DM64P_POKE_ONCE

# How pi_io.c moves a run of words: io (the default) by CPU load and store, dma by PI DMA through a
# bounce buffer. dma programs the PI's DMA registers, which a game-resident agent must not do; it
# exists so n64/bringup can show whether an EverDrive needs it. It changes pi_io_load_words and
# pi_io_store_words only: the X7 driver's USB window and every build's PEEKROM, not the SC64
# driver, which has its own copy loop.
PI_IO ?= io

ifeq ($(PI_IO),dma)
  BUILD       := build/$(CART)-dma
  CART_DEFINE += -DPI_IO_DMA
else ifneq ($(PI_IO),io)
  $(error PI_IO must be io or dma)
endif

# The flags live in flags.mk, which n64/bringup shares. Override CFLAGS to match a host build whose
# objects this must link beside.
include $(dir $(lastword $(MAKEFILE_LIST)))flags.mk
CFLAGS ?= $(AGENT_CFLAGS)
CPPFLAGS = $(EXTRA_CPPFLAGS) $(CART_DEFINE) $(POKE_ONCE) -I. -I$(PROTO)

# cart_rom.o (PEEKROM) is the same for every cart, and reads through pi_io.o.
OBJECTS = $(BUILD)/agent.o $(CART_OBJECTS) $(BUILD)/cart_rom.o $(BUILD)/pi_io.o $(BUILD)/mem_proto.o

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

$(BUILD)/cart_rom.o: cart_rom.c pi_io.h $(PROTO)/mem_proto.h $(PROTO)/m64p_types.h | $(BUILD)
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
# cart, with tests/reassembly_test.c standing in for the driver, and with M64P_POKE_ONCE as every
# agent build has it. Needs a host gcc or clang, not the N64 toolchain. CI runs it; it says nothing
# about any driver on a cart.
HOSTCC ?= cc
HOST_CFLAGS ?= -O1 -g -fsanitize=address,undefined -fno-sanitize-recover=all -Wall -Wextra
HOST_TEST_SOURCES = tests/reassembly_test.c agent.c $(PROTO)/mem_proto.c

# tests/sc64_test.c: the real sc64.c with SC64_HOST_TEST routing its PI loads, stores and interrupt
# mask through a fake cart that can hold the bus busy. It checks that every wait is bounded and every
# failure is reported, not that the register model matches an SC64.
#
# tests/cart_rom_test.c: the real mem_proto.c and cart_rom.c with a fake pi_io_load_words serving a
# known ROM image. It checks PEEKROM's protocol handling and cart_rom.c's byte extraction at every
# alignment, not that a cart answers.
#
# tests/watch_test.c: the real mem_proto.c over a buffer standing in for RDRAM. It checks that a
# value written and overwritten between two of the host's reads still reaches it, in order, because
# the agent looked every frame -- and the rules around that (spec 4.3). No console, no cart.
#
# tests/poke_once_test.c: the real mem_proto.c over a buffer standing in for RDRAM, built once with
# M64P_POKE_ONCE as every agent is and once without it as the test ROM is. With it, it checks that a
# write the host sent again under the same rid is acknowledged and not applied a second time, that a
# new rid, or the same one after any other request, is a new write, and that HELLO says so; without
# it, that HELLO says nothing and a copy is applied again (spec 4.4). No console, no cart.
#
# tests/ed64_test.c: the real ed64.c linked against fake pi_io_* functions in place of pi_io.c,
# modelling the X7's USBCFG register and USB window, and built with ED64_HOST_TEST so that the test
# supplies the driver's clock. It checks that a received message is delivered in order and whole,
# or reported lost once with the messages behind it intact, that a busy or slow PI loses none, and
# that every call ends within its limit on that clock, not that the model matches an X7. On a cart,
# the X7 driver has worked on one X7, in n64/bringup's ROM and inside one game, Ocarina of Time, as
# it stood before it kept its place in the stream between calls and timed its waits. No cart has
# run it since.
host-test:
	mkdir -p build/host
	$(HOSTCC) $(HOST_CFLAGS) $(POKE_ONCE) -DAGENT_CART_ED64 -I. -I$(PROTO) $(HOST_TEST_SOURCES) -o build/host/reassembly-ed64
	build/host/reassembly-ed64
	$(HOSTCC) $(HOST_CFLAGS) $(POKE_ONCE) -DAGENT_CART_ED64PRO -I. -I$(PROTO) $(HOST_TEST_SOURCES) -o build/host/reassembly-ed64pro
	build/host/reassembly-ed64pro
	$(HOSTCC) $(HOST_CFLAGS) -DSC64_HOST_TEST -I. -I$(PROTO) tests/sc64_test.c sc64.c -o build/host/sc64
	build/host/sc64
	$(HOSTCC) $(HOST_CFLAGS) -DED64_HOST_TEST -I. -I$(PROTO) tests/ed64_test.c ed64.c -o build/host/ed64
	build/host/ed64
	$(HOSTCC) $(HOST_CFLAGS) -I. -I$(PROTO) tests/cart_rom_test.c cart_rom.c $(PROTO)/mem_proto.c -o build/host/cart_rom
	build/host/cart_rom
	$(HOSTCC) $(HOST_CFLAGS) -I. -I$(PROTO) tests/watch_test.c $(PROTO)/mem_proto.c -o build/host/watch
	build/host/watch
	$(HOSTCC) $(HOST_CFLAGS) $(POKE_ONCE) -I. -I$(PROTO) tests/poke_once_test.c $(PROTO)/mem_proto.c -o build/host/poke_once
	build/host/poke_once
	$(HOSTCC) $(HOST_CFLAGS) -I. -I$(PROTO) tests/poke_once_test.c $(PROTO)/mem_proto.c -o build/host/poke_once-off
	build/host/poke_once-off

clean:
	rm -rf build
