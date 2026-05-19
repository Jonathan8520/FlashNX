# flash-for-switch

Port [Ruffle](https://github.com/ruffle-rs/ruffle) (Flash player Rust) sur Nintendo Switch en `.nro`. Cible : faire tourner **Super Mario 63** (AS2) et tout `.swf` AS1/AS2 depuis la SD.

## Décision d'architecture

**Option B : switch-mesa (OpenGL).** Choisie sur Option A (dawn-switch/WebGPU) parce que :
- switch-mesa est mature (`dkp-pacman -S switch-mesa`), utilisé en prod par ScummVM, PPSSPP, RetroArch
- dawn-switch est un POC 1-commit, dépend de NVK Switch non sourcé publiquement
- Le backend GL de wgpu *« only seems to work under a Mesa context »* — switch-mesa EST un contexte Mesa

Stratégie **hybride C++/Rust** (Ruffle nécessite `std`, pas no_std → newlib via devkitPro).

```
cpp/ (devkitPro)  →  rust staticlib (Ruffle + backends)  →  switch-mesa GL  →  GPU Tegra X1
```

## Structure projet

```
flash-for-switch/
├── Makefile                      # orchestration top-level
├── cpp/
│   ├── Makefile                  # template devkitPro switch
│   ├── src/
│   │   ├── main.cpp              # libnx init + applet loop
│   │   ├── gl_context.cpp        # EGL/GL via switch-mesa
│   │   ├── input.cpp             # padInit, padUpdate → events
│   │   ├── audio.cpp             # audren init/voices/buffers
│   │   └── ruffle_bridge.cpp     # appels Rust staticlib
│   └── include/ruffle_bridge.h
├── rust/
│   ├── Cargo.toml                # crate-type = ["staticlib"]
│   ├── build.rs                  # bindgen libnx
│   ├── .cargo/config.toml        # target aarch64-unknown-linux-gnu
│   └── src/
│       ├── lib.rs                # #[no_mangle] extern "C" exports
│       ├── ffi/                  # bindings libnx (audren/hid/applet)
│       ├── backend/
│       │   ├── render.rs         # RenderBackend (wgpu GL)
│       │   ├── audio.rs          # AudioBackend → audren FFI
│       │   ├── ui.rs             # stub
│       │   ├── navigator.rs      # stub
│       │   ├── storage.rs        # sdmc:/
│       │   └── log.rs            # nxlink stdout
│       └── player.rs             # PlayerBuilder + lifecycle
├── third_party/ruffle/           # git submodule, pin tag stable
├── assets/{icon.jpg, *.nacp}
└── scripts/{setup-env.ps1, build.ps1}
```

## Build flow

```
cargo build --release --target aarch64-unknown-linux-gnu
   → rust/target/.../libruffle_switch.a
make -C cpp/
   → link contre libruffle_switch.a + libnx + libEGL/libGLESv2 (switch-mesa)
   → cpp/flash-for-switch.nro
```

## Phases

**Phase 0 — hello triangle** (2-4 semaines)
- `cpp/main.cpp` ouvre fenêtre + GL context switch-mesa
- `rust/lib.rs` expose `ruffle_init()`, `ruffle_render_frame()`, `ruffle_shutdown()`
- Rendu = `glClear(rouge)`. Si rouge à l'écran sur Switch → fondation validée.

**Phase 1 — intégration Ruffle** (2-4 mois)
- submodule `third_party/ruffle/`, link `ruffle_core` depuis rust/
- Impl `RenderBackend` MVP (5 méthodes) : `submit_frame`, `register_shape`, `register_bitmap`, `update_texture`, `viewport_dimensions`
- Impl `AudioBackend` via audren (FFI libnx C — la crate `nx` n'expose pas audren)
- Stubs : `NavigatorBackend`, `UiBackend`, `LogBackend` → nxlink
- Frontend : file picker `.swf` depuis `sdmc:/switch/ruffle/`

**Phase 2 — polish** (1-2 mois)
- Cycle applet (focus-lost, suspend/resume, libération GPU)
- Mapping joycon → événements souris/clavier Flash
- `.nacp` metadata, icon, packaging hb-app.store

**Pivot si Phase 1 casse sur wgpu-GL :** écrire `RenderBackend` directement en GL natif (sans wgpu).

## Contraintes / faits à retenir

- **Mario 63 = AS2 pur interprété.** Pas d'AVM2 JIT requis → service Horizon `jit:u` non nécessaire.
- **Ruffle deps à neutraliser :** `cpal` (remplacer par notre AudioBackend), `flate2` feature `rust_backend`, `reqwest` stub via `NavigatorBackend`. `tokio` n'est PAS dans `ruffle_core`.
- **libnx symbols à bindgen :**
  - HID : `hidInitialize`, `padConfigureInput`, `padInitializeDefault`, `padUpdate`, `padGetButtons`, `padGetStickPos`
  - Audio : `audrenInitialize`, `audrenStartAudioRenderer`, `audrvCreate`, `audrvMemPoolAdd/Attach`, `audrvVoiceInit`, `audrvVoiceAddWaveBuf`, `audrvUpdate`
  - Applet : `appletMainLoop`, `appletGetCurrentFocusState`, `appletHook`
  - Socket : `socketInitializeDefault`, `nxlinkStdio` (debug stdout réseau)
  - FS : rien à wrapper — `sdmc:/` monté auto par crt0 libnx, `fopen("sdmc:/...")` marche direct
- **bindgen config :** `.use_core() + .ctypes_prefix("core::ffi")` (Rust ≥1.64).
- **Cross Windows → Switch :** LLVM Windows + `LIBCLANG_PATH`, clang `-target aarch64-none-elf --sysroot=$DEVKITPRO/devkitA64/aarch64-none-elf -isystem $LIBNX/include`. Piège : chemins MSYS vs Windows.
- **Pattern d'architecture à copier :** ScummVM `backends/platform/sdl/switch/` — séparation OSystem → OSystem_SDL → OSystem_Switch. Appliquer en Rust : trait `Backend` portable + `SwitchBackend` mince.

## Toolchain requise

- devkitPro + meta-package `switch-dev` + `dkp-pacman -S switch-mesa`
- Rust nightly + `rustup component add rust-src`
- LLVM Windows (libclang.dll), CMake ≥3.20, Python 3
- Env : `DEVKITPRO`, `DEVKITA64`, `LIBCLANG_PATH`

## Hardware

- Switch moddée Atmosphère
- nxlink pour stdout réseau (debug)
- SD : `/switch/flash-for-switch.nro`, `/switch/ruffle/*.swf`

## Références

- Ruffle : https://github.com/ruffle-rs/ruffle (`render/src/backend.rs` pour le trait à impl)
- aarch64-switch-rs : https://github.com/aarch64-switch-rs/{nx,cargo-nx}
- libnx doc : https://switchbrew.github.io/libnx/
- ScummVM Switch (pattern reference) : `backends/platform/sdl/switch/`
- Mario 63 source : https://github.com/runouw/Super-Mario-63
- GBAtemp Switch homebrew dev : https://gbatemp.net/forums/switch-homebrew-development.300/
