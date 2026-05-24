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

## Structure projet (état actuel)

```
flash-for-switch/
├── cpp/
│   ├── Makefile                  # template devkitPro switch
│   ├── src/
│   │   ├── main.cpp              # libnx init + worker thread + applet loop + joycon/touch input
│   │   ├── gl_context.cpp        # EGL/GL via switch-mesa, EGL_STENCIL_SIZE=8
│   │   ├── input.cpp             # (vide — placeholder, l'input est dans main.cpp)
│   │   ├── audio.cpp             # libnx audren wrapper + worker thread (Phase 2.2)
│   │   └── ruffle_bridge.cpp     # ruffle_log_cstr + getrandom + sysconf stubs + svcGetInfo RAM
│   └── include/ruffle_bridge.h
├── rust/
│   ├── Cargo.toml                # crate-type = ["staticlib"], ruffle_core features=[audio,mp3] + jpeg-decoder patch
│   ├── rust-toolchain.toml       # nightly-x86_64-pc-windows-gnu + rust-src
│   ├── .cargo/config.toml        # target aarch64-nintendo-switch-freestanding + rustflags
│   └── src/
│       ├── lib.rs                # FFI exports + PlayerBuilder + SWF loader + input handlers
│       ├── ffi/gl.rs             # OpenGL FFI subset (no bindgen — hand-written)
│       └── backend/
│           ├── render.rs         # SwitchRenderBackend (~1900 lignes, 4 shaders, atlas, edge replication, UV wrap)
│           ├── audio.rs          # SwitchAudioBackend (port CpalAudioBackend → libnx audren)
│           ├── tracing.rs        # Routes Ruffle's tracing events to nxlink stdout
│           └── log.rs            # SwitchLogBackend → ruffle_log_cstr
├── third_party/
│   ├── ruffle/                   # git submodule, master @ e41992ab (2026-05-24)
│   └── jpeg-decoder-switchfork/  # vendored jpeg-decoder-0.3.2 with select_worker → Immediate forced
├── assets/{icon.jpg, *.nacp}
└── scripts/{build.sh, setup-env.ps1, setup-env.sh}
```

Les backends Navigator/UI/Storage/Video utilisent les implémentations `Null*` que ruffle_core fournit par défaut — pas de fichier dédié. **Audio** = `SwitchAudioBackend` (Phase 2.2).

## Build

```bash
./scripts/build.sh
```

Le script orchestre :
1. `cargo build --release` côté Rust (target `aarch64-nintendo-switch-freestanding`, std-via-newlib, build-std nightly) → `rust/target/.../libruffle_switch.a` (~13-14 MB avec features audio+mp3)
2. `make` côté C++ lancé **dans le bash MSYS2 de devkitPro** (pour que `switch_rules` résolve les paths correctement) → link contre `libruffle_switch.a` + libnx + libEGL/libGLESv2 → `cpp/flash-for-switch.nro` (~12.2 MB)

**État Mai 2026 (Phase 2.1 + 2.2 + 2.1.b ✓)** : full std via les 2 patches stdlib (voir plus bas), `ruffle_core` linké avec features `audio` + `mp3`, **Mario 63 jouable longue durée avec jetpack + audio + sprites complets** (atlas 2048×2048, edge replication, UV wrap shader, audren via SwitchAudioBackend qui porte le pattern CpalAudioBackend, mega-buffer arena GL pour éviter la saturation handles Mesa, libnx `__libnx_exception_handler` natif pour diagnose tout crash).

## Tester sur Switch

1. Copier ton `.swf` sur la SD à un des chemins reconnus :
   - `sdmc:/ruffle/test.swf` (de préférence)
   - `sdmc:/ruffle/mario.swf`
   - `sdmc:/ruffle/Super_Mario_63_2010.swf`
   - `sdmc:/switch/ruffle/test.swf`
   - Sinon : fallback embarqué (43-octet `SimpleRedBackground.swf` → fond rouge)
2. Switch en mode **netloader** : Homebrew Menu → `Y` (ou `R` sur anciennes versions)
3. PC : `nxlink -s cpp/flash-for-switch.nro`

