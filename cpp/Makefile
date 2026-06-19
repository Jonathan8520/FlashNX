#---------------------------------------------------------------------------------
# devkitPro Switch Makefile for flash-for-switch.
# Links against the Rust staticlib built in ../rust/.
#---------------------------------------------------------------------------------
.SUFFIXES:

ifeq ($(strip $(DEVKITPRO)),)
$(error "Please set DEVKITPRO in your environment. export DEVKITPRO=<path to>/devkitpro")
endif

TOPDIR ?= $(CURDIR)
include $(DEVKITPRO)/libnx/switch_rules

#---------------------------------------------------------------------------------
TARGET      := FlashNX
BUILD       := build
SOURCES     := src
INCLUDES    := include

# User-facing name = "FlashNX" (rebrand 2026-05-26, fully applied at v1.0.0).
# The repo + Cargo lib name stay `flash-for-switch` (dev-facing, stable); every
# user-facing artifact is FlashNX — the `.nro`/.nacp name (TARGET), the .nacp
# title/author below, and the SD data dir `switch/FlashNX/`.
APP_TITLE   := FlashNX
APP_AUTHOR  := Jonathan8520
APP_VERSION := 1.3.1
# Icon shown in hbmenu / Sphaira / forwarders. Lives in the repo root's
# `assets/` folder so it sits next to README + the banner. devkitPro's
# %.nro rule reads $(NROFLAGS) which we wire below — without --icon= the
# .nro gets no embedded icon at all (NOT the default_icon.jpg fallback;
# that fallback only kicks in when APP_ICON is consumed by elf2nro
# explicitly via NROFLAGS).
ICON        := ../assets/icon.jpg

#---------------------------------------------------------------------------------
ARCH     := -march=armv8-a+crc+crypto -mtune=cortex-a57 -mtp=soft -fPIE

CFLAGS   := -g -Wall -O2 -ffunction-sections $(ARCH) $(DEFINES) -D__SWITCH__
CFLAGS   += $(INCLUDE)

CXXFLAGS := $(CFLAGS) -fno-rtti -fno-exceptions -std=gnu++17

ASFLAGS  := -g $(ARCH)
LDFLAGS  := -specs=$(DEVKITPRO)/libnx/switch.specs -g $(ARCH) -Wl,-Map,$(notdir $*.map)

#---------------------------------------------------------------------------------
# Rust staticlib + switch-mesa GL stack + libnx
#---------------------------------------------------------------------------------
# RUST_PROFILE picks which Cargo profile dir to link against. Default
# `release` matches `cargo build --release`. Override to `release-dev` for
# the LTO=thin fast-iteration dev profile (see scripts/build.sh --dev).
RUST_PROFILE ?= release
RUST_LIB_DIR := $(TOPDIR)/../rust/target/aarch64-nintendo-switch-freestanding/$(RUST_PROFILE)
# Concrete static lib path — used as a make dependency so the .elf relinks
# automatically whenever cargo regenerates it. Without this, switching a
# Rust file would not trigger the C++ link step and the .nro would stay
# stale on disk while the .a is fresh.
RUST_STATICLIB := $(RUST_LIB_DIR)/libruffle_switch.a

# Phase 3.7 — libcurl + mbedtls (HTTPS via switch-curl/switch-mbedtls portlibs).
# Order matters: curl after mbedtls is wrong; curl uses mbedtls so it must
# come first on the link line for static libs. zlib needed for curl's
# CURLOPT_ACCEPT_ENCODING and the SWF CWS inflate (already pulled by Rust
# but switch-zlib provides the .a). nx must stay last so libnx exports
# resolve. usbhsfs and friends aren't needed.
LIBS    := -lruffle_switch -lEGL -lglapi -ldrm_nouveau -lGLESv2 \
           -lcurl -lmbedtls -lmbedx509 -lmbedcrypto -lz \
           -lnx
LIBDIRS := $(PORTLIBS) $(LIBNX)

#---------------------------------------------------------------------------------
ifneq ($(BUILD),$(notdir $(CURDIR)))
#---------------------------------------------------------------------------------
export OUTPUT  := $(CURDIR)/$(TARGET)
export TOPDIR  := $(CURDIR)
export VPATH   := $(foreach dir,$(SOURCES),$(CURDIR)/$(dir))
export DEPSDIR := $(CURDIR)/$(BUILD)

CFILES   := $(foreach dir,$(SOURCES),$(notdir $(wildcard $(dir)/*.c)))
CPPFILES := $(foreach dir,$(SOURCES),$(notdir $(wildcard $(dir)/*.cpp)))

ifeq ($(strip $(CPPFILES)),)
    export LD := $(CC)
else
    export LD := $(CXX)
endif

export OFILES_SRC := $(CPPFILES:.cpp=.o) $(CFILES:.c=.o)
export OFILES     := $(OFILES_SRC)

export INCLUDE    := $(foreach dir,$(INCLUDES),-I$(CURDIR)/$(dir)) \
                     $(foreach dir,$(LIBDIRS),-I$(dir)/include) \
                     -I$(CURDIR)/$(BUILD)

export LIBPATHS   := $(foreach dir,$(LIBDIRS),-L$(dir)/lib) \
                     -L$(RUST_LIB_DIR)

# Wire the NACP + icon into the final .nro via elf2nro. The %.nro rule in
# devkitPro's switch_rules invokes `elf2nro $< $@ $(NROFLAGS)` — without
# these flags the .nro ships with no metadata + no icon, so hbmenu shows
# it as "Unknown" with the default question-mark tile. APP_ICON is the
# absolute path elf2nro needs (relative paths break the recursive make).
export APP_ICON  := $(TOPDIR)/$(ICON)
export NROFLAGS  += --nacp=$(CURDIR)/$(TARGET).nacp --icon=$(APP_ICON)

.PHONY: $(BUILD) clean all

all: $(BUILD)

$(BUILD):
	@[ -d $@ ] || mkdir -p $@
	@$(MAKE) --no-print-directory -C $(BUILD) -f $(CURDIR)/Makefile

clean:
	@echo "[cpp] clean"
	@rm -fr $(BUILD) $(TARGET).nro $(TARGET).nacp $(TARGET).elf

#---------------------------------------------------------------------------------
else
.PHONY: all

DEPENDS := $(OFILES:.o=.d)

all: $(OUTPUT).nro

$(OUTPUT).nro: $(OUTPUT).elf $(OUTPUT).nacp
# Force .elf relink whenever the Rust staticlib changes. Without this, a
# `cargo build` followed by `make` shows "Rien à faire" and the .nro
# stays at the previous Rust binary.
$(OUTPUT).elf: $(OFILES) $(RUST_STATICLIB)

-include $(DEPENDS)

endif
