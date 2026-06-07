# FlashNX — technical guide

**FlashNX** — homebrew Flash player for Nintendo Switch (`.nro`). Runs any AS1/AS2 `.swf` (and part of AS3) straight from the SD card.

**Powered by [Ruffle](https://github.com/ruffle-rs/ruffle)** (Apache-2.0 / MIT) — FlashNX embeds the `ruffle_core` core (SWF parsing + AVM1/AVM2 interpreter) and wires a native Switch stack onto it: custom OpenGL backend (switch-mesa), audren audio backend, SD storage, joycon-navigable library UI, in-game key editor, anti-fragmentation GL mega-arena, native libnx exception handler, patched fork of jpeg-decoder for newlib. The AVM1/AS2/AVM2 compatibility gaps come from the upstream Ruffle engine — see [ruffle.rs/compatibility](https://ruffle.rs/compatibility) (~99% of the AVM1 language, ~75-81% of the APIs).

> **About the name**: the repo, the toolchain, the build scripts and the Cargo crate keep the historical name `flash-for-switch` (to avoid a destructive switch-over). **FlashNX** is the user-facing name — the UI, the `.nacp`, the `.nro` and the SD folder (`sdmc:/flashnx/`).

> 🕘 The phase-by-phase development log (Phases 0 → 4, dates, estimates) now lives in the `git log` history and the [CHANGELOG](CHANGELOG.md). This document is a **reference guide** for building, testing and contributing — not a log.

## Architecture decision

**Chosen option: switch-mesa (OpenGL)**, rather than dawn-switch (WebGPU):
- switch-mesa is mature (`dkp-pacman -S switch-mesa`), used in production by ScummVM, PPSSPP, RetroArch.
- dawn-switch is a 1-commit POC, depends on NVK Switch that is not publicly sourced.
- wgpu's GL backend *"only seems to work under a Mesa context"* — switch-mesa **is** a Mesa context.

**Hybrid C++/Rust** strategy (Ruffle requires `std`, not `no_std` → newlib via devkitPro):

```
cpp/ (devkitPro)  →  rust staticlib (Ruffle + backends)  →  switch-mesa GL  →  GPU Tegra X1
```

## Project structure

```
flash-for-switch/
├── cpp/
│   ├── Makefile                  # template devkitPro switch + APP_TITLE/AUTHOR/VERSION + --icon/--nacp
│   ├── src/
│   │   ├── main.cpp              # libnx init + worker thread + applet loop + joycon/touch input
│   │   ├── gl_context.cpp        # EGL/GL via switch-mesa, EGL_STENCIL_SIZE=8
│   │   ├── input.cpp             # (empty — placeholder, input lives in main.cpp)
│   │   ├── audio.cpp             # libnx audren wrapper + worker thread
│   │   ├── exception.cpp         # native __libnx_exception_handler (symbolizable crash log)
│   │   ├── swf_picker.cpp        # SD scan via opendir/readdir (works around the Horizon read_dir bug)
│   │   ├── net.cpp               # swkbd (URL + rename) + remote import helpers
│   │   └── ruffle_bridge.cpp     # ruffle_log_cstr + getrandom + sysconf stubs + svcGetInfo RAM
│   └── include/ruffle_bridge.h
├── rust/
│   ├── Cargo.toml                # crate-type = ["staticlib"], ruffle_core features=[audio,mp3,default_font] + jpeg-decoder patch
│   ├── rust-toolchain.toml       # nightly-x86_64-pc-windows-gnu + rust-src
│   ├── .cargo/config.toml        # target aarch64-nintendo-switch-freestanding + rustflags
│   └── src/
│       ├── lib.rs                # FFI exports + PlayerBuilder + SWF loader + input handlers + tick/render profiling
│       ├── library.rs            # Library UI state + banner/icon embed + SWF header parse + meta/keymap sidecars
│       ├── net.rs                # HTTPS transport: http_get (sync) + async curl multi download + swkbd prompts
│       ├── sources/             # Multi-source: classify (archive.org / direct .swf) + Flashpoint cover lookup (metadata only)
│       ├── covers.rs            # Local cover art: sidecar/cache/default resolution + PNG/JPEG decode + opt-in Flashpoint fetch
│       ├── keymap.rs             # JSON keymap (sidecar + default + fallback) + mutation API
│       ├── menu.rs               # TOUCHES sub-screen state machine (list + dropdown)
│       ├── loc.rs                # UI localization (EN/FR/ES/RU) + settings.json persistence
│       ├── ffi/gl.rs             # OpenGL FFI subset (hand-written, no bindgen) + glReadPixels/PACK_ALIGNMENT
│       └── backend/
│           ├── render.rs         # SwitchRenderBackend : atlas + UV wrap + GlStateCache + mega-arena +
│           │                     #   stencil masking INCR/DECR + Glow/DropShadow/Blur/ColorMatrix/Bevel filters +
│           │                     #   FilterTexturePool TTL + render_offscreen/resolve_sync_handle (BitmapData.draw) + UI overlays
│           ├── audio.rs          # SwitchAudioBackend (port CpalAudioBackend → libnx audren)
│           ├── storage.rs        # SwitchStorageBackend (port DiskStorageBackend → sdmc:/flashnx/ flat)
│           ├── tracing.rs        # Routes Ruffle's tracing events to nxlink stdout
│           └── log.rs            # SwitchLogBackend → ruffle_log_cstr
├── patches/
│   ├── README.md                 # How to re-apply after git submodule update
│   └── 0001-mario63-zero-scale-hit-test.patch  # Fix Toad castle #6906
├── third_party/
│   ├── ruffle/                   # git submodule + patches/*.patch applied
│   └── jpeg-decoder-switchfork/  # vendored jpeg-decoder-0.3.2, select_worker → Immediate forced
├── assets/{icon.jpg, banner.png, cacert.pem, screenshots/, *.nacp}
└── scripts/{build.sh, build.ps1, setup-env.ps1, setup-env.sh}
```

The Navigator/UI/Video backends use the `Null*` implementations provided by default by `ruffle_core` — no dedicated file. **Audio** = `SwitchAudioBackend`. **Storage** = `SwitchStorageBackend`.

### Assets embedded in the `.nro`

| Asset | Format | Dimensions | Usage |
|---|---|---|---|
| `assets/icon.jpg` | JPEG baseline sRGB | 256×256 | `.nro` icon picked up by hbmenu / Sphaira via `elf2nro --icon=` ([cpp/Makefile](cpp/Makefile)). |
| `assets/banner.png` | PNG RGBA | 720×144 (5:1 ratio) | Banner at the top of the library UI: embedded via `include_bytes!` in [rust/src/library.rs](rust/src/library.rs), decoded via the `png` 0.18 crate at boot, uploaded to a GL texture (`upload_rgba_texture`), rendered by `draw_textured_rect` (1 textured quad/frame). ASCII "FLASHNX" fallback if the decode fails. |
| `assets/cacert.pem` | PEM | — | Mozilla CA bundle for libcurl HTTPS (archive.org import), embedded via `include_bytes!` + written to SD on first boot. |

`assets/screenshots/` is only used by the README — not embedded in the `.nro`.

## Build & netload

```bash
# 1. BUILD (from Git Bash, at the repo root)
./scripts/build.sh            # release: LTO=full, ~3 min, official .nro
./scripts/build.sh --dev      # release-dev: LTO=thin + codegen-units=16, ~30 s rebuild
#   Equivalents: `make` (= build.sh) / `make dev` (= build.sh --dev) /
#   `scripts\build.ps1 [--dev]` from PowerShell. All delegate to build.sh.

# 2. NETLOAD (Switch: Homebrew Menu → Y to switch to netloader)
nxlink -s -p FlashNX/FlashNX.nro cpp/FlashNX.nro
#   The netloader does NOT run from RAM — it WRITES the received .nro to the SD
#   card under the hbmenu root (sdmc:/switch) and runs it from there. Without
#   `-p`, nxlink sends just the basename → it lands as sdmc:/switch/FlashNX.nro
#   (loose) and overwrites any installed copy there. `-p FlashNX/FlashNX.nro`
#   saves it to sdmc:/switch/FlashNX/FlashNX.nro (next to its config dir).
#   `-s` keeps the terminal attached to the Switch's stdout.
```

> ⚠️ `scripts/build.sh` is **the only supported build path**. Do not call
> `cargo build --target aarch64-unknown-linux-gnu` (wrong target → libc
> `target_env=gnu` failure). The correct target (`aarch64-nintendo-switch-freestanding`,
> tier-3, build-std) is provided by `rust/.cargo/config.toml`; build.sh runs
> `cargo build` without `--target`. The root `Makefile` and `build.ps1` are just
> thin wrappers around build.sh.

> 💡 The `-s` flag of `nxlink` keeps the terminal attached to the Switch's stdout:
> that's where the perf heartbeat (`f1234: fps=… tick=…ms
> render=…ms …`) and the `SLOW f…` lines from the slow-frame detector show up (see
> [rust/src/lib.rs](rust/src/lib.rs) `render_frame_with_dt`).

The script orchestrates:
1. `cargo build --release` (or `--profile release-dev`) on the Rust side (target `aarch64-nintendo-switch-freestanding`, std-via-newlib, build-std nightly) → `rust/target/.../libruffle_switch.a`.
2. `make` on the C++ side launched **inside devkitPro's MSYS2 bash** (so that `switch_rules` resolves the paths) → links against `libruffle_switch.a` + libnx + libEGL/libGLESv2 → `cpp/FlashNX.nro`.

The Makefile has `libruffle_switch.a` as an explicit dependency of the `.elf`, so any Rust change triggers the C++ relink automatically (no more manual `make clean`). The `release-dev` profile is selected via the `RUST_PROFILE` env var that `build.sh --dev` exports.

## Testing on Switch

1. Copy your `.swf` files to the SD card in **`sdmc:/flashnx/`** (or `sdmc:/switch/flashnx/`). Any file name works. The legacy `sdmc:/ruffle/` is also scanned for backward-compat. At boot the `.nro` opens the **FlashNX library**: list of detected `.swf` files with banner + per-game color chip.
   - **LOCAL inputs**: up/down = navigate (D-pad **or left stick or right stick** — **hold to scroll fast**), **A** = PLAY, **X** = OPTIONS (TOUCHES + RENAME + BACK), **Y** = toggle to REMOTE mode (archive.org import), **−** = quit the `.nro`.
   - **REMOTE mode**: **A** = enter an archive.org URL via the soft keyboard (`swkbd`); on DistantIdle the history is shown, **L/R** cycle through it, **ZR** = directly fetch the displayed URL without reopening the keyboard. On the remote file list, an `OK` badge next to the ones already on SD, **A** starts the download (silent no-op on the `OK` ones). Live progress bar, **B** cancels in progress.
   - **Empty state**: if the SD card is empty, the library shows "NO GAME" + instructions on where to put the `.swf` files. **Y** lets you go to REMOTE to download.
   - **Ultimate fallback** (library init fail): `ruffle_init` with a 43-byte embedded SWF (red background). The `.nro` never refuses to boot.
2. Switch in **netloader** mode: Homebrew Menu → `Y` (or `R` on older versions).
3. PC: `nxlink -s -p FlashNX/FlashNX.nro cpp/FlashNX.nro`.

**In-game controls** (default platformer binding; remappable via TOUCHES):

| Joycon | Flash action |
|---|---|
| A | Space (main jump) |
| B | Z (alt jump) |
| X | X (run/dive) |
| Y | Shift (alt run) |
| Left stick / D-pad | Arrows |
| **Right stick** | **Mouse cursor** (visible crosshair) |
| **ZR** or **touchscreen** | **Mouse click** |
| R | Enter ("Press Start") |
| L | Escape |
| Plus | P (standard pause key) |
| **Minus** | **Opens the pause menu** (RESUME / TOUCHES / RESTART / QUIT) |

In the pause menu: D-pad **or left stick or right stick** to navigate (**hold to scroll** in the TOUCHES editor), **A** confirms, **B** or **Minus** closes. **"QUIT" returns to the FlashNX library** (from the library, **−** = exit the `.nro`). "RESTART" reloads the SWF from scratch (keeps the `.sol` files), "TOUCHES" opens the keymap editor.

## Key customization

Two ways — the in-game editor is by far the simplest.

### In-game "TOUCHES" editor (recommended)

From the pause menu (**Minus** in-game) OR from OPTIONS in the library (**X** on a game before launching it), select **TOUCHES** + A. You see the list of Switch buttons with their current binding between `[ brackets ]`. Navigate with up/down (**hold to scroll fast**), **A** on a row opens a dropdown of the **48 supported Flash keys**:

- **Modifiers / nav**: `(none)`, `Space`, `Enter`, `Escape`, `Shift`, `Control`, `Alt`, `Tab`, `Backspace`
- **Arrows**: `Up`, `Down`, `Left`, `Right`
- **Letters**: `A`..`Z` · **Digits**: `0`..`9`

The dropdown is scrollable (10 visible, scrollbar on the right). **A** confirms, **B** cancels. On each confirmation, the JSON sidecar is saved to SD AND the binding applies immediately (no need to RESTART). The written sidecar is **`sdmc:/flashnx/<basename>.keymap.json`** — per game, without touching the global default.

### Manual JSON editing (power users)

The `.nro` reads / writes JSON files on the SD card.

**Lookup hierarchy** (first hit wins):
1. `sdmc:/flashnx/<basename>.keymap.json` — per-game override
2. `sdmc:/ruffle/<basename>.keymap.json` — legacy backward-compat
3. `sdmc:/flashnx/keymap_default.json` — global default chosen by you
4. `sdmc:/ruffle/keymap_default.json` — legacy
5. Hardcoded fallback in the `.nro` — the table above

On first boot, if no `keymap_default.json` exists anywhere, the `.nro` writes it to `sdmc:/flashnx/keymap_default.json` with the hardcoded fallback.

**Schema** (example):
```json
{
  "version": 1,
  "bindings": {
    "A": "Space", "B": "Z", "X": "X", "Y": "Shift",
    "L": "Escape", "R": "Enter", "Plus": "P",
    "Up": "Up", "Down": "Down", "Left": "Left", "Right": "Right",
    "StickLUp": "Up", "StickLDown": "Down", "StickLLeft": "Left", "StickLRight": "Right"
  }
}
```

**Supported Switch buttons**: `A`, `B`, `X`, `Y`, `L`, `R`, `ZL`, `Plus`, `Up`/`Down`/`Left`/`Right` (D-pad), `StickLUp`/`StickLDown`/`StickLLeft`/`StickLRight`. Buttons not listed = unbound. `Minus` is reserved for the pause menu. `ZR` is reserved for the in-game mouse click (available in the library in REMOTE mode for fetch-without-keyboard).

On each boot the `.nro` logs the final resolution via nxlink (`keymap: resolved 16 bindings: A=1 B=8 ...`).

## Display name customization (RENAME)

To rename a game **without touching the physical `.swf` file** (saves + keymap stay stable):

1. In the library, select the game, **X** = OPTIONS → **RENAME** + A.
2. swkbd opens pre-filled with the current name — edit it.
3. Submit → writes `sdmc:/flashnx/<basename>.meta.json` with `{"display_name": "..."}`. Empty field = deletes the sidecar = back to the basename.

The library reads this sidecar on every SD scan. The metadata panel still shows `[basename.swf]` in small text. **Steam/ScummVM/iTunes pattern**: display alias only. `.sol` saves, keymap and SharedObject URLs (`http://flashforswitch.local/<basename>`) stay stable.

## SD card layout

Everything lives **flat** in `sdmc:/flashnx/`:

```
sdmc:/flashnx/
├── Super_Mario_63_2010.swf                           ← the game itself
├── Super_Mario_63_2010.swf.keymap.json               ← per-game keymap
├── Super_Mario_63_2010.swf.meta.json                 ← display_name (rename)
├── Super_Mario_63_2010.swf.<SaveName>.sol            ← save (flat)
├── keymap_default.json                               ← global default keymap (edited via + → DEFAULT CONTROLS)
├── settings.json                                     ← UI language (EN/FR/ES/RU), set via + → LANGUAGE
└── ...
```

The **`+` button** in the library opens a global Settings modal: **DEFAULT CONTROLS** (edits `keymap_default.json` with the same editor as the per-game TOUCHES screen) and **LANGUAGE** (writes `settings.json`; auto-detected from the console language on first boot — see [rust/src/loc.rs](rust/src/loc.rs)).

**Backward-compat**: files in the old `sdmc:/ruffle/` are detected and used (scan + read-fallback). Saves in the old nested tree `sdmc:/ruffle/saves/<host>/<basename>/<sol>.sol` are read and then automatically migrated to the flat path on the next save.

**System files** (in the homebrew folder):
- `sdmc:/switch/FlashNX/cacert.pem` — Mozilla CA bundle for HTTPS
- `sdmc:/switch/FlashNX/distant_history.json` — archive.org URL history (persisted)
- `sdmc:/switch/ruffle-crash.log` — replay of the last native crash (seen at the next boot via nxlink)

## Known limitations

For playing common AS1/AS2 SWFs, it's functionally near-complete. Honest inventory of what's left.

### Rendering (backend)

| Missing | Impact |
|---|---|
| **Filters** GradientGlow / GradientBevel / Convolution / DisplacementMap | Dropped (passthrough). Done: ColorMatrix / Blur / Glow / DropShadow / Bevel. |
| **`BitmapData.draw()`**: clear-transparent (no composite over the existing content), temp texture per call (no pool) | OK for the tile-engine pattern (SMWF), not faithful for all BitmapData uses. |
| **Blend modes** Alpha / Erase (need layer tracking); nested blend (non-recursive offscreen FBO) | Trivial (Add/Screen…) and Complex (Multiply/Overlay…) are done. |
| **Filter perf in menu transitions** (N FBO passes/frame) | Hiccups on heavily filtered menus (Mario 63). Bounded by budget/frame + pool TTL. Real fix = batching the passes. |
| **Context3D / Stage3D**, **PixelBender** (AS3) | Not implemented (stubs). Near-zero for 2D AS1/2 Flash. |

### Ruffle core (out of our scope)

- **Perf of heavy games** — limit of the **Ruffle interpreter** (no JIT). On Mario 63 in a dense scene, the simulation `tick` can reach hundreds of ms/frame and grows over time (Mario 63 object/memory leak + Ruffle documented upstream); our rendering stays at ~5-15 ms/frame. **Not fixable from the backend** (web-verified: Ruffle lags even on an i7). Out-of-code lever: CPU overclock (dock mode / sys-clk).
- **AS3 / AVM2** — partially supported by Ruffle, worse perf (no JIT). `AS3` badge in the library.

### Platform / distribution

- **Savestate** — knowingly skipped: Ruffle does not expose a `Player::serialize()` (the state is a `gc-arena` graph). A real savestate = a ~2-week upstream effort. The native `.sol` saves work.
- **Library** — Deleting a game (with confirm), sorting, box art: not implemented.
- **hb-app.store packaging** + **extended user documentation** — for wider distribution.
- **Home menu forwarders** — doc-only ([Sphaira](https://github.com/ITotalJustice/sphaira) generates the NSP, no code on our side). ⚠️ Forwarders look like games to Nintendo: safe only on emuNAND.

## Constraints / facts to keep in mind

- **Pure interpreted AS2** (e.g. Mario 63) → AVM2 JIT not needed, Horizon `jit:u` service not required.
- **Ruffle deps**: `cpal`/`reqwest`/`tokio`/`wgpu` are **not** in `ruffle_core`, only in `ruffle_desktop` → nothing to neutralize. `flate2` workspace default = `miniz_oxide` (pure Rust). Everything links directly.
- **libnx FFI used**:
  - HID: `padConfigureInput`/`padInitializeDefault`/`padUpdate`/`padGetButtonsDown/Up`/`padGetStickPos`/`hidInitializeTouchScreen`/`hidGetTouchScreenStates` (in `cpp/src/main.cpp`)
  - Applet: `appletMainLoop`, `nwindowGetDefault`, `appletSetCpuBoostMode(FastLoad)`. The suspend/resume cycle (home button, sleep) is handled implicitly by `appletMainLoop` — no `appletHook` hooks needed in practice.
  - Socket: `socketInitializeDefault`, `nxlinkStdio` (network stdout)
  - FS: `sdmc:/...` auto-mounted by the libnx crt0 → `std::fs::read` works from Rust (except `read_dir` which is buggy — hence the C++ `opendir`/`readdir` scan)
  - Thread: `threadCreate`/`threadStart`/`threadWaitForExit`/`threadClose` (GL worker + audio worker)
  - System: `svcGetInfo` (RAM via `ruffle_query_ram`), `armGetSystemTick` (real dt pacing)
  - Audio: `audrenInitialize`/`audrvCreate`/`audrvVoiceInit`/`audrvVoiceAddWaveBuf`/`audrvUpdate`/… (cf. [cpp/src/audio.cpp](cpp/src/audio.cpp))
- **Bindgen**: not used. FFI hand-written in `rust/src/ffi/gl.rs` (GL 4.3 core subset) and `cpp/src/ruffle_bridge.cpp`.
- **Architecture pattern**: ScummVM `backends/platform/sdl/switch/` (OSystem → OSystem_SDL → OSystem_Switch separation). Adapted here: `RenderBackend` trait (from Ruffle) + thin `SwitchRenderBackend` impl.

## Toolchain

- **devkitPro** in `C:\devkitPro\` with packages `switch-dev`, `switch-mesa`, `switch-glm`, `switch-glad`, `switch-curl`, `switch-mbedtls`.
- **Rust**: toolchain pinned via `rust/rust-toolchain.toml` → `nightly-x86_64-pc-windows-gnu` + `rust-src` (GNU host required — MSVC breaks the build scripts without Visual Studio Build Tools).
- **MinGW-w64** via `scoop install mingw` — for `dlltool.exe` which Rust nightly GNU ships in a buggy state. `scripts/build.sh` adds `~/scoop/apps/mingw/current/bin` to the PATH before `cargo build`.
- **`third_party/jpeg-decoder-switchfork/`**: patched fork of `jpeg-decoder` 0.3.2 (`select_worker` always returns `Immediate`), referenced via `[patch.crates-io]` in `rust/Cargo.toml`. Without it, JPEGs > 128×128 px spawn a `std::thread` that crashes the newlib pthread shim.

### rust-src patches to re-apply after each `rustup update`

**Patch 1** — `…\nightly-x86_64-pc-windows-gnu\lib\rustlib\src\rust\library\std\build.rs`: add after the line `|| (target_vendor == "nintendo" && target_env == "newlib")`:

```rust
|| (target_vendor == "nintendo" && target_os == "horizon")
```

Without it, stdlib compiles in `restricted_std` mode → all the std crates from crates.io (memchr, num-traits, thiserror…) refuse to compile.

**Patch 2** — `…\nightly-x86_64-pc-windows-gnu\lib\rustlib\src\rust\library\std\src\hash\random.rs`: wrap the body of `RandomState::new()` in a cfg-switch:

```rust
pub fn new() -> RandomState {
    #[cfg(target_os = "horizon")]
    {
        use crate::sync::atomic::{AtomicU64, Ordering};
        static K0: AtomicU64 = AtomicU64::new(0x9E3779B97F4A7C15);
        let k0 = K0.fetch_add(1, Ordering::Relaxed);
        return RandomState { k0, k1: 0x517CC1B727220A95 };
    }
    #[cfg(not(target_os = "horizon"))]
    {
        // ... original code (thread_local) ...
    }
}
```

Without it, `HashMap::new()` then `.insert()` crashes on hardware (the lazy stdlib thread_local with per-function init crashes on our target). Hash-flooding DoS not relevant for a Flash player.

**Environment gotchas**:
- Avast Web Shield (HTTPS scanning) intercepts pacman/pkg.devkitpro.org by injecting its root CA → disable HTTPS scanning before `pacman -Sy`.
- Avast CyberCapture flags the cargo build scripts compiled to `.exe` → add an exception on the project folder.
- The chocolatey `make` doesn't handle devkitPro MSYS-style paths → `scripts/build.sh` delegates to `/c/devkitPro/msys2/usr/bin/bash -lc 'make'`.
- Target `aarch64-nintendo-switch-freestanding` = tier-3 → no pre-built rust-std → `-Z build-std` → nightly required.

## Hardware

- Atmosphère-modded Switch.
- nxlink for network stdout (debug) + netloader (push `.nro` over WiFi).
- SD: copy the `.nro` to `sdmc:/switch/FlashNX.nro` for SD mode (not required if netloading).
- SWFs looked up first in `sdmc:/flashnx/` (see "Testing on Switch").

## References

- Ruffle: https://github.com/ruffle-rs/ruffle (`render/src/backend.rs` for the trait to impl)
- aarch64-switch-rs: https://github.com/aarch64-switch-rs/{nx,cargo-nx}
- libnx docs: https://switchbrew.github.io/libnx/
- ScummVM Switch (pattern reference): `backends/platform/sdl/switch/`
- GBAtemp Switch homebrew dev: https://gbatemp.net/forums/switch-homebrew-development.300/
