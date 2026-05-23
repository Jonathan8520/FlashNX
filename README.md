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

## Build

```bash
./scripts/build.sh
```

Le script orchestre :
1. `cargo build --release` côté Rust (target `aarch64-nintendo-switch-freestanding`, no_std, build-std nightly) → `rust/target/.../libruffle_switch.a`
2. `make` côté C++ lancé **dans le bash MSYS2 de devkitPro** (pour que `switch_rules` résolve les paths correctement) → link contre `libruffle_switch.a` + libnx + libEGL/libGLESv2 → `cpp/flash-for-switch.nro`

**Phase 0 actuelle** : Rust est en no_std (juste FFI vers glClear). **Phase 1** : passera à un target custom JSON `std-via-newlib` pour pouvoir tirer `ruffle_core` qui nécessite std.

## Roadmap

### Phase 0 — fondation validée ✓ (2026-05-20)

- `cpp/main.cpp` ouvre fenêtre + GL context switch-mesa
- `rust/lib.rs` expose `ruffle_init()`, `ruffle_render_frame()`, `ruffle_shutdown()`
- Rendu = `glClear(rouge)`. **Confirmé sur Switch réelle :** écran rouge affiché, exit sur bouton +.
- Ce que ça a prouvé : cross-compile Rust ARM64 + staticlib link C++ devkitPro + FFI Rust↔C + switch-mesa sur hardware + pipeline `.nro` complète.

### Phase 0.5 — triangle réel (codée 2026-05-21, attend test hardware)

Avant le gros plongeon Phase 1, dérisquer un point précis : est-ce que des shaders GLSL compilent et tournent sur switch-mesa ?

- Vertex + fragment shader GLSL 330 core chargés depuis Rust no_std (`rust/src/lib.rs`)
- VBO + VAO d'un triangle RGB (pos.xy + col.rgb interleavé), `glDrawArrays`
- FFI GL côté Rust : `rust/src/ffi/gl.rs` (subset GL 3.3+ core)
- Callback log Rust → nxlink : `ruffle_log_cstr` dans `cpp/src/ruffle_bridge.cpp`
- Build OK, `.nro` 5.8 MB, **attend validation sur Switch réelle**

**Critères de succès attendus sur Switch :**
- Fond bleu nuit `rgb(13, 13, 26)` au lieu du rouge Phase 0
- Triangle centré sommet rouge (haut), vert (bas-gauche), bleu (bas-droite) avec gradient interpolé
- Pas de panic / pas d'écran noir → GLSL compile sur switch-mesa
- Si shader compile fail → message d'erreur visible via `nxlink -s`

### Phase 1 — intégration Ruffle (6-10 semaines)

Objectif : charger un `.swf` depuis la SD et voir *quelque chose* à l'écran (probablement buggé).

