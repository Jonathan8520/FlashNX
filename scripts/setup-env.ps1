#Requires -Version 5.1
# Verifies the dev environment has everything needed to build flash-for-switch.

$ErrorActionPreference = 'Stop'

function Test-EnvVar {
    param([string]$Name)
    $val = [Environment]::GetEnvironmentVariable($Name)
    if (-not $val) {
        Write-Host "MISSING: `$env:$Name not set" -ForegroundColor Red
        return $false
    }
    if (-not (Test-Path $val)) {
        Write-Host "INVALID: `$env:$Name = $val (path does not exist)" -ForegroundColor Yellow
        return $false
    }
    Write-Host "OK     : `$env:$Name = $val" -ForegroundColor Green
    return $true
}

function Test-CommandInPath {
    param([string]$Cmd)
    if (Get-Command $Cmd -ErrorAction SilentlyContinue) {
        Write-Host "OK     : $Cmd in PATH" -ForegroundColor Green
        return $true
    }
    Write-Host "MISSING: $Cmd not in PATH" -ForegroundColor Red
    return $false
}

Write-Host "Checking environment for flash-for-switch..." -ForegroundColor Cyan

$ok = $true
$ok = (Test-EnvVar 'DEVKITPRO')      -and $ok
$ok = (Test-EnvVar 'DEVKITA64')      -and $ok
$ok = (Test-EnvVar 'LIBCLANG_PATH')  -and $ok

$ok = (Test-CommandInPath 'rustc')   -and $ok
$ok = (Test-CommandInPath 'cargo')   -and $ok
$ok = (Test-CommandInPath 'cmake')   -and $ok
$ok = (Test-CommandInPath 'python')  -and $ok
$ok = (Test-CommandInPath 'make')    -and $ok

if ($ok) {
    Write-Host "`nAll checks passed." -ForegroundColor Green
    exit 0
} else {
    Write-Host "`nSome checks failed. See README.md for setup." -ForegroundColor Red
    exit 1
}
