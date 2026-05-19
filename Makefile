.PHONY: all rust cpp clean

all: cpp

rust:
	cd rust && cargo build --release --target aarch64-unknown-linux-gnu

cpp: rust
	$(MAKE) -C cpp

clean:
	cd rust && cargo clean
	$(MAKE) -C cpp clean
