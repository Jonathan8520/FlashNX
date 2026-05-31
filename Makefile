# The supported build entry point is scripts/build.sh. It (1) builds the Rust
# staticlib for the tier-3 `aarch64-nintendo-switch-freestanding` target via
# `-Z build-std` (target + rustflags supplied by rust/.cargo/config.toml), then
# (2) links the .nro *inside devkitPro's MSYS2 bash* so `switch_rules` resolves
# /opt/devkitpro paths correctly.
#
# The old recipe here ran `cargo build --target aarch64-unknown-linux-gnu`,
# which is WRONG: it targets the host-ish GNU triple (target_env=gnu), so libc's
# `timespec` is configured out and the build dies with 40+ errors. These targets
# now just delegate to build.sh so `make` stays a valid, reliable entry point.
.PHONY: all dev clean

all:
	bash scripts/build.sh

dev:
	bash scripts/build.sh --dev

clean:
	cd rust && cargo clean
	rm -rf cpp/build cpp/FlashNX.nro cpp/FlashNX.elf cpp/FlashNX.nacp cpp/FlashNX.map
