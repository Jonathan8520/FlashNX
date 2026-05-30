#Requires -Version 5.1
# Thin wrapper around scripts/build.sh — the single source of truth for the
# build. The real logic must (1) build Rust for the tier-3 Switch target
# `aarch64-nintendo-switch-freestanding` via build-std (NOT the host-ish
# `aarch64-unknown-linux-gnu`, which this script used to pass to cargo and which
# fails in libc with target_env=gnu), and (2) run the C++ link inside
# devkitPro's MSYS2 bash so switch_rules resolves /opt/devkitpro paths. Both
# live in build.sh, so just forward to it through Git Bash (passing --dev etc).
#
# Usage:
#   scripts\build.ps1          # release profile
#   scripts\build.ps1 --dev    # release-dev profile (fast iteration)

$ErrorActionPreference = 'Stop'
$Root = Split-Path $PSScriptRoot -Parent

& bash "$Root/scripts/build.sh" @args
if ($LASTEXITCODE -ne 0) { throw "build.sh failed (exit $LASTEXITCODE)" }
