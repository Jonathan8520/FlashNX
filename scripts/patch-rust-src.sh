#!/usr/bin/env bash
#
# Re-apply the two rust-src patches FlashNX's Switch build needs.
#
# These patches live INSIDE rustup's std source
# (~/.rustup/toolchains/<tc>/lib/rustlib/src/rust/library/std/), NOT in this
# repo, so every `rustup update` / toolchain reinstall replaces them with the
# pristine upstream files and they are lost. Without them the build fails:
#   - Patch 1 (build.rs)   : missing -> `error[E0658] restricted_std` on
#                            memchr/arrayvec/simd-adler32 (the Switch target is
#                            os=horizon/vendor=nintendo/env="", not in std's
#                            "supported" allowlist, so std builds restricted).
#   - Patch 2 (random.rs)  : missing -> HashMap crashes on the console (the lazy
#                            thread_local RNG crashes on Horizon).
#
# Run this after ANY toolchain change, then rebuild. Idempotent — safe to re-run.
# See DEVELOPMENT.md "rust-src patches" for the full rationale.
#
set -uo pipefail

TC="nightly-x86_64-pc-windows-gnu"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# 1. Make sure the toolchain + rust-src are installed (a bare `nightly` grabs the
#    MSVC host, which needs VS Build Tools — pin the GNU host explicitly).
if ! rustup toolchain list 2>/dev/null | grep -q "^$TC"; then
    echo "==> Installing $TC (+ rust-src)..."
    rustup toolchain install "$TC" --component rust-src || exit 1
fi
rustup component add rust-src --toolchain "$TC" >/dev/null 2>&1 || true

STD="$(rustc "+$TC" --print sysroot)/lib/rustlib/src/rust/library/std"
BUILD_RS="$STD/build.rs"
RANDOM_RS="$STD/src/hash/random.rs"
[ -f "$BUILD_RS" ]  || { echo "ERROR: $BUILD_RS not found (rust-src missing?)"  >&2; exit 1; }
[ -f "$RANDOM_RS" ] || { echo "ERROR: $RANDOM_RS not found" >&2; exit 1; }

# 2. Apply both patches (idempotent, anchored on stable strings).
out="$(python3 - "$BUILD_RS" "$RANDOM_RS" <<'PY'
import sys
build_rs, random_rs = sys.argv[1], sys.argv[2]

# --- Patch 1: allow std for the Switch (Horizon) target -> no restricted_std ---
s = open(build_rs, encoding="utf-8").read()
if 'target_os == "horizon"' in s:
    print("Patch 1 (build.rs): already applied")
else:
    anchor = '        || (target_vendor == "nintendo" && target_env == "newlib")\n'
    if anchor not in s:
        sys.exit("Patch 1: anchor line not found in build.rs (upstream layout changed?)")
    s = s.replace(
        anchor,
        anchor + '        || (target_vendor == "nintendo" && target_os == "horizon")\n',
        1,
    )
    open(build_rs, "w", encoding="utf-8").write(s)
    print("Patch 1 (build.rs): APPLIED")

# --- Patch 2: deterministic RandomState on Horizon (thread_local crashes) ------
s = open(random_rs, encoding="utf-8").read()
# Detect ANY prior horizon patch (this script's, or a hand-applied one) so we
# never double-inject: pristine upstream random.rs has no horizon cfg.
if 'target_os = "horizon"' in s:
    print("Patch 2 (random.rs): already applied")
else:
    anchor = "    pub fn new() -> RandomState {\n"
    if anchor not in s:
        sys.exit("Patch 2: anchor 'pub fn new() -> RandomState {' not found")
    inject = (
        "    // FlashNX-horizon-randomstate: the thread_local RNG below crashes on\n"
        "    // Switch (Horizon); use a deterministic process-static counter there.\n"
        "    #[allow(unreachable_code)]\n"
        "    pub fn new() -> RandomState {\n"
        "        #[cfg(target_os = \"horizon\")]\n"
        "        {\n"
        "            use crate::sync::atomic::{AtomicU64, Ordering};\n"
        "            static K0: AtomicU64 = AtomicU64::new(0x9E3779B97F4A7C15);\n"
        "            let k0 = K0.fetch_add(1, Ordering::Relaxed);\n"
        "            return RandomState { k0, k1: 0x517CC1B727220A95 };\n"
        "        }\n"
    )
    open(random_rs, "w", encoding="utf-8").write(s.replace(anchor, inject, 1))
    print("Patch 2 (random.rs): APPLIED")
PY
)"
status=$?
echo "$out"
[ $status -ne 0 ] && { echo "ERROR: patching failed." >&2; exit 1; }

# 3. If anything changed, std must be recompiled: `-Z build-std` does NOT
#    re-fingerprint on a rust-src edit, so a stale (restricted) std stays cached.
if grep -q "APPLIED" <<<"$out"; then
    echo "==> Patches applied. Running 'cargo clean' so build-std rebuilds std..."
    (cd "$ROOT/rust" && cargo clean)
    echo "==> Done. Now: bash scripts/build.sh --dev"
else
    echo "==> Nothing to do (both patches already present)."
fi
