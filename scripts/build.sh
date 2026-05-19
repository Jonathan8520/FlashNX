#!/usr/bin/env bash
# Orchestrates: Rust staticlib -> C++ devkitPro link -> .nro
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "[1/2] Building Rust staticlib..."
(cd "$ROOT/rust" && cargo build --release --target aarch64-unknown-linux-gnu)

echo "[2/2] Building C++ wrapper and linking .nro..."
(cd "$ROOT/cpp" && make)

echo
echo "Done. Output: cpp/flash-for-switch.nro"
