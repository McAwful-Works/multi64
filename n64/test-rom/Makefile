# Libdragon N64 build — requires N64_INST. Produces multi64_test.z64

BUILD_DIR = build

# Default EEPROM save; use `make N64_ROM_SAVETYPE=sram256k` (or sram768k / sram1m) for SRAM M64T tests.
# libdragon `n64.mk` assigns `N64_ROM_SAVETYPE` empty — snapshot before include so our default / CLI value survives.
N64_ROM_SAVETYPE ?= eeprom4k
SAVED_SAVETYPE := $(N64_ROM_SAVETYPE)

include $(N64_INST)/include/n64.mk

N64_ROM_SAVETYPE := $(SAVED_SAVETYPE)

ifeq ($(N64_ROM_SAVETYPE),sram256k)
  SRAM_TEST_FLAGS := -DTEST_SRAM_BYTES=32768
else ifeq ($(N64_ROM_SAVETYPE),sram768k)
  SRAM_TEST_FLAGS := -DTEST_SRAM_BYTES=98304
else ifeq ($(N64_ROM_SAVETYPE),sram1m)
  SRAM_TEST_FLAGS := -DTEST_SRAM_BYTES=131072
else
  SRAM_TEST_FLAGS := -DTEST_SRAM_BYTES=0
endif

OBJS = $(BUILD_DIR)/main.o $(BUILD_DIR)/test_proto.o $(BUILD_DIR)/mem_proto.o $(BUILD_DIR)/save_hw.o

all: multi64_test.z64
.PHONY: all

multi64_test.z64: N64_ROM_TITLE = "multi64 test"
multi64_test.z64: CFLAGS += $(SRAM_TEST_FLAGS)

$(BUILD_DIR)/multi64_test.elf: $(OBJS)

clean:
	rm -rf $(BUILD_DIR) *.z64
.PHONY: clean

# Verify the installed toolchain against ../toolchain.lock. Opt-in: CI does not
# build this ROM, so drift is only ever caught by a human running it.
check-toolchain:
	@sh ../check-toolchain.sh "$(N64_INST)"
.PHONY: check-toolchain

-include $(wildcard $(BUILD_DIR)/*.d)
