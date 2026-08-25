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
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Pick Cargo profile. `release-dev` inherits from release but with
# lto = "thin" + codegen-units = 16 for fast iteration. The resulting .nro
# is slightly larger (~5-15%) but behaves identically modulo LLVM LTO bugs.
PROFILE="release"
CARGO_FLAG="--release"
if [[ "${1:-}" == "--dev" ]]; then
    PROFILE="release-dev"
    CARGO_FLAG="--profile release-dev --features instr"
fi

export PATH="$USERPROFILE/.cargo/bin:$PATH"
# Phase 1.2: Rust nightly GNU's bundled dlltool.exe crashes on raw-dylib build
# scripts (windows-sys). MinGW-w64 from scoop provides a working dlltool.
export PATH="$USERPROFILE/scoop/apps/mingw/current/bin:$PATH"

echo "[1/2] Building Rust no_std staticlib (profile: $PROFILE)..."
(cd "$ROOT/rust" && cargo build $CARGO_FLAG)

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
