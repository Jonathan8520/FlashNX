#!/usr/bin/env bash
# Orchestrates: Rust no_std staticlib -> C++ devkitPro link -> .nro
#
# Must be run from Git Bash (MinGW64) or an equivalent shell with Windows-style
# paths. The cpp/ build delegates to devkitPro's MSYS2 bash so that switch_rules
# resolves paths consistently.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

export PATH="$USERPROFILE/.cargo/bin:$PATH"

echo "[1/2] Building Rust no_std staticlib for aarch64-nintendo-switch-freestanding..."
(cd "$ROOT/rust" && cargo build --release)

echo "[2/2] Building C++ wrapper and linking .nro via devkitPro MSYS2..."
/c/devkitPro/msys2/usr/bin/bash.exe -lc "
    export DEVKITPRO=/opt/devkitpro
    export DEVKITA64=/opt/devkitpro/devkitA64
    cd '$ROOT/cpp'
    make
"

echo
echo "Done. Output: cpp/flash-for-switch.nro"
ls -la "$ROOT/cpp/flash-for-switch.nro" 2>/dev/null || echo "(.nro not found — build failed)"
