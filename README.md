# FlashNX

**FlashNX** — lecteur Flash homebrew pour Nintendo Switch (`.nro`). Cible : faire tourner **Super Mario 63** (AS2) et tout `.swf` AS1/AS2 depuis la SD card.

**Powered by [Ruffle](https://github.com/ruffle-rs/ruffle)** (Apache 2.0 / MIT) — FlashNX intègre le core `ruffle_core` (parsing SWF + interpréteur AVM1/AVM2) et lui branche tout un stack natif Switch : backend OpenGL custom (switch-mesa), backend audio audren, storage SD, library UI joycon-navigable, éditeur de touches in-game, mega-arena GL anti-fragmentation, exception handler libnx natif, fork patché de jpeg-decoder pour newlib. Les écarts de compatibilité AVM1/AS2/AVM2 viennent du moteur Ruffle upstream — voir [ruffle.rs/compatibility](https://ruffle.rs/compatibility) pour l'état (~99% du langage AVM1, ~75-81% des APIs).

> **À propos du nom** : le repo + la toolchain s'appellent encore `flash-for-switch` (paths, build scripts, Cargo, .nacp) pour éviter une bascule destructrice mid-projet. **FlashNX** est le nom user-facing — celui affiché dans l'UI, le `.nacp` final pour la release v1, et la marque de référence pour la doc / hb-app.store.

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
│   │   ├── exception.cpp         # __libnx_exception_handler natif (Phase 2.1.b)
│   │   ├── swf_picker.cpp        # scan SD via opendir/readdir (Phase 2.6, contourne bug read_dir Horizon)
│   │   └── ruffle_bridge.cpp     # ruffle_log_cstr + getrandom + sysconf stubs + svcGetInfo RAM
│   └── include/ruffle_bridge.h
├── rust/
│   ├── Cargo.toml                # crate-type = ["staticlib"], ruffle_core features=[audio,mp3] + jpeg-decoder patch
│   ├── rust-toolchain.toml       # nightly-x86_64-pc-windows-gnu + rust-src
│   ├── .cargo/config.toml        # target aarch64-nintendo-switch-freestanding + rustflags
│   └── src/
│       ├── lib.rs                # FFI exports + PlayerBuilder + SWF loader + input handlers + ruffle_set_swf_path (Phase 2.6) + TOUCHES FFI (Phase 3.3)
│       ├── keymap.rs             # JSON keymap (sidecar + default + fallback) + mutation API (Phase 3.3)
│       ├── menu.rs               # TOUCHES sub-screen state machine (list + dropdown) (Phase 3.3)
│       ├── ffi/gl.rs             # OpenGL FFI subset (no bindgen — hand-written)
│       └── backend/
│           ├── render.rs         # SwitchRenderBackend (~2000 lignes, 4 shaders, atlas, edge replication, UV wrap, GlStateCache Phase 2.5)
│           ├── audio.rs          # SwitchAudioBackend (port CpalAudioBackend → libnx audren)
│           ├── storage.rs        # SwitchStorageBackend port DiskStorageBackend → sdmc:/ruffle/saves/ (Phase 2.4.bis)
│           ├── tracing.rs        # Routes Ruffle's tracing events to nxlink stdout
│           └── log.rs            # SwitchLogBackend → ruffle_log_cstr
├── patches/
│   ├── README.md                 # Comment ré-appliquer après git submodule update
│   └── 0001-mario63-zero-scale-hit-test.patch  # Fix Toad château #6906 (Phase 2.4.a)
├── third_party/
│   ├── ruffle/                   # git submodule, master @ 71280cd1 (bump 2026-05-25, +42 commits dont AVM1/AVM2 fixes) + patches/*.patch appliqués
│   └── jpeg-decoder-switchfork/  # vendored jpeg-decoder-0.3.2 with select_worker → Immediate forced
├── assets/{icon.jpg, *.nacp}
└── scripts/{build.sh, setup-env.ps1, setup-env.sh}
```

Les backends Navigator/UI/Video utilisent les implémentations `Null*` que ruffle_core fournit par défaut — pas de fichier dédié. **Audio** = `SwitchAudioBackend` (Phase 2.2). **Storage** = `SwitchStorageBackend` (Phase 2.4.bis).

## Assets branding FlashNX ✓ livrés 2026-05-26

Deux logos en place dans `assets/` :

| Asset | Format | Dimensions | État | Usage |
|---|---|---|---|---|
| `assets/icon.jpg` | JPEG baseline, sRGB, no alpha | **256×256** px | ✓ livré | Icône `.nro` reprise automatiquement par hbmenu / Sphaira / forwarders home menu. "Flash / NX" stacked, typo orange-rouge gradient italique + accent éclair, fond gris clair avec motif éclairs subtil. |
| `assets/banner.png` | PNG RGBA | **720×144** px (ratio 5:1) | ✓ livré | Banner horizontal pour le top de la library UI, "FlashNX" en une ligne avec éclair stylisé entre "Flash" et "NX". Fond transparent → s'overlay propre sur le panel navy. À intégrer via `include_bytes!` + `image` crate decode + texture upload + render shader bitmap existant (~1-2 h, Phase 3.4 polish pack). |

**À faire au moment du coding Phase 3.4** : wire `icon.jpg` dans le Makefile pour qu'il finisse dans le `.nro` (probablement déjà géré par devkitPro template via `APP_ICON`), et l'asset pipeline Rust pour `banner.png`. Test secondaire à la production : check lisibilité de `icon.jpg` réduit à 100×100 px (taille hbmenu/Sphaira) — si bruit du fond éclair fatigue à cette échelle, simplifier le background plus tard.

## Build

```bash
./scripts/build.sh            # release : LTO=full, ~3 min, .nro 12.2 MB (officiel)
./scripts/build.sh --dev      # release-dev : LTO=thin + codegen-units=16, ~30 s rebuild, .nro légèrement plus gros
```

Le script orchestre :
1. `cargo build --release` (ou `--profile release-dev`) côté Rust (target `aarch64-nintendo-switch-freestanding`, std-via-newlib, build-std nightly) → `rust/target/.../libruffle_switch.a` (~13-14 MB avec features audio+mp3)
2. `make` côté C++ lancé **dans le bash MSYS2 de devkitPro** (pour que `switch_rules` résolve les paths correctement) → link contre `libruffle_switch.a` + libnx + libEGL/libGLESv2 → `cpp/flash-for-switch.nro` (~12.2 MB)

Le Makefile a `libruffle_switch.a` comme dépendance explicite du `.elf`, donc tout changement Rust déclenche le relink C++ automatiquement (plus besoin de `make clean` manuel après chaque modif Rust). Le profile `release-dev` est sélectionné via la variable d'env `RUST_PROFILE` que `build.sh --dev` exporte.

**État Mai 2026 (Phase 2.1 + 2.2 + 2.1.b + 2.4.a + 2.4.bis + 2.5 + 2.6 ✓)** : full std via les 2 patches stdlib (voir plus bas), `ruffle_core` linké avec features `audio` + `mp3`, **Mario 63 jouable longue durée avec jetpack + audio + sprites complets** (atlas 2048×2048, edge replication, UV wrap shader, audren via SwitchAudioBackend qui porte le pattern CpalAudioBackend, mega-buffer arena GL pour éviter la saturation handles Mesa, libnx `__libnx_exception_handler` natif pour diagnose tout crash). Sauvegardes persistées via `SwitchStorageBackend` (Phase 2.4.bis), fix Toad château #6906 (Phase 2.4.a) appliqué en submodule + dump dans `patches/`, file picker libnx fsdev (Phase 2.6, accepte n'importe quel filename), GL state cache (Phase 2.5, élimine ~80% des glUseProgram/glBindTexture/glBindVertexArray redondants par frame).

## Tester sur Switch

1. Copier ton `.swf` sur la SD dans **`sdmc:/ruffle/`** (ou `sdmc:/switch/ruffle/`). N'importe quel nom de fichier marche depuis Phase 2.6 — le file picker C++ scan le dossier via libnx fsdev et prend le premier `.swf` trouvé. Si plusieurs SWFs sont présents, l'ordre est déterminé par `readdir` (généralement ordre d'écriture). UI de sélection joycon viendra en Phase 3.4.
   - Liste de fallback (utilisée si le scan ne trouve rien) : `sdmc:/ruffle/test.swf`, `sdmc:/ruffle/mario.swf`, `sdmc:/ruffle/Super_Mario_63_2010.swf`, `sdmc:/switch/ruffle/test.swf`
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
| R | Enter (« Press Start ») |
| L | Escape |
| Plus | P (touche pause standard) |
| **Minus** | **Ouvre le menu pause** (REPRENDRE / TOUCHES / REDEMARRER / QUITTER) |

Dans le menu pause : D-pad/stick haut-bas pour naviguer, **A** valide, **B** ou **Minus** referme sans rien faire. « QUITTER » sort vers le Homebrew Menu, « REDEMARRER » recharge le SWF depuis zéro (conserve les sauvegardes `.sol`), « TOUCHES » ouvre l'éditeur de keymap (voir ci-dessous).

## Customisation des touches

Deux moyens, le second est de loin le plus simple :

### Éditeur in-game « TOUCHES » (recommandé)

Depuis le menu pause (Minus), sélectionne **TOUCHES** + A. Tu vois la liste des boutons Switch avec leur binding actuel entre `[ brackets ]`. Navigue avec haut/bas, **A** sur une ligne ouvre un dropdown listant toutes les touches Flash possibles (`Space`, `Z`, `X`, `Shift`, `Enter`, `Escape`, `P`, flèches, ou `(aucune)` pour unbind). **A** confirme, **B** annule.

À chaque confirmation, le sidecar JSON est sauvé sur SD ET le binding s'applique immédiatement en jeu (pas besoin de REDEMARRER pour tester). **B** ou **Minus** depuis l'éditeur revient au menu pause.

Le sidecar écrit est `sdmc:/ruffle/<basename>.keymap.json` — par jeu, sans toucher au default global.

### Édition JSON manuelle (power users)

Si tu préfères tout faire au clavier depuis ton PC, le `.nro` lit / écrit des JSON sur la SD.

**Hiérarchie de lookup** (premier hit gagne) :
1. `sdmc:/ruffle/<basename>.keymap.json` — override par jeu (ex. `sdmc:/ruffle/Super_Mario_63_2010.swf.keymap.json`)
2. `sdmc:/ruffle/keymap_default.json` — default global choisi par toi
3. Fallback hardcodé dans le `.nro` — la table ci-dessus

Au premier boot, si `keymap_default.json` n'existe pas, le `.nro` l'écrit avec le fallback hardcodé — ouvre-le dans Notepad pour voir le schema et adapter.

**Schema** (exemple) :
```json
{
  "version": 1,
  "bindings": {
    "A": "Space",
    "B": "Z",
    "X": "X",
    "Y": "Shift",
    "L": "Escape",
    "R": "Enter",
    "Plus": "P",
    "Up": "Up", "Down": "Down", "Left": "Left", "Right": "Right",
    "StickLUp": "Up", "StickLDown": "Down", "StickLLeft": "Left", "StickLRight": "Right"
  }
}
```

**Noms de boutons Switch supportés** : `A`, `B`, `X`, `Y`, `L`, `R`, `ZL`, `Plus`, `Up`/`Down`/`Left`/`Right` (D-pad), `StickLUp`/`StickLDown`/`StickLLeft`/`StickLRight` (stick gauche directionnel). Boutons absents = unbound. `Minus` est réservé pour le menu pause et ne peut pas être remappé.

**Noms de touches Flash supportées** : `Space`, `Enter`, `Escape`, `Up`/`Down`/`Left`/`Right`, `Z`, `X`, `Shift`, `P`. D'autres lettres/touches à demander.

**Vérification** : à chaque boot le `.nro` logue via nxlink la résolution finale (`keymap: resolved 15 bindings: A=1 B=8 ...`) — utile pour confirmer que ton fichier est bien pris en compte.

**Exemple use case** : un jeu utilise Q/W/E au lieu de Z/X/Shift → tu crées un sidecar `sdmc:/ruffle/MonJeu.swf.keymap.json` avec ces bindings spécifiques sans toucher au default global qui reste sur Mario 63 (ou plus simple : tu fais le remap depuis l'éditeur in-game TOUCHES, même résultat).

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

### Phase 2 — finir Mario 63 (sprites ✓ + son ✓ + Toad château ✓ + sauvegardes ✓ + file picker ✓ + perf ✓)

À ce stade Mario 63 charge, l'AS2 exécute, l'input répond, le premier niveau se joue **avec sprites visibles + son audible (musique + SFX) + sauvegardes `.sol` persistées + Toad château présent + GL state cache + n'importe quel SWF auto-pris depuis `sdmc:/ruffle/`**. Il reste :

| Étape | Boulot | Risque |
|---|---|---|
| 2.1 ✓ Sprites visibles (validé hardware 2026-05-24) | ~4 h debug + ~1 h fix | **Résolu**. Voir 1.5.e + 1.5.e.bis ci-dessus. Aussi un bug d'UV wrap (Mario apparaissait sur le sol) corrigé en pousser `fract/clamp` dans le fragment shader avant le remap atlas. Et un bug d'edge bleed (lignes noires entre sprites avec LINEAR filtering) corrigé en répliquant les pixels du bord dans le pad atlas. |
| 2.2 ✓ Audio audren (validé hardware 2026-05-24 fin journée) | ~3 h | **Résolu**. [cpp/src/audio.cpp](cpp/src/audio.cpp) wrappe `audrenInitialize`/`audrvCreate`/`audrvVoiceInit`/`audrvVoiceAddWaveBuf` + worker thread libnx (NUM_WAVE_BUFS=4, 4096 frames each, ~340 ms cushion). Côté Rust, [rust/src/backend/audio.rs](rust/src/backend/audio.rs) = port de `frontend-utils/CpalAudioBackend`: wraps `ruffle_core::AudioMixer` + `impl_audio_mixer_backend!` macro, expose `proxy` via `OnceLock<Mutex<>>` que le C++ pull via `ruffle_audio_fill_buffer`. Features `ruffle_core = ["audio", "mp3"]` (Mario 63 utilise MP3 pour TOUT son audio incl. SFX, +250 KB symphonia mais indispensable). `mixer.set_volume(0.5)` pour éviter clipping (sans, `max_seen=32767` constant → grésillements audibles ; avec, `max_seen=6009` propre). |
| 2.1.b ✓ Mega-buffer arena + libnx exception handler (validé hardware 2026-05-25 nuit) | ~3 h diag + ~2 h refactor | **Résolu**. Bug : Mario 63 + rocket-nozzle FLUDD particle system émet ~3 shapes/frame, accumulés sans relâche par Ruffle. À ~27 000 GpuDraws live (= ~83 000 VBO/IBO/VAO handles GL côté Mesa-NVK Switch), `glBindBuffer` segfault dans une table interne saturée (DataAbort, `x24=GL_ARRAY_BUFFER`, FAR=index 0x1011). Le crash bypassait le `panic_hook` Rust car c'est une faute native. **Fixes appliqués :** (a) [cpp/src/exception.cpp](cpp/src/exception.cpp) — `__libnx_exception_handler` weak-override (32 KB dedicated stack) qui dump PC/LR/SP/FAR/ESR + x0–x28 vers nxlink + `sdmc:/switch/ruffle-crash.log`. (b) [cpp/src/main.cpp](cpp/src/main.cpp) — boot-replay du `ruffle-crash.log` au lancement suivant. (c) [rust/src/backend/render.rs](rust/src/backend/render.rs) — `BufferArena` (1 mega-VBO 64 MB + 1 mega-IBO 32 MB, freelist coalesçant), `PENDING_FREES` queue drainée au top de `submit_frame`, `GpuDraw` reshape en `{vbo_offset, vbo_size, ibo_offset, ibo_size, num_indices, kind}`, single global `shape_vao` configuré une fois au boot, render path utilise `glDrawElementsBaseVertex`. **Gotcha critique :** l'alignement arena VBO doit égaler le vertex stride (24 bytes), pas une puissance de 2 (16 cassait base_vertex). Round-up générique `((x+a-1)/a)*a` au lieu de `& !(a-1)`. **Résultat hardware :** 18 720 frames / 1.2 M bitmap_draws / 30 502 live draws au test, exit propre via Plus. Phase 2.4 = bugs Ruffle upstream (Toad NPC manquant dans château, "non-registered character" errors) prend le relais. |
| 2.3 ⏸️ Filtres Flash (Glow / DropShadow / Blur / ColorMatrix) — **PARKED** | Tenté 2026-05-25 nuit (2e fois). Port fidèle des 4 shaders WGSL→GLSL + infra FBO + texture pool + dispatcher + boucle cache_entries dans submit_frame. ~5 h de code, 1624 lignes, compile clean release. Mais sur hardware Mario 63 : (1) **boutons menus invisibles** parce que Mario 63 utilise Bevel (pas implémenté) et notre chain logic break sur filtre None → sprite skip entier ; (2) **fps de 30 → 5.5** à cause des `glGenTextures`/`glDeleteTextures` à chaque cache_entry × frame ; (3) **text bizarre** non diagnostiqué. Code sauvé sous [temp/phase-2.3-filters-wip/](temp/phase-2.3-filters-wip/) (patch + README détaillé avec stratégie de fix). Ré-application : `git apply temp/phase-2.3-filters-wip/phase-2.3.patch`. À reprendre quand on aura un créneau plusieurs heures focused pour les 3 fixes ciblés (chain passthrough sur unsupported + pool dans cache_entries + diag text). | Moyen-fort |
| 2.4 ✓ Bump submodule Ruffle | **Validé 2026-05-25 nuit**. Submodule passé de `e41992ab` (2026-05-20) à `71280cd1` (2026-05-25), +42 commits dont fixes notables : `core: Fix looping for movie clips without End tag`, `core: Fix looping for one-frame movies`, `core: Normalize "blank" target to "_blank"`, `avm1: Do not trace an error on with(undefined)/with(null)`, AVM2 stack trace improvements. Patch Toad 2.4.a ré-appliqué proprement après bump. Issues Mario 63 spécifiques (#13198/#1909/#4690/#11077/#2448) toujours non fixées upstream — restent à diagnostiquer en jeu (peut-être que certaines sont impactées par les fixes de looping). | Faible |
| 2.4.a ✓ Toad manquant dans le château (#6906) | **Validé 2026-05-25 nuit**. Fix appliqué dans [third_party/ruffle/core/src/display_object.rs:2587](third_party/ruffle/core/src/display_object.rs#L2587) + 2614 (`hit_test_bounds` + `hit_test_shape` default impl) : return `false` quand `self.local_to_global_matrix().determinant().abs() <= f32::EPSILON`. Parity Adobe Flash Player. Patch dumpé dans [patches/0001-mario63-zero-scale-hit-test.patch](patches/0001-mario63-zero-scale-hit-test.patch) pour survivre à un futur bump du submodule. **TODO** : PR upstream Ruffle (+ tests SWF référence) — bénéficie à tout l'écosystème. | Faible |
| 2.4.bis ✓ SharedObject / URL invalide / fs::read bug | **Validé 2026-05-25 soir (hardware end-to-end, Mario 63 affiche "Continuer")**. Trois fixes empilés : (a) **URL fix** dans [rust/src/lib.rs](rust/src/lib.rs) : `file://sdmc:/...` → `http://flashforswitch.local/<basename>` (le parser URL Ruffle rejetait l'IDN "sdmc"). (b) **`SwitchStorageBackend`** dans [rust/src/backend/storage.rs](rust/src/backend/storage.rs) — port direct du `DiskStorageBackend` upstream, pointe sur **`sdmc:/ruffle/saves/`** (à côté du jeu, plus simple à backup/restore depuis Windows). Layout final : `sdmc:/ruffle/saves/<host>/<swf_path>/<savename>.sol`. (c) **Chunked 4 KB read** — debug en 3 itérations sur hardware a révélé que `std::fs::read` ET `read_to_end` retournent `OutOfMemory` sur Switch alors que le fichier existe ; root cause : le syscall `read()` newlib retourne `ENOMEM` quand appelé avec un buffer ≥ 32 KB (defaut de `read_to_end`). Workaround : lire par chunks de 4 KB dans un buffer fixe (voir [[reference-horizon-fs-quirks]]). **Bonus** : compat AMF Adobe → glisser ses `.sol` Windows sur la SD marche aussi. | Faible |
| 2.5 ✓ Performance — GL state cache + FPS heartbeat + CpuBoostMode | **Validé 2026-05-25 soir**. Trois fixes empilés : (a) `GlStateCache` ([rust/src/backend/render.rs](rust/src/backend/render.rs)) cache `last_program` / `last_texture` / `last_wrap_mode` / `last_vao` via `Cell<>` — court-circuite les `glUseProgram`/`glBindTexture`/`glBindVertexArray` redondants ; sampler `u_tex` set une fois au link. (b) **FPS heartbeat avec tick=Xms render=Yms** dans submit_frame — accumule via [rust/src/lib.rs](rust/src/lib.rs) `TICK_TICKS_ACCUM`/`RENDER_TICKS_ACCUM`, log toutes les 60 frames. Profile sur hardware a révélé que **le bottleneck est l'AVM1 interpréteur, pas notre backend GL** (tick=50ms/frame vs render=5ms/frame en scène lourde). (c) **`appletSetCpuBoostMode(ApmCpuBoostMode_FastLoad)`** dans [cpp/src/main.cpp](cpp/src/main.cpp) — bascule l'enveloppe power Tegra X1 vers le CPU au prix du GPU (qu'on n'utilise qu'à 30%). **Résultat hardware** : scène lourde 17.8 fps → 30.9 fps (+73%), tick par frame 50 ms → 27.8 ms (-44%). Parité avec PC. Pas un overclock — clocks Nintendo stock. Thread prio 0x20 testé sans gain (Switch pas loaded en autres threads), reverté à 0x2C default. | Faible |
| 2.6 ✓ File picker C++ libnx | **Validé 2026-05-25 nuit**. [cpp/src/swf_picker.cpp](cpp/src/swf_picker.cpp) — `opendir`/`readdir` côté C (newlib bypasse le bug Rust `std::fs::read_dir`). Scan dans `sdmc:/ruffle/` puis `sdmc:/switch/ruffle/` ; premier `.swf` trouvé → push à Rust via nouvelle FFI `ruffle_set_swf_path`. Rust override la candidates list. Plus besoin de renommer son SWF pour matcher un nom hardcodé. **UI sélection** (liste joycon-navigable) = Phase 3.4 (library UI ScummVM-style). | Faible |

### Phase 3 — Plateforme Flash games (ScummVM-style)

**Vision** : transformer le `.nro` "Mario 63 player" en **plateforme générique pour SWF AS1/AS2** sur Switch, avec UI propre type ScummVM/Dolphin port. C'est ce qui distingue un POC d'un vrai logiciel de référence pour la préservation Flash sur Switch.

**Contexte qui justifie ça** (avec honnêteté sur les trade-offs) :
- Le `.exe` Flash projector standalone Adobe (v32.0.0.363, avril 2020 — dernière version possible, Adobe a tué Flash en décembre 2020) est **l'environnement de référence sur lequel Mario 63 a été développé et finalisé**. À ce titre il joue le jeu à 100% sans bug : c'est la cible originale, par construction.
- Ruffle est une **clean-room reimplementation** de Flash Player en Rust, faite par des bénévoles. Selon [ruffle.rs/compatibility](https://ruffle.rs/compatibility) : 99% du langage AVM1/AS2 supporté, **mais seulement 75-81% des APIs et 77% des properties**. Donc des écarts visibles sur les SWF complexes — c'est notre cas (Toad manquant Mario 63, `non-registered character` errors). Ces écarts se réduisent à chaque release nightly, sans probablement jamais atteindre 100% parity. **Ruffle n'est PAS plus fiable que le `.exe` Adobe.**
- L'argument pour Ruffle est ailleurs : **(a) portabilité** — Ruffle tourne sur Switch ARM, Web, embedded ; le `.exe` Adobe ne tourne que sur Win/Mac/Linux x86 ; **(b) maintenance** — Ruffle évolue, le `.exe` est définitivement figé.
- Pour un utilisateur Windows qui veut juste jouer Mario 63 sur PC, **le `.exe` reste la meilleure option qualité visuelle/fidélité**. Notre projet n'a pas vocation à le remplacer là-dessus.
- Notre projet a vocation à **être la seule voie possible vers Flash portable Switch** — d'où l'intérêt de viser une vraie UX (plateforme ScummVM-style) et pas juste "ça boot Mario 63". Le compromis qualité (écarts Ruffle) est accepté en échange de la portabilité.

| Étape | Boulot | Risque |
|---|---|---|
| 3.1 ✓ Cycle applet | **Validé empiriquement 2026-05-25 nuit** : home-button + mise en veille Switch fonctionnent déjà sans crash sur Mario 63 long-play. `appletMainLoop()` de libnx gère implicitement le pause/resume du worker thread — pas besoin de hooks explicites (`appletGetCurrentFocusState`/`appletHook`) tant qu'on ne fait rien d'exotique côté GPU pendant le focus-lost. Si un cas pathologique apparaît (e.g. crash après suspend très long), on pourra ajouter les hooks à ce moment-là. | Faible / aucun |
| 3.2 ✓ `SwitchStorageBackend` (.sol persistés) | **Fait via 2.4.bis** ([rust/src/backend/storage.rs](rust/src/backend/storage.rs)). Sauvegardes `.sol` persistées dans `sdmc:/ruffle/saves/`, import depuis AppData Windows / Ruffle desktop fonctionnel (format AMF cross-platform). | Faible |
| 3.3 ✓ Menu pause in-game + éditeur TOUCHES | **Livré 2026-05-25 nuit → 2026-05-26 nuit**. Modal custom GL natif via `SwitchRenderBackend::draw_menu_overlay` (font 5×7 pixel-art hand-encodée `GLYPHS`, backdrop semi-transparent, sélection ambre). 4 entrées : **REPRENDRE / TOUCHES / REDEMARRER / QUITTER**. REDEMARRER = `ruffle_restart()` drop+rebuild Player (cache SWF static contre OOM heap). Plus = touche `P`. **Éditeur TOUCHES** (2e modal) : liste scrollable des boutons Switch avec binding actuel, A ouvre un dropdown des touches Flash (Space/Z/X/Shift/Enter/Escape/P/flèches/(aucune)), A confirme → save sidecar `sdmc:/ruffle/<basename>.keymap.json` + live reload des BINDINGS via `consume_dirty` flag (pas besoin de REDEMARRER pour tester). State machine TOUCHES vit en Rust ([rust/src/menu.rs](rust/src/menu.rs)), C++ forward juste les down-edges via `ruffle_touches_input(name)`. **Reste à ajouter** (dépendent d'autres phases) : Save state / Load state (Phase 3.5) et Back to library (Phase 3.4). | ✓ |
| 3.4 Library UI (v1 minimaliste, "FlashNX launcher") | **Design arrêté 2026-05-26 nuit** après discussion + comparaison avec ScummVM launcher ([docs](https://docs.scummvm.org/en/latest/use_scummvm/the_launcher.html)). Boot du `.nro` → scan `sdmc:/ruffle/*.swf` + `sdmc:/switch/ruffle/*.swf` via libnx `fsFsOpenDirectory` (contourne bug `std::fs::read_dir` Horizon), parse header SWF light pour version + dims, **toujours** affiche la library (même avec 1 SWF, cohérence > économie d'1 clic). **Layout** : liste textuelle scrollable réutilisant l'infra TOUCHES list (pixel font + scrollbar), curseur navigue, metadata du jeu sélectionné affichée en bas (`15 MB · SWF V8 · 450x300`). **Inputs** : **A**=JOUER, **X**=OPTIONS (modal par jeu), **-**=QUITTER `.nro`. **Empty state** propre si SD vide (instructions où poser les `.swf`). **Modal OPTIONS** = pour l'instant juste 1 entrée TOUCHES (réutilise `menu::open()` existant → user peut configurer les touches AVANT de lancer le jeu, plus juste in-game) + RETOUR. Préparé architecturalement pour devenir un dialog tab-bar style ScummVM quand on aura plusieurs catégories (audio volume, scaling, etc.) — pas le cas au v1. **Hors scope v1 (Phase 3.4.bis)** : Renommer (**display-name only** via sidecar `.meta.json` `{"display_name": "..."}` + libnx `swkbd` — **on NE renomme PAS le fichier `.swf` ni rien d'autre**. Le basename physique reste, donc `.sol`/`.keymap.json`/URLs Ruffle/SharedObject paths inchangés → zéro migration en cascade, zéro risque de saves perdues. Pattern Steam/ScummVM/iTunes. La library UI affiche le display_name, le metadata panel montre toujours `[basename.swf]` en petit dessous pour que l'user voie quel fichier physique est concerné — pas de surprise). Supprimer (avec confirm, préserve `.sol` saves comme ScummVM), tri options, jaquettes (skip décidé — user qui veut une jaquette sur home menu génère un forwarder Sphaira avec icône custom, cf. notes ban sysNAND ci-dessous), grid view, search. **Pas de "Back to library"** depuis le pause menu au v1 (QUITTER exit le `.nro`). Remplace définitivement Phase 2.6 (file picker hardcodé). | Moyen, ~2 j v1 |
| 3.4.bis Forwarders home menu (doc-only, pas de code) | Pour avoir l'icône FlashNX (ou jaquette custom Mario 63) sur le **home menu Switch** à côté des vrais jeux : l'user passe par [Sphaira](https://github.com/ITotalJustice/sphaira) → "Create forwarder" → notre `.nro` → Sphaira lui demande icône custom + nom + génère le NSP. **Pas de code chez nous** — Sphaira fait tout. Doc dans README avec ⚠️ **warning ban sysNAND** explicite (les forwarders ressemblent à des jeux piratés pour Nintendo, safe seulement sur emuNAND). | Doc 30 min |
| 3.4 polish "vie" (pack léger) | Après le v1 fonctionnel, **pass de polish visuel low-cost** pour que la library ne soit pas terne : (1) **drop shadow sur le titre uniquement** (~120 draw_rect doublés, depth instant), (2) **curseur animé** `►` qui pulse couleur via `sin(time)` (~0 coût), (3) **pulsing ligne sélectionnée** modulation sin sur la couleur ambre des pixels déjà dessinés (~0 coût), (4) **banner logo FlashNX en PNG bitmap** (~720×144 px) bundlé via `include_bytes!` + décodé via `image` crate au boot + texture upload + render via shader `bitmap_prog` existant — **1 quad textured par frame au lieu de 120 draw_rect du pixel font ASCII** = gain perf ET visuel énorme (typo custom, anti-alias, gradient). Ouvre la porte à d'autres assets bitmap (splash, fond library) sans surcoût marginal. **+1-2 h pour l'asset pipeline** (décode PNG une fois au boot, garder texture vivante, alpha blending). (5) **color chip par jeu** carré 16×16 px à gauche de chaque ligne, couleur dérivée d'un hash du basename — chaque jeu a sa signature visuelle unique sans config (1 draw_rect par row, gratuit visuellement). **Skip explicite** : drop shadow sur toutes les lignes (~3000 draw_rect / frame, ROI faible), ASCII art multi-ligne (trop lourd), background gradient animé. **Note perf** : menu library est paused (Ruffle ne tick pas), donc même à ~15-30 fps c'est fluide à naviguer. Si on ressent vraiment de la latence, **batching text rendering** (~1-2h optim : tous les quads d'une string dans un seul `glBufferData`+`glDrawArrays`, ×10 moins de GL calls) est l'escape hatch — à garder en tête, pas urgent. | Faible, ~3-4 h (avec asset pipeline) |
| 3.5 ✗ Savestate — **décidé skip 2026-05-26 nuit** | Discussion + arbitrage avec user : pas d'entrée Save / Load dans le menu pause. **3 raisons** : (1) Ruffle n'a PAS de `Player::serialize()` natif (verified 2026-05-25, 0 résultat grep + 0 GitHub issue). Le Player tient un `gc_arena::Gc<>` graph non sérialisable trivialement (display list + AVM1 stack + scope + timers + audio positions). Vrai savestate = 2-3 semaines upstream Ruffle (Phase 4 optionnelle). (2) Le compromis "savestate light" (SharedObject + frame + `_root.foo` capture + injection au reload) **n'ajoute pas de valeur** : pour Mario 63 le `.sol` capture déjà tout ce que le jeu sait restaurer (checkpoints, étoiles), et frame + `_root.*` ne suffit pas à reconstruire un mid-jump/mid-cutscene — donc "Save" ≈ ce que le `.sol` fait déjà, "Load" ≈ REDEMARRER qu'on a déjà. (3) Variante slots multiples (`.sol` dupliqués en `.slot1.sol` etc.) — granularité reste celle du jeu (Mario 63 = points de save fixes), ROI faible. **Conclusion** : skip dans le scope Phase 3, attendre vrai savestate upstream Ruffle (Phase 4 optionnel) si la demande remonte. Le menu pause reste à 4 entrées (Reprendre / Touches / Redemarrer / Quitter). | (skip) |
| 3.6 Compat globale autres SWF AS1/AS2 | Tester Madness, Newgrounds classics (Alien Hominid, Castle Crashers prototype, etc.). Probable que chaque jeu exposera ses propres bugs Ruffle. Reuse de la diag infrastructure (exception handler natif, crash log, compteurs live arena). | Variable |

### Phase 4 — polish + distribution

- `.nacp` metadata final propre, icône custom
- Packaging hb-app.store officiel
- Documentation utilisateur (comment importer ses saves, mappings touches, troubleshooting)
- **Savestate "vrai" (upstream contribution)** : ajouter `Player::serialize_state()`/`deserialize_state()` à ruffle_core (traverser le GC graph, sérialiser tout). Gros chantier ~2 semaines en collaboration mainteneurs Ruffle. Optionnel, bénéficie à tous les frontends Ruffle.

### Verdict timeline solo (révisé 2026-05-25 nuit après Phase 2.4.a + 2.4.bis + 2.5 + 2.6)

- ~~Premier `.swf` qui affiche un truc : 6-10 semaines~~ → **fait en 6 jours**
- ~~Mario 63 jouable : +3-6 mois~~ → **input + chargement + SPRITES VISIBLES faits en 6 jours**
- Phase 2.1 sprites : ~~estimation 1-2 semaines~~ → **fait en ~5 h** après débuggage méthodique du crash silencieux jpeg_decoder
- Phase 2.2 audio : ~~estimation 2-3 jours~~ → **fait en ~3 h** en portant CpalAudioBackend tel quel + libnx audren côté C++
- Phase 2.1.b mega-arena (crash jetpack) : ~~bug bloquant Mario 63 long-play~~ → **fait en ~5 h** (3 h diag + 2 h refactor) en installant `__libnx_exception_handler` natif pour capturer le DataAbort Mesa
- Phase 2.4.a Toad château (#6906) : ~~estimation 30 min~~ → **fait en ~30 min** ; patch ~12 lignes net, dumpé dans `patches/` pour survivre futurs bumps submodule
- Phase 2.4.bis URL fix + StorageBackend : ~~estimation 1 h~~ → **fait en ~30 min** (URL fix `http://flashforswitch.local/...` + port direct `DiskStorageBackend`)
- Phase 2.5 GL state cache + FPS heartbeat + CpuBoostMode : ~~estimation 2-3 j (batching)~~ → **fait en ~3 h** (cache via `Cell<>` + sampler `u_tex` set au link + tick/render profiling + CpuBoostMode FastLoad). Le profiling a révélé que le bottleneck n'était PAS notre backend mais l'AVM1 interpréteur Ruffle. Le batching `glMultiDrawElementsBaseVertex` aurait gagné ~+5 fps marginaux ; le CpuBoostMode a gagné **+13 fps en scène lourde**. ROI sans commune mesure.
- Phase 2.6 file picker libnx fsdev : ~~estimation 1 j~~ → **fait en ~45 min** (newlib opendir/readdir bypasse le bug Rust read_dir Horizon)
- Phase 2.3 filtres Glow/DropShadow : **PARKED 2026-05-25 nuit** (2e tentative). Le code est écrit, compile clean, port fidèle des 4 shaders + infra FBO + dispatcher. Mais 3 bugs hardware (boutons invisibles via chain-break sur filtre non supporté, fps 30→5 à cause alloc GL textures par frame, text bizarre). Sauvé dans `temp/phase-2.3-filters-wip/`. Estimation pour reprendre : **1 jour focused** (les fixes sont localisés et identifiés).
- Phase 2.4 bump submodule : **fait en ~5 min** après désac temporaire Avast HTTPS scan. Submodule passé de `e41992ab` à `71280cd1` (+42 commits). Patch Toad 2.4.a ré-appliqué proprement via `git apply ../../patches/*.patch`. Rebuild Rust complet 6 min, .nro final 12.25 MB.
- **Phase 3.1 cycle applet** : ~~estimation ½ j~~ → **0 jour, déjà OK** (validé empiriquement 2026-05-25 nuit, home-button + mise en veille marchent sans crash via `appletMainLoop` libnx implicit)
- **Phase 3.3 menu pause + éditeur TOUCHES** : ~~estimation 2-3 j (avec FreeType + libnx pl)~~ → **fait en ~5 h total sur 2 nuits** (25 nuit modal + REDEMARRER, 26 nuit éditeur TOUCHES live + keymap JSON system). Font pixel-art 5×7 hand-encodée suffisante (pas besoin FreeType). Save state / Load state / Back to library = dépendent de 3.5 / 3.4 respectivement, pas vraiment du scope 3.3.
- **Phase 3.5 savestate** : ~~estimation 1 j (light)~~ → **skip décidé 2026-05-26 nuit** (cf. ligne 3.5 du tableau Phase 3 ci-dessus). Pas de Save/Load dans le menu — light savestate = pas mieux que ce que le `.sol` + REDEMARRER font déjà, vrai savestate = chantier upstream Ruffle 2-3 semaines (Phase 4 optionnelle, attendre demande).
- **Phase 3 restant (library uniquement)** : estimation **2-3 jours** (3.4 library scan SD multi-SWFs + UI joycon). Tout le reste Phase 3 = ✓ ou ✗-skip-justifié.
- Release publique propre v1 (Phase 4) : **+2-3 semaines**
- Savestate "vrai" (upstream contribution Ruffle) : **+2 semaines** optionnel post-v1

La sous-estimation initiale (6-12 mois) venait surtout de la peur autour de `std-via-newlib` (Phase 1.1) qui s'est résolue en 1h au lieu de 1-2 semaines, car upstream stdlib avait déjà les branches `target_os = "horizon"` pour le 3DS. La même surprise s'est répétée Phase 2.4.bis : on imaginait un effort `StorageBackend` complexe, en fait l'API Ruffle est minimale (3 méthodes) et un `DiskStorageBackend` est directement réutilisable.

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