**Contrôles** (Mario 63 et autres jeux Flash) :

| Joycon | Action Flash |
|---|---|
| A | Space (jump principal) |
| B | Z (alt jump) |
| X | X (run/dive) |
| Y | Shift (alt run) |
| Stick gauche / D-pad | Flèches |
| **Stick droit** | **Curseur souris** (crosshair visible) |
| **ZR** ou **écran tactile** | **Clic souris** |
| Minus | Enter (« Press Start ») |
| L | Escape |
| Plus | Quitter le `.nro` |

## Roadmap

### Phase 0 — fondation validée ✓ (2026-05-20)

- `cpp/main.cpp` ouvre fenêtre + GL context switch-mesa
- `rust/lib.rs` expose `ruffle_init()`, `ruffle_render_frame()`, `ruffle_shutdown()`
- Rendu = `glClear(rouge)`. **Confirmé sur Switch réelle :** écran rouge affiché, exit sur bouton +.
- Ce que ça a prouvé : cross-compile Rust ARM64 + staticlib link C++ devkitPro + FFI Rust↔C + switch-mesa sur hardware + pipeline `.nro` complète.

### Phase 0.5 — triangle GLSL réel ✓ (validée hardware 2026-05-21)

Vertex + fragment shader GLSL 330 core chargés depuis Rust, VBO/VAO d'un triangle RGB. Confirme que les shaders GLSL compilent et tournent sur switch-mesa. Triangle visible sommet rouge (haut), vert (bas-gauche), bleu (bas-droite) sur fond bleu nuit.

### Phase 1 — intégration Ruffle ✓ (bouclée en ~6 jours au lieu des 6-10 semaines estimées)

Objectif initial : charger un `.swf` depuis la SD et voir quelque chose à l'écran. **Atteint et dépassé** — Mario 63 jouable avec input complet.

