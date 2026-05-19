#!/usr/bin/env bash
# Verifies the dev environment has everything needed to build flash-for-switch.
set -u

ok=0
fail=0

check_env() {
    local name="$1"
    local val="${!name:-}"
    if [[ -z "$val" ]]; then
        echo "MISSING: \$$name not set"
        fail=$((fail+1))
        return
    fi
    if [[ ! -e "$val" ]]; then
        echo "INVALID: \$$name = $val (path does not exist)"
        fail=$((fail+1))
        return
    fi
    echo "OK     : \$$name = $val"
    ok=$((ok+1))
}

check_cmd() {
    local cmd="$1"
    if command -v "$cmd" >/dev/null 2>&1; then
        echo "OK     : $cmd in PATH"
        ok=$((ok+1))
    else
        echo "MISSING: $cmd not in PATH"
        fail=$((fail+1))
    fi
}

echo "Checking environment for flash-for-switch..."

check_env DEVKITPRO
check_env DEVKITA64
check_env LIBCLANG_PATH

check_cmd rustc
check_cmd cargo
check_cmd cmake
check_cmd python
check_cmd make

echo
if [[ $fail -eq 0 ]]; then
    echo "All checks passed ($ok OK)."
    exit 0
else
    echo "Failed checks: $fail (passed: $ok). See README.md for setup."
    exit 1
fi
