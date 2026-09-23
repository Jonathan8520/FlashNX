#!/usr/bin/env bash
# Orchestrates: Rust no_std staticlib -> C++ devkitPro link -> .nro
#
# Must be run from Git Bash (MinGW64) or an equivalent shell with Windows-style
# paths. The cpp/ build delegates to devkitPro's MSYS2 bash so that switch_rules
# resolves paths consistently.
#
# Usage:
#   scripts/build.sh           # release profile (LTO=full, ~3 min, smaller .nro)
#   scripts/build.sh --dev     # release-dev profile (LTO=thin, ~30 s, dev iterations)
#   scripts/build.sh --prof    # release-dev + frame pointers, for cpp/src/prof.cpp
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Pick Cargo profile. `release-dev` inherits from release but with
# lto = "thin" + codegen-units = 16 for fast iteration. The resulting .nro
# is slightly larger (~5-15%) but behaves identically modulo LLVM LTO bugs.
PROFILE="release"
CARGO_FLAG="--release"
CARGO_CONFIG=()
if [[ "${1:-}" == "--dev" ]]; then
    PROFILE="release-dev"
    CARGO_FLAG="--profile release-dev --features instr"
elif [[ "${1:-}" == "--prof" ]]; then
    # No `instr`: its per-action timers would show up in the profile. The
    # rustflag is appended to the ones in .cargo/config.toml (a --config array
    # merges, an env RUSTFLAGS would replace them all).
    PROFILE="release-prof"
    CARGO_FLAG="--profile release-prof"
    CARGO_CONFIG=(--config 'build.rustflags=["-C", "force-frame-pointers=yes"]')
fi

export PATH="$USERPROFILE/.cargo/bin:$PATH"
# Phase 1.2: Rust nightly GNU's bundled dlltool.exe crashes on raw-dylib build
# scripts (windows-sys). MinGW-w64 from scoop provides a working dlltool.
export PATH="$USERPROFILE/scoop/apps/mingw/current/bin:$PATH"

echo "[1/2] Building Rust no_std staticlib (profile: $PROFILE)..."
(cd "$ROOT/rust" && cargo build $CARGO_FLAG "${CARGO_CONFIG[@]}")

# The .elf depends on the staticlib of the profile it was linked with, so going
# back to a profile whose library is OLDER than the last link would not relink,
# and the .nro would silently stay the other build. Relink on every change.
STAMP="$ROOT/cpp/build/.rust_profile"
if [[ "$(cat "$STAMP" 2>/dev/null)" != "$PROFILE" ]]; then
    rm -f "$ROOT/cpp/FlashNX.elf"
    # prof.cpp and main.cpp are compiled differently for --prof (see
    # cpp/Makefile, FLASHNX_PROF_ALWAYS).
    touch "$ROOT/cpp/src/prof.cpp" "$ROOT/cpp/src/main.cpp"
    mkdir -p "$ROOT/cpp/build"
    echo "$PROFILE" > "$STAMP"
fi

# The tile forwarder, turned into a byte array the C++ build links in.
#
# `make` here produces forwarder.nsp, which is not an installable NSP but the
# ExeFS partition of one: `main` (the loader) and `main.npdm` (its permissions
# and its identity). cpp/src/nsp.cpp wraps that into an NCA at install time.
#
# HERE and not in cpp/Makefile, and the ordering is the whole point: that
# Makefile picks up sources with a wildcard evaluated when it is READ, so a .s
# produced by one of its own targets does not exist yet and never enters the
# link. The binary then comes out silently missing the feature, which is exactly
# what happened.
echo "[1.5/2] Building the tile forwarder..."
/c/devkitPro/msys2/usr/bin/bash.exe -lc "
    export DEVKITPRO=/opt/devkitpro
    export DEVKITA64=/opt/devkitpro/devkitA64
    cd '$ROOT/forwarder' && make
    # Renamed before bin2s because bin2s names the symbol after the file, and
    # nsp.cpp declares flashnx_forwarder_nsp.
    cp forwarder.nsp flashnx_forwarder.nsp
    /opt/devkitpro/tools/bin/bin2s flashnx_forwarder.nsp > '$ROOT/cpp/src/flashnx_forwarder.s'
"

echo "[2/2] Building C++ wrapper and linking .nro via devkitPro MSYS2..."
/c/devkitPro/msys2/usr/bin/bash.exe -lc "
    export DEVKITPRO=/opt/devkitpro
    export DEVKITA64=/opt/devkitpro/devkitA64
    export RUST_PROFILE=$PROFILE
    cd '$ROOT/cpp'
    make
"

echo
echo "Done. Output: cpp/FlashNX.nro"
ls -la "$ROOT/cpp/FlashNX.nro" 2>/dev/null || echo "(.nro not found — build failed)"
