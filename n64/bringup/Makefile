# Libdragon N64 build -- requires N64_INST. Produces multi64_bringup.z64.
#
# The harness (main.c, stages.c, load.c, wrap.c) is an ordinary libdragon program. The code under
# test is not: each agent variant is the agent's own sources from ../agent, compiled with the
# agent's flags from ../agent/flags.mk (o64 in place of o32, to match libdragon's ABI), linked into
# one object, and every symbol prefixed so four agents fit in one image. See variants.h.

BUILD_DIR = build
include $(N64_INST)/include/n64.mk

all: multi64_bringup.z64
.PHONY: all

AGENT = ../agent
PROTO = ../test-rom
include $(AGENT)/flags.mk

VARIANT_CFLAGS = $(subst -mabi=32,-mabi=o64,$(AGENT_CFLAGS))
VB = $(BUILD_DIR)/variants

PI_IO_FUNCS = pi_io_read pi_io_write pi_io_write_stored pi_io_load_words pi_io_store_words \
              pi_io_load_port pi_io_store_port
# The driver's and PEEKROM's calls into pi_io go to wrap.c instead.
PI_IO_RENAMES = $(foreach f,$(PI_IO_FUNCS),--redefine-sym $(f)=wrap_$(f))

AGENT_HEADERS = $(AGENT)/agent.h $(AGENT)/sc64.h $(AGENT)/ed64.h $(AGENT)/ed64pro.h $(AGENT)/pi_io.h \
                $(PROTO)/mem_proto.h $(PROTO)/m64p_types.h $(AGENT)/flags.mk

# $(1) build directory name, $(2) symbol prefix, $(3) cart driver (sc64, ed64, ed64pro),
# $(4) defines, $(5) the driver functions agent.c calls, which go to wrap.c instead.
define VARIANT
$(VB)/$(1):
	mkdir -p $$@

$(VB)/$(1)/agent.o: $(AGENT)/agent.c $(AGENT_HEADERS) | $(VB)/$(1)
	$(N64_CC) $(VARIANT_CFLAGS) $(4) -I$(AGENT) -I$(PROTO) -c $$< -o $$@
	$(N64_OBJCOPY) $(foreach f,$(5),--redefine-sym $(f)=wrap_$(f)) $$@

$(VB)/$(1)/$(3).o: $(AGENT)/$(3).c $(AGENT_HEADERS) | $(VB)/$(1)
	$(N64_CC) $(VARIANT_CFLAGS) $(4) -I$(AGENT) -I$(PROTO) -c $$< -o $$@
	$(N64_OBJCOPY) $(PI_IO_RENAMES) $$@

$(VB)/$(1)/cart_rom.o: $(AGENT)/cart_rom.c $(AGENT_HEADERS) | $(VB)/$(1)
	$(N64_CC) $(VARIANT_CFLAGS) $(4) -I$(AGENT) -I$(PROTO) -c $$< -o $$@
	$(N64_OBJCOPY) $(PI_IO_RENAMES) $$@

$(VB)/$(1)/pi_io.o: $(AGENT)/pi_io.c $(AGENT_HEADERS) | $(VB)/$(1)
	$(N64_CC) $(VARIANT_CFLAGS) $(4) -I$(AGENT) -I$(PROTO) -c $$< -o $$@

$(VB)/$(1)/mem_proto.o: $(PROTO)/mem_proto.c $(AGENT_HEADERS) | $(VB)/$(1)
	$(N64_CC) $(VARIANT_CFLAGS) $(4) -I$(AGENT) -I$(PROTO) -c $$< -o $$@

# One object, every symbol prefixed. The only names it may leave undefined are its wrap_* calls.
$(VB)/$(1).o: $(VB)/$(1)/agent.o $(VB)/$(1)/$(3).o $(VB)/$(1)/cart_rom.o $(VB)/$(1)/pi_io.o $(VB)/$(1)/mem_proto.o
	$(N64_LD) -r -o $$@.tmp $$^
	$(N64_OBJCOPY) --prefix-symbols=$(2) $$@.tmp $$@
	rm -f $$@.tmp
	@if $(N64_NM) -u $$@ | grep -v ' $(2)wrap_' | grep .; then \
		echo "$$@: undefined symbols other than $(2)wrap_*" >&2; rm -f $$@; exit 1; fi
endef

# $(1) name, $(2) prefix, $(3) defines: pi_io.c on its own, for the harness's probes.
define PI_IO_COPY
$(VB)/$(1).o: $(AGENT)/pi_io.c $(AGENT_HEADERS)
	mkdir -p $(VB)
	$(N64_CC) $(VARIANT_CFLAGS) $(3) -I$(AGENT) -I$(PROTO) -c $$< -o $$@.tmp
	$(N64_OBJCOPY) --prefix-symbols=$(2) $$@.tmp $$@
	rm -f $$@.tmp
endef

$(eval $(call VARIANT,sc64,bsc_,sc64,,sc64_init sc64_poll sc64_read sc64_write))
$(eval $(call VARIANT,x7,bx7_,ed64,-DAGENT_CART_ED64,ed64_init ed64_receive ed64_send))
$(eval $(call VARIANT,x7dma,bx7d_,ed64,-DAGENT_CART_ED64 -DPI_IO_DMA,ed64_init ed64_receive ed64_send))
$(eval $(call VARIANT,pro,bpro_,ed64pro,-DAGENT_CART_ED64PRO,ed64pro_init ed64pro_receive ed64pro_send))
$(eval $(call PI_IO_COPY,pio,pio_,))
$(eval $(call PI_IO_COPY,pdma,pdma_,-DPI_IO_DMA))

VARIANT_OBJS = $(VB)/sc64.o $(VB)/x7.o $(VB)/x7dma.o $(VB)/pro.o $(VB)/pio.o $(VB)/pdma.o

$(BUILD_DIR)/spin_limits.h: spin_limits.sh $(AGENT)/pi_io.c $(AGENT)/ed64.c $(AGENT)/sc64.c $(AGENT)/ed64pro.c
	mkdir -p $(BUILD_DIR)
	sh spin_limits.sh $(AGENT) > $@.tmp
	mv $@.tmp $@

OBJS = $(BUILD_DIR)/main.o $(BUILD_DIR)/stages.o $(BUILD_DIR)/load.o $(BUILD_DIR)/wrap.o

$(BUILD_DIR)/stages.o: $(BUILD_DIR)/spin_limits.h

multi64_bringup.z64: N64_ROM_TITLE = "multi64 bringup"
multi64_bringup.z64: CFLAGS += -I$(BUILD_DIR)

$(BUILD_DIR)/multi64_bringup.elf: $(OBJS) $(VARIANT_OBJS)

clean:
	rm -rf $(BUILD_DIR) *.z64
.PHONY: clean

-include $(wildcard $(BUILD_DIR)/*.d)
