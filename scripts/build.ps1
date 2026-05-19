#Requires -Version 5.1
# Orchestrates: Rust staticlib -> C++ devkitPro link -> .nro

$ErrorActionPreference = 'Stop'

$Root = Split-Path $PSScriptRoot -Parent

Write-Host "[1/2] Building Rust staticlib..." -ForegroundColor Cyan
Push-Location "$Root\rust"
try {
    cargo build --release --target aarch64-unknown-linux-gnu
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally {
    Pop-Location
}

Write-Host "[2/2] Building C++ wrapper and linking .nro..." -ForegroundColor Cyan
Push-Location "$Root\cpp"
try {
    make
    if ($LASTEXITCODE -ne 0) { throw "make failed" }
} finally {
    Pop-Location
}

Write-Host "`nDone. Output: cpp\flash-for-switch.nro" -ForegroundColor Green