| Étape | Boulot | Risque |
|---|---|---|
| 1.1 ~~Custom `target.json` `std-via-newlib`~~ → cfg rustflags sur target upstream ✓ validée hardware 2026-05-21 | ~~1-2 semaines~~ ~1 h | **Résolu** : on pirate le target upstream avec `--cfg target_family=unix` + `--cfg target_env=newlib` + `--cfg unix` + `-Aexplicit_builtin_cfgs_in_flags` ; stdlib utilise les branches `target_os = "horizon"` déjà présentes pour le 3DS. `#![feature(restricted_std)]` côté lib. Triangle Phase 0.5 + `std::format!`/`Vec<u8>` au boot → confirmés sur Switch. `.nro` 5.83 MB (+40 KB vs Phase 0.5). |
| 1.2 ✓ Ajouter `ruffle_core` comme dep + validation hardware (2026-05-21) | ~4 h | **Résolu**. submodule pin `nightly-2026-05-19`. `cpal`/`reqwest`/`tokio`/`wgpu` ne sont PAS dans `core`, juste `desktop` → rien à neutraliser. `flate2` workspace par défaut = miniz_oxide (pure Rust). Blocages résolus : (a) MinGW dlltool manquant → `scoop install mingw` ; (b) `restricted_std` → **Patch 1** stdlib (build.rs) ; (c) `getrandom` 0.3 sans backend → `--cfg getrandom_backend="custom"` + impl xorshift dans `lib.rs` ; (d) `libc::getrandom` appelé par stdlib pour HashMap seeding → stub C dans `ruffle_bridge.cpp` ; (e) **lazy thread_local crash sur Horizon** → **Patch 2** stdlib (`hash/random.rs` cfg-gated AtomicU64). Bisection A-G a isolé : `BTreeMap` ✓, const thread_local ✓, getrandom direct ✓, **lazy thread_local avec init par fn = crash**. PlayerBuilder construit + droppé OK avec les 2 patches. `.nro` 5.84 MB. |
| 1.3.1 ✓ `SwitchRenderBackend` squelette validé hardware (2026-05-23) | ~3 h | **Résolu**. Tout le trait implémenté Null-style ; `submit_frame` exécute le clear ; `set_viewport_dimensions` appelle `glViewport`. `ruffle_init` exerce le trait via `Box<dyn RenderBackend>` pour défaire la dévirtualisation LTO. Hardware OK via nxlink. |
| 1.3.2 ✓ Tessellator + CommandList → glDrawElements (validé hardware 2026-05-23) | ~4 h | **Résolu**. `register_shape` lance `ShapeTessellator::tessellate_shape` (lyon) et upload chaque `Draw` en VAO/VBO/IBO ; les GL handles sont possédés par un `Arc<GpuShape>` dont le `Drop` libère via `glDeleteBuffers`/`glDeleteVertexArrays`. `submit_frame` implémente `CommandHandler` : `render_shape` redessine les caches en `glDrawElements(GL_TRIANGLES, ..., GL_UNSIGNED_INT)` avec la matrice Flash convertie en `mat3` (Flash 2x3 affine ∘ pixels→NDC Y-flippé) en uniform `u_world`. `draw_rect` upload un quad unitaire ré-coloré (`GL_DYNAMIC_DRAW`) puis `glDrawArrays`. Les 8 autres commandes (`render_bitmap`, `render_stage3d`, `render_alpha_mask`, `draw_line`, `draw_line_rect`, masking) no-op avec un `warn_once` borné à 8 messages pour pas spammer nxlink. Shader unique : `(pos.xy, rgba)` + `mat3 u_world`. **Hardware OK** : `ruffle_init` registre une `DistilledShape` triangle (lyon a tourné sur ARM64 Switch sans crash), puis chaque `ruffle_render_frame` construit une `CommandList` synthétique (3 `DrawRect` rouge/vert/bleu + 1 `RenderShape` triangle jaune sur `Transform::default()`) et l'envoie à `submit_frame`. Visuel confirmé : 3 rectangles en haut + triangle jaune au centre sur fond `#0D0D1A`. `.a` 447 → 645 KB (+200 KB lyon+tessellator) ; `.nro` 5.85 → 5.94 MB. |
| 1.3.3 Upload bitmap réel via `glTexImage2D`/`glTexSubImage2D` dans `register_bitmap`/`update_texture` + `render_bitmap` qui sample la texture | 3-5 jours | Faible |
| 1.3.4 Color transform (mult_rgba + add_rgba) via uniforms + DrawLine/DrawLineRect via `GL_LINES` | 2-3 jours | Faible |
| 1.3.5 Masking via stencil buffer (push_mask, activate_mask, deactivate_mask, pop_mask) | 4-6 jours | Moyen |
| 1.3.6 Gradient fills (linear/radial/focal) — texture 1D + matrice gradient + shader dédié | 4-6 jours | Moyen |
| 1.3 pivot wgpu-GL : *non-applicable* — on est parti en GL natif dès 1.3.1. |  |  |
| 1.4 Stubs `NavigatorBackend` (no-op), `UiBackend` (minimal), `StorageBackend` (sdmc:/), `LogBackend` (nxlink) | 2-3 jours | Faible |
| 1.5 Frontend C++ : file picker `.swf` depuis `sdmc:/switch/ruffle/`, pump `Player.tick()` chaque frame | 2-3 jours | Faible |

**Pivot Phase 1.3 si wgpu-GL casse sur switch-mesa :** *non-applicable* — on est déjà parti directement en GL natif (pas via wgpu). Décision prise dès Phase 1.3.1 vu le risque connu wgpu-GL/mesa.

**Pivot Phase 1.1 si std-via-newlib échoue :** fork Ruffle pour le rendre no_std (énorme, plusieurs mois) OU pivoter vers un autre player Flash open-source (peu de candidats). C'est le risque qui peut tuer le projet.

### Phase 2 — Mario 63 jouable (3-6 mois après Phase 1)

