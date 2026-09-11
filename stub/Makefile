#---------------------------------------------------------------------------------
# The FlashNX shortcut stub (see source/main.c).
#
# Built ONCE, at FlashNX build time, and embedded in the player as a byte array.
# FlashNX then writes a patched copy per game into `sdmc:/switch/`. Nothing here
# is ever compiled on the console.
#
# Deliberately NOT a devkitPro `switch_rules` app: no icon, no NACP, no romfs.
# The copies get their own NACP written into them at runtime, and an asset
# section here would only be dead weight in every copy.
#---------------------------------------------------------------------------------
.SUFFIXES:

ifeq ($(strip $(DEVKITPRO)),)
$(error "Please set DEVKITPRO in your environment. export DEVKITPRO=<path to>/devkitpro")
endif

TOPDIR ?= $(CURDIR)
include $(DEVKITPRO)/libnx/switch_rules

TARGET   := flashnx_stub
BUILD    := build
SOURCES  := source

ARCH     := -march=armv8-a+crc+crypto -mtune=cortex-a57 -mtp=soft -fPIE
# -Os and section GC: every byte here is paid again in FlashNX's own binary AND
# in each shortcut on the card.
CFLAGS   := -g -Wall -Os -ffunction-sections -fdata-sections $(ARCH) -D__SWITCH__ \
            -I$(LIBNX)/include
LDFLAGS  := -specs=$(DEVKITPRO)/libnx/switch.specs -g $(ARCH) -Wl,--gc-sections
LIBS     := -lnx
LIBDIRS  := $(LIBNX)

CFILES   := $(notdir $(wildcard $(SOURCES)/*.c))
OFILES   := $(CFILES:.c=.o)

export LIBPATHS := $(foreach dir,$(LIBDIRS),-L$(dir)/lib)
export INCLUDE  := $(foreach dir,$(LIBDIRS),-I$(dir)/include)

.PHONY: all clean

all: $(TARGET).nro

$(TARGET).nro: $(TARGET).elf
	@elf2nro $< $@
	@echo "stub: $@ ($$(stat -c%s $@ 2>/dev/null || echo ?) bytes)"

$(TARGET).elf: $(BUILD)/$(OFILES)
	@$(CC) $(LDFLAGS) $< $(LIBPATHS) $(LIBS) -o $@

$(BUILD)/%.o: $(SOURCES)/%.c
	@[ -d $(BUILD) ] || mkdir -p $(BUILD)
	@$(CC) -MMD -MP $(CFLAGS) -c $< -o $@

clean:
	@rm -fr $(BUILD) $(TARGET).elf $(TARGET).nro

-include $(BUILD)/*.d