| Étape | Boulot | Risque |
|---|---|---|
| 1.1 ~~Custom `target.json` `std-via-newlib`~~ → cfg rustflags sur target upstream ✓ validée hardware 2026-05-21 | ~~1-2 semaines~~ ~1 h | **Résolu** : on pirate le target upstream avec `--cfg target_family=unix` + `--cfg target_env=newlib` + `--cfg unix` + `-Aexplicit_builtin_cfgs_in_flags` ; stdlib utilise les branches `target_os = "horizon"` déjà présentes pour le 3DS. `#![feature(restricted_std)]` côté lib. Triangle Phase 0.5 + `std::format!`/`Vec<u8>` au boot → confirmés sur Switch. `.nro` 5.83 MB (+40 KB vs Phase 0.5). |
| 1.2 ✓ Ajouter `ruffle_core` comme dep + validation hardware (2026-05-21) | ~4 h | **Résolu**. submodule pin `nightly-2026-05-19`. `cpal`/`reqwest`/`tokio`/`wgpu` ne sont PAS dans `core`, juste `desktop` → rien à neutraliser. `flate2` workspace par défaut = miniz_oxide (pure Rust). Blocages résolus : (a) MinGW dlltool manquant → `scoop install mingw` ; (b) `restricted_std` → **Patch 1** stdlib (build.rs) ; (c) `getrandom` 0.3 sans backend → `--cfg getrandom_backend="custom"` + impl xorshift dans `lib.rs` ; (d) `libc::getrandom` appelé par stdlib pour HashMap seeding → stub C dans `ruffle_bridge.cpp` ; (e) **lazy thread_local crash sur Horizon** → **Patch 2** stdlib (`hash/random.rs` cfg-gated AtomicU64). Bisection A-G a isolé : `BTreeMap` ✓, const thread_local ✓, getrandom direct ✓, **lazy thread_local avec init par fn = crash**. PlayerBuilder construit + droppé OK avec les 2 patches. `.nro` 5.84 MB. |
| 1.3.1 ✓ `SwitchRenderBackend` squelette validé hardware (2026-05-23) | ~3 h | **Résolu**. Tout le trait implémenté Null-style ; `submit_frame` exécute le clear ; `set_viewport_dimensions` appelle `glViewport`. `ruffle_init` exerce le trait via `Box<dyn RenderBackend>` pour défaire la dévirtualisation LTO. Hardware OK via nxlink. |
| 1.3.2 ✓ Tessellator + CommandList → glDrawElements (validé hardware 2026-05-23) | ~4 h | **Résolu**. `register_shape` → `ShapeTessellator::tessellate_shape` (lyon), upload VAO/VBO/IBO, possédés par `Arc<GpuShape>`. `submit_frame` implémente `CommandHandler` ; `render_shape` redessine en `glDrawElements`. Shader `(pos.xy, rgba) + mat3 u_world` (Flash affine ∘ pixels→NDC Y-flippé). |
| 1.3.3 ✓ Bitmaps texturés (validé hardware 2026-05-23) | ~2 h inclus dans le rush 1.3 | **Résolu**. `register_bitmap` upload via `glTexImage2D` (RGBA8 + nearest/linear filter + clamp wrap). `update_texture` via `glTexSubImage2D` sur le rectangle de `PixelRegion`. `BitmapHandle` possédé par `Arc<GpuTexture>` → `Drop` appelle `glDeleteTextures`. `render_bitmap` scale la matrice par `(bitmap.width, bitmap.height)` puis dessine le quad bitmap (pos+uv) avec le shader bitmap. Démo : checkerboard 16x16 RGBA construit en Rust + scalé ×8 sur écran. |
| 1.3.4 ✓ Color transform + lignes (validé hardware 2026-05-23) | ~1 h inclus dans le rush 1.3 | **Résolu**. Tous les shaders prennent `uniform vec4 u_mult, u_add` ; `frag = clamp(col * u_mult + u_add, 0, 1)`. `DrawLine` upload une ligne unité (0,0)→(1,0) en `GL_DYNAMIC_DRAW` puis `glDrawArrays(GL_LINES, 0, 2)`. `DrawLineRect` envoie 8 vertices = 4 segments. `DrawRect` reste sur le shader solide en color transform identité (pas de Transform argument). Démo : ligne jaune horizontale + carré cyan teinté via `ColorTransform { r/g/b_multiply: 0.5, g_add: 80 }`. |
| 1.3.5 ✓ Masking via stencil (validé hardware 2026-05-23) | ~2 h inclus dans le rush 1.3 | **Résolu**. La config EGL ([cpp/src/gl_context.cpp:32](cpp/src/gl_context.cpp#L32)) demande déjà `EGL_STENCIL_SIZE=8` donc aucun changement C++ requis. State machine 4-temps : `push_mask` → `glColorMask(false×4)` + `glStencilFunc(ALWAYS, value, 0xFF)` + `glStencilOp(KEEP, KEEP, REPLACE)` (dessin du masque dans le stencil seul) ; `activate_mask` → réactive la couleur, `glStencilFunc(EQUAL, value, 0xFF)` + `glStencilOp(KEEP, KEEP, KEEP)` (le maskee passe seulement où le masque a écrit) ; `deactivate_mask` revient en mode "dessiner masque" pour pop propre ; `pop_mask` désactive le stencil si plus de mask actif. Profondeur de nesting jusqu'à 8 via bitmasks dans `active_value`. Démo : rect orange 400x140 visible seulement à travers un petit masque carré 100x80. |
| 1.3.6 ✓ Gradients (validé hardware 2026-05-23) | ~3 h inclus dans le rush 1.3 | **Résolu**. Le tesselator donne déjà `DrawType::Gradient { matrix, gradient }` où `matrix` est déjà inversée + normalisée par `swf_to_gl_matrix` (lp.x ∈ [0,1] pour linéaire). On bake chaque `Gradient.records` en texture RGBA 256×1 (interpolation linéaire entre stops voisins). Shader dédié : prend `u_grad_local` mat3, `u_grad_kind` (0=lin/1=rad/2=focal), `u_grad_spread` (0=pad/1=reflect/2=repeat), `u_grad_focal` ; calcule `t` puis sample la texture 1D. Focal traité comme radial avec offset approx. Démo : 2 rects 200×100 — un avec gradient horizontal rouge→bleu, un avec gradient radial blanc→violet. |
| 1.3 pivot wgpu-GL : *non-applicable* — on est parti en GL natif dès 1.3.1. |  |  |
| 1.4 ✓ Backends Null + `SwitchLogBackend` (validé hardware 2026-05-24) | ~30 min | **Résolu**. Ruffle's Null backends + stub `sysconf()` pour `jpeg_decoder`. |
| 1.5.a ✓ Player.build() + tick/render empty movie (validé hardware 2026-05-24) | ~2 h | **Résolu**. AVM1+AVM2+GC arena+Stage tournent sur Horizon. `.nro` 5.95 → 11.6 MB. |
| 1.5.b ✓ Real SWF parser + render (validé hardware 2026-05-24) | ~30 min | **Résolu**. `std::fs::read` + `SwfMovie::from_data` sur Switch. Fallback embarqué `SimpleRedBackground.swf` 43 octets. Background rouge confirmé visuellement. |
| 1.5.c ✓ Input keyboard+mouse+touch via joycon (validé hardware 2026-05-24) | ~3 h | **Résolu**. FFI `ruffle_handle_key`/`ruffle_handle_mouse_move`/`ruffle_handle_mouse_button`. Mapping joycon : A=Space, B=Z, X=X, Y=Shift, D-pad/L-stick=arrows, Minus=Enter, L=Escape, stick droit=souris, ZR=clic, **écran tactile**=tap. Crosshair overlay (rouge quand clic). [[feedback-flash-input-mouse]] : Flash games ont souvent besoin de clic souris pour leurs menus, même les platformers. |
| 1.5.d ✓ **Mario 63 jouable sur Switch** (validé hardware 2026-05-24) | ~1 h | **Résolu**. 15.3 MB SWF v8 450x300 chargé depuis `sdmc:/ruffle/Super_Mario_63_2010.swf` (path candidates list pour contourner bug `read_dir` Horizon). AS2/AVM1 bytecode exécute, trace `Runouw.com` visible, input réactif, "Press Start" passé, premier niveau chargé. **Limitation actuelle** : sprites en blocs blancs car `register_shape` ne résout pas `DrawType::Bitmap` (cap 0 — voir 1.5.e ci-dessous). |
| 1.5.e ✓ Texture atlas pour bitmap fills (validé hardware 2026-05-24 fin journée) | ~3 h | **Résolu**. `Atlas` skyline packer 2048×2048 RGBA (16 MB) avec edge-pixel replication dans `Atlas::upload_region_padded`. Shaders `BITMAP_PROG` et `SHAPE_BITMAP_PROG` avec `u_uv_remap` + `u_wrap_mode` (clamp/fract selon `is_repeating`). Bitmaps comme Mario 63 ground fills tilés rendent correctement sans bleeding sur d'autres bitmaps de l'atlas. `PER_SHAPE_BITMAP_BUDGET = usize::MAX`. ~1200 bitmaps Mario 63 décodés + atlasés sans crash. |
| 1.5.e.bis ✓ Fork jpeg-decoder pour Switch | ~30 min diag + 30 min fix | **Le diagnostic** : crash à frame ~40 avec budget>0 vient du fait que `jpeg_decoder` 0.3.2 (sans rayon) utilise quand même `std::thread::spawn` quand `width * height > 128*128 px`. La newlib pthread shim de devkitPro crashe natif silencieusement sur ce spawn. **Le fix** : `third_party/jpeg-decoder-switchfork/` patche `select_worker` pour toujours retourner `Immediate` (mono-thread). Référencé via `[patch.crates-io] jpeg-decoder = { path = "..." }` dans `rust/Cargo.toml`. Coût perf : décodage JPEG passe de multi-thread à single-thread (~5 ms / JPEG vs ~1 ms estimé multi-thread), preload total +~5 sec mais stable. |
| 1.5.f File picker C++ : scan `sdmc:/ruffle/*.swf`, UI de sélection joycon, passer le path à Rust | 2-3 jours | Faible. Bloqué partiellement par bug `std::fs::read_dir` Horizon (filenames tronqués de 2 chars). Workaround actuel : liste hardcodée de chemins candidats. |

### Phase 2 — finir Mario 63 (sprites ✓ + son ✓ + reste)

À ce stade Mario 63 charge, l'AS2 exécute, l'input répond, le premier niveau se joue **avec sprites visibles + son audible (musique + SFX)**. Il manque :

| Étape | Boulot | Risque |
|---|---|---|
| 2.1 ✓ Sprites visibles (validé hardware 2026-05-24) | ~4 h debug + ~1 h fix | **Résolu**. Voir 1.5.e + 1.5.e.bis ci-dessus. Aussi un bug d'UV wrap (Mario apparaissait sur le sol) corrigé en pousser `fract/clamp` dans le fragment shader avant le remap atlas. Et un bug d'edge bleed (lignes noires entre sprites avec LINEAR filtering) corrigé en répliquant les pixels du bord dans le pad atlas. |
| 2.2 ✓ Audio audren (validé hardware 2026-05-24 fin journée) | ~3 h | **Résolu**. [cpp/src/audio.cpp](cpp/src/audio.cpp) wrappe `audrenInitialize`/`audrvCreate`/`audrvVoiceInit`/`audrvVoiceAddWaveBuf` + worker thread libnx (NUM_WAVE_BUFS=4, 4096 frames each, ~340 ms cushion). Côté Rust, [rust/src/backend/audio.rs](rust/src/backend/audio.rs) = port de `frontend-utils/CpalAudioBackend`: wraps `ruffle_core::AudioMixer` + `impl_audio_mixer_backend!` macro, expose `proxy` via `OnceLock<Mutex<>>` que le C++ pull via `ruffle_audio_fill_buffer`. Features `ruffle_core = ["audio", "mp3"]` (Mario 63 utilise MP3 pour TOUT son audio incl. SFX, +250 KB symphonia mais indispensable). `mixer.set_volume(0.5)` pour éviter clipping (sans, `max_seen=32767` constant → grésillements audibles ; avec, `max_seen=6009` propre). |
| 2.1.b ✓ Mega-buffer arena + libnx exception handler (validé hardware 2026-05-25 nuit) | ~3 h diag + ~2 h refactor | **Résolu**. Bug : Mario 63 + rocket-nozzle FLUDD particle system émet ~3 shapes/frame, accumulés sans relâche par Ruffle. À ~27 000 GpuDraws live (= ~83 000 VBO/IBO/VAO handles GL côté Mesa-NVK Switch), `glBindBuffer` segfault dans une table interne saturée (DataAbort, `x24=GL_ARRAY_BUFFER`, FAR=index 0x1011). Le crash bypassait le `panic_hook` Rust car c'est une faute native. **Fixes appliqués :** (a) [cpp/src/exception.cpp](cpp/src/exception.cpp) — `__libnx_exception_handler` weak-override (32 KB dedicated stack) qui dump PC/LR/SP/FAR/ESR + x0–x28 vers nxlink + `sdmc:/switch/ruffle-crash.log`. (b) [cpp/src/main.cpp](cpp/src/main.cpp) — boot-replay du `ruffle-crash.log` au lancement suivant. (c) [rust/src/backend/render.rs](rust/src/backend/render.rs) — `BufferArena` (1 mega-VBO 64 MB + 1 mega-IBO 32 MB, freelist coalesçant), `PENDING_FREES` queue drainée au top de `submit_frame`, `GpuDraw` reshape en `{vbo_offset, vbo_size, ibo_offset, ibo_size, num_indices, kind}`, single global `shape_vao` configuré une fois au boot, render path utilise `glDrawElementsBaseVertex`. **Gotcha critique :** l'alignement arena VBO doit égaler le vertex stride (24 bytes), pas une puissance de 2 (16 cassait base_vertex). Round-up générique `((x+a-1)/a)*a` au lieu de `& !(a-1)`. **Résultat hardware :** 18 720 frames / 1.2 M bitmap_draws / 30 502 live draws au test, exit propre via Plus. Phase 2.4 = bugs Ruffle upstream (Toad NPC manquant dans château, "non-registered character" errors) prend le relais. |
| 2.3 ⏳ Filtres Flash (Glow / DropShadow / Blur) | Porter [render/wgpu/src/filters/](third_party/ruffle/render/wgpu/src/filters/) au backend GL. Touche `render_offscreen` + `apply_filter` + cache_entries dans `submit_frame`. Premier essai 2026-05-24 a régressé les sprites (BitmapHandle Atlas vs Owned mal géré, source==destination UB). Revert clean. À refaire en portant fidèlement (cf. [[port-ruffle-dont-invent]]). Mario 63 visuel manque "brillures" sur logo et "bordures aux lettres". | Moyen-fort |
| 2.4 Bugs upstream Mario 63 | Issues Ruffle connues #13198 (text/audio open), #1909 (crash tutorial), #6906 (castle), #4690 (title freeze), #11077 (Bowser non-completable), #2448 (gradients). Beaucoup ont des fixes upstream à intégrer en bumpant le submodule. | Variable |
| 2.5 Performance | Mesurer FPS sur Tegra X1 docked/handheld. Optimiser : batcher les draws solides, cacher uniforms entre draws, réduire glUseProgram. | Faible |
| 2.6 Real file picker C++ | Scan `sdmc:/ruffle/*.swf` via libnx fsdev (contourne le bug `std::fs::read_dir` Horizon — filenames tronqués). UI joycon : liste avec A=select. | Faible |

### Phase 3 — polish + distribution

- Cycle applet (`appletGetFocusState`, suspend/resume, libération GPU au focus-lost)
- `.nacp` metadata final, icon
- Packaging hb-app.store
- Compat globale : Madness, Newgrounds classics, autres SWF AS1/AS2 populaires

### Verdict timeline solo (révisé 2026-05-24 fin journée)

- ~~Premier `.swf` qui affiche un truc : 6-10 semaines~~ → **fait en 6 jours**
- ~~Mario 63 jouable : +3-6 mois~~ → **input + chargement + SPRITES VISIBLES faits en 6 jours**
- Phase 2.1 sprites : ~~estimation 1-2 semaines~~ → **fait en ~5 h (le même jour que budget=0)** après débuggage methodique du crash silencieux jpeg_decoder
- Phase 2.2 audio : ~~estimation 2-3 jours~~ → **fait en ~3 h (même journée)** en portant CpalAudioBackend tel quel + libnx audren côté C++
- Phase 2.3 (bump submodule + bugs upstream + filtres Glow/DropShadow) : estimation **3-7 jours**
- Release publique propre : **+1-2 mois**

La sous-estimation initiale (6-12 mois) venait surtout de la peur autour de `std-via-newlib` (Phase 1.1) qui s'est résolue en 1h au lieu de 1-2 semaines, car upstream stdlib avait déjà les branches `target_os = "horizon"` pour le 3DS.

## Contraintes / faits à retenir

- **Mario 63 = AS2 pur interprété.** AVM2 JIT non nécessaire → service Horizon `jit:u` non requis. Confirmé en pratique : le SWF v8 charge et exécute sans JIT.
- **Ruffle deps à neutraliser :** ~~prévu pour `cpal`, `flate2 rust_backend`, `reqwest`~~ → **non nécessaire en pratique**. `cpal`/`reqwest`/`tokio`/`wgpu` ne sont PAS dans `ruffle_core`, juste dans `ruffle_desktop`. `flate2` workspace default = `miniz_oxide` (pure Rust). Tout linke direct.
- **FFI libnx utilisée** (Phase 1 + 2.2 complétées) :
  - HID : `padConfigureInput`/`padInitializeDefault`/`padUpdate`/`padGetButtonsDown/Up`/`padGetStickPos`/`hidInitializeTouchScreen`/`hidGetTouchScreenStates` (appelés direct dans `cpp/src/main.cpp`)
  - Applet : `appletMainLoop`, `nwindowGetDefault`
  - Socket : `socketInitializeDefault`, `nxlinkStdio` (stdout réseau)
  - FS : `sdmc:/...` monté auto par crt0 libnx → `std::fs::read` marche depuis Rust (sauf `read_dir` qui bug — voir Phase 2.5)
  - Thread : `threadCreate`/`threadStart`/`threadWaitForExit`/`threadClose` pour worker GL (main.cpp) et worker audio (audio.cpp)
  - System : `svcGetInfo` (RAM diagnostic via `ruffle_query_ram`), `armGetSystemTick` (pacing dt réel pour Ruffle)
  - **Audio (Phase 2.2)** : `audrenInitialize`/`audrenStartAudioRenderer`/`audrenWaitFrame`/`audrvCreate`/`audrvMemPoolAdd|Attach`/`audrvVoiceInit`/`audrvVoiceSetDestinationMix`/`audrvVoiceSetMixFactor`/`audrvVoiceAddWaveBuf`/`audrvVoiceIsPlaying`/`audrvVoiceStart`/`audrvVoiceStop`/`audrvUpdate`/`audrvClose`/`audrenExit`
- **FFI libnx à venir** (Phase 3) :
  - Applet : `appletGetCurrentFocusState`/`appletHook` pour cycle suspend/resume
- **Bindgen** : non utilisé en pratique. FFI écrite à la main dans `rust/src/ffi/gl.rs` (subset GL 4.3 core) et `cpp/src/ruffle_bridge.cpp`. Plus simple et zéro dep build-time.
- **Pattern d'architecture à copier :** ScummVM `backends/platform/sdl/switch/` — séparation OSystem → OSystem_SDL → OSystem_Switch. Adapté ici en Rust : trait `RenderBackend` (de Ruffle) + impl `SwitchRenderBackend` mince.

## Toolchain installée (état actuel)

- **devkitPro** dans `C:\devkitPro\` avec packages `switch-dev`, `switch-mesa`, `switch-glm`, `switch-glad`
- **Rust** : toolchain pin via `rust/rust-toolchain.toml` → `nightly-x86_64-pc-windows-gnu` + `rust-src` (host GNU obligatoire — MSVC casse les build scripts sans Visual Studio Build Tools)
- **MinGW-w64** via `scoop install mingw` (16.1.0) — pour `dlltool.exe` que Rust nightly GNU embarque buggé. Ajouter `~/scoop/apps/mingw/current/bin` au PATH avant `cargo build` (le `scripts/build.sh` le fait déjà).
- LLVM/CMake/Python : **toujours pas nécessaires** — confirmé en pratique. Phase 2.2 audren faite via FFI manuelle dans `cpp/src/audio.cpp`, pas bindgen.
- **`third_party/jpeg-decoder-switchfork/`** : fork patchée de `jpeg-decoder` 0.3.2 (single-line patch dans `select_worker` pour toujours retourner Immediate). Référencée via `[patch.crates-io]` dans `rust/Cargo.toml`. Sans elle, Mario 63 crashe silencieusement après ~3 sec parce que jpeg-decoder spawne `std::thread` sur newlib pour JPEGs > 128×128 px.

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
- nxlink pour stdout réseau (debug) + netloader (push `.nro` par WiFi)
- SD : copier le `.nro` dans `/switch/flash-for-switch.nro` pour le mode SD (non requis si netload)
- SWFs cherchés en priorité dans `sdmc:/ruffle/` (voir section « Tester sur Switch » plus haut)

## Références

- Ruffle : https://github.com/ruffle-rs/ruffle (`render/src/backend.rs` pour le trait à impl)
- aarch64-switch-rs : https://github.com/aarch64-switch-rs/{nx,cargo-nx}
- libnx doc : https://switchbrew.github.io/libnx/
- ScummVM Switch (pattern reference) : `backends/platform/sdl/switch/`
- Mario 63 source : https://github.com/runouw/Super-Mario-63
- GBAtemp Switch homebrew dev : https://gbatemp.net/forums/switch-homebrew-development.300/