À ce stade un `.swf` charge mais probablement plein de bugs visuels/comportementaux. Phase 2 c'est rendre *un* jeu (Mario 63) jouable de bout en bout :

- Backend audio réel via audren (au lieu du stub silencieux)
- Mapping joycon → événements souris/clavier Flash (Mario 63 utilise beaucoup le clavier)
- Bugs `RenderBackend` qui sortent uniquement sur du contenu non-trivial (sprites multiples, masques, gradients)
- Quirks Mario 63 spécifiques découverts en jouant (timing, state machines AS2)
- Performance : viser 60 FPS sur Tegra X1

### Phase 3 — polish + distribution (1-2 mois)

- Cycle applet (focus-lost, suspend/resume, libération GPU)
- `.nacp` metadata, icon final, packaging hb-app.store
- README utilisateur, instructions install SD
- Compat globale : tester sur d'autres `.swf` AS1/AS2 populaires (Madness, Newgrounds classics)

### Verdict timeline solo

- Premier `.swf` qui affiche un truc : **6-10 semaines**
- Mario 63 jouable : **+3-6 mois**
- Release publique propre : **+1-2 mois**
- **Total estimé : 6-12 mois solo** sur du temps de soir/weekend soutenu

Ces estimations supposent que Phase 1.1 (std-via-newlib) marche. Si ça casse, multiplier par 2-3.

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

## Toolchain installée (état actuel)

- **devkitPro** dans `C:\devkitPro\` avec packages `switch-dev`, `switch-mesa`, `switch-glm`, `switch-glad`
- **Rust** : toolchain pin via `rust/rust-toolchain.toml` → `nightly-x86_64-pc-windows-gnu` + `rust-src` (host GNU obligatoire — MSVC casse les build scripts sans Visual Studio Build Tools)
- **MinGW-w64** via `scoop install mingw` (16.1.0) — pour `dlltool.exe` que Rust nightly GNU embarque buggé. Ajouter `~/scoop/apps/mingw/current/bin` au PATH avant `cargo build` (le `scripts/build.sh` le fait déjà).
- LLVM/CMake/Python : pas nécessaires pour Phase 0/1 ; viendront si Phase 2 a besoin de bindgen libnx pour `audren`.

### Patches rust-src à ré-appliquer après chaque `rustup update`

**Patch 1** — `C:\Users\Jlevy\.rustup\toolchains\nightly-x86_64-pc-windows-gnu\lib\rustlib\src\rust\library\std\build.rs` : ajouter après la ligne `|| (target_vendor == "nintendo" && target_env == "newlib")` :

```rust
|| (target_vendor == "nintendo" && target_os == "horizon")
```

Sans ça, stdlib se compile en mode `restricted_std` → tous les crates std de crates.io (memchr, simd-adler32, num-traits, thiserror, etc.) refusent de compiler. Cargo ne nous laisse pas overrider `CARGO_CFG_TARGET_ENV` proprement, d'où ce patch.

**Patch 2** — `C:\Users\Jlevy\.rustup\toolchains\nightly-x86_64-pc-windows-gnu\lib\rustlib\src\rust\library\std\src\hash\random.rs` : envelopper le corps de `RandomState::new()` dans un cfg-switch :

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
        // ... code original (thread_local) ...
    }
}
```

Sans ça, `HashMap::new()` puis `.insert()` crash sur hardware. Le lazy thread_local de stdlib avec init par fonction crashe sur notre target (bisection A-G validée 2026-05-21). Hash-flooding DoS est non-pertinent pour un player Flash.

**Gotchas rencontrés et résolus :**
- Avast Web Shield (HTTPS scanning) intercepte les connexions pacman/pkg.devkitpro.org en injectant son propre root CA → désactiver le « scan HTTPS » dans Avast avant `pacman -Sy`.
- Avast CyberCapture flag `target/release/build/build-script-build.exe` (cargo build script compilé en .exe Windows) à chaque build → ajouter une exception sur le dossier du projet.
- Le `make` chocolatey ne gère pas les paths MSYS-style de devkitPro → `scripts/build.sh` délègue à `/c/devkitPro/msys2/usr/bin/bash -lc 'make'` qui voit `/opt/devkitpro/...` correctement.
- Le target `aarch64-nintendo-switch-freestanding` est tier-3 → pas de rust-std pré-built → `-Z build-std` requis → nightly requis.

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
