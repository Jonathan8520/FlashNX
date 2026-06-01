# FlashNX — guide technique

**FlashNX** — lecteur Flash homebrew pour Nintendo Switch (`.nro`). Fait tourner tout `.swf` AS1/AS2 (et une partie de l'AS3) depuis la carte SD.

**Powered by [Ruffle](https://github.com/ruffle-rs/ruffle)** (Apache-2.0 / MIT) — FlashNX intègre le core `ruffle_core` (parsing SWF + interpréteur AVM1/AVM2) et lui branche un stack natif Switch : backend OpenGL custom (switch-mesa), backend audio audren, storage SD, library UI joycon-navigable, éditeur de touches in-game, mega-arena GL anti-fragmentation, exception handler libnx natif, fork patché de jpeg-decoder pour newlib. Les écarts de compatibilité AVM1/AS2/AVM2 viennent du moteur Ruffle upstream — voir [ruffle.rs/compatibility](https://ruffle.rs/compatibility) (~99 % du langage AVM1, ~75-81 % des APIs).

> **À propos du nom** : le repo, la toolchain, les scripts de build et le crate Cargo gardent le nom historique `flash-for-switch` (pour éviter une bascule destructrice). **FlashNX** est le nom user-facing — l'UI, le `.nacp`, le `.nro` et le dossier SD (`sdmc:/flashnx/`).

> 🕘 Le journal de développement phase-par-phase (Phases 0 → 4, dates, estimations) vit désormais dans l'historique `git log` et le [CHANGELOG](CHANGELOG.md). Ce document est un **guide de référence** pour builder, tester et contribuer — pas un journal.

## Décision d'architecture

**Option retenue : switch-mesa (OpenGL)**, plutôt que dawn-switch (WebGPU) :
- switch-mesa est mature (`dkp-pacman -S switch-mesa`), utilisé en prod par ScummVM, PPSSPP, RetroArch.
- dawn-switch est un POC 1-commit, dépend de NVK Switch non sourcé publiquement.
- Le backend GL de wgpu *« only seems to work under a Mesa context »* — switch-mesa **est** un contexte Mesa.

Stratégie **hybride C++/Rust** (Ruffle nécessite `std`, pas `no_std` → newlib via devkitPro) :

```
cpp/ (devkitPro)  →  rust staticlib (Ruffle + backends)  →  switch-mesa GL  →  GPU Tegra X1
```

## Structure projet

```
flash-for-switch/
├── cpp/
│   ├── Makefile                  # template devkitPro switch + APP_TITLE/AUTHOR/VERSION + --icon/--nacp
│   ├── src/
│   │   ├── main.cpp              # libnx init + worker thread + applet loop + joycon/touch input
│   │   ├── gl_context.cpp        # EGL/GL via switch-mesa, EGL_STENCIL_SIZE=8
│   │   ├── input.cpp             # (vide — placeholder, l'input est dans main.cpp)
│   │   ├── audio.cpp             # libnx audren wrapper + worker thread
│   │   ├── exception.cpp         # __libnx_exception_handler natif (crash log symbolisable)
│   │   ├── swf_picker.cpp        # scan SD via opendir/readdir (contourne le bug read_dir Horizon)
│   │   ├── net.cpp               # swkbd (URL + rename) + helpers import distant
│   │   └── ruffle_bridge.cpp     # ruffle_log_cstr + getrandom + sysconf stubs + svcGetInfo RAM
│   └── include/ruffle_bridge.h
├── rust/
│   ├── Cargo.toml                # crate-type = ["staticlib"], ruffle_core features=[audio,mp3,default_font] + jpeg-decoder patch
│   ├── rust-toolchain.toml       # nightly-x86_64-pc-windows-gnu + rust-src
│   ├── .cargo/config.toml        # target aarch64-nintendo-switch-freestanding + rustflags
│   └── src/
│       ├── lib.rs                # FFI exports + PlayerBuilder + SWF loader + input handlers + tick/render profiling
│       ├── library.rs            # Library UI state + banner/icon embed + SWF header parse + meta/keymap sidecars
│       ├── net.rs                # Import distant archive.org : fetch JSON + download async curl multi
│       ├── keymap.rs             # JSON keymap (sidecar + default + fallback) + mutation API
│       ├── menu.rs               # TOUCHES sub-screen state machine (list + dropdown)
│       ├── ffi/gl.rs             # OpenGL FFI subset (hand-written, no bindgen) + glReadPixels/PACK_ALIGNMENT
│       └── backend/
│           ├── render.rs         # SwitchRenderBackend : atlas + UV wrap + GlStateCache + mega-arena +
│           │                     #   masquage stencil INCR/DECR + filtres Glow/DropShadow/Blur/ColorMatrix/Bevel +
│           │                     #   FilterTexturePool TTL + render_offscreen/resolve_sync_handle (BitmapData.draw) + UI overlays
│           ├── audio.rs          # SwitchAudioBackend (port CpalAudioBackend → libnx audren)
│           ├── storage.rs        # SwitchStorageBackend (port DiskStorageBackend → sdmc:/flashnx/ à plat)
│           ├── tracing.rs        # Route les events tracing de Ruffle vers stdout nxlink
│           └── log.rs            # SwitchLogBackend → ruffle_log_cstr
├── patches/
│   ├── README.md                 # Comment ré-appliquer après git submodule update
│   └── 0001-mario63-zero-scale-hit-test.patch  # Fix Toad château #6906
├── third_party/
│   ├── ruffle/                   # git submodule + patches/*.patch appliqués
│   └── jpeg-decoder-switchfork/  # vendored jpeg-decoder-0.3.2, select_worker → Immediate forcé
├── assets/{icon.jpg, banner.png, cacert.pem, screenshots/, *.nacp}
└── scripts/{build.sh, build.ps1, setup-env.ps1, setup-env.sh}
```

Les backends Navigator/UI/Video utilisent les implémentations `Null*` fournies par défaut par `ruffle_core` — pas de fichier dédié. **Audio** = `SwitchAudioBackend`. **Storage** = `SwitchStorageBackend`.

### Assets embarqués dans le `.nro`

| Asset | Format | Dimensions | Usage |
|---|---|---|---|
| `assets/icon.jpg` | JPEG baseline sRGB | 256×256 | Icône `.nro` reprise par hbmenu / Sphaira via `elf2nro --icon=` ([cpp/Makefile](cpp/Makefile)). |
| `assets/banner.png` | PNG RGBA | 720×144 (ratio 5:1) | Banner du top de la library UI : embarqué via `include_bytes!` dans [rust/src/library.rs](rust/src/library.rs), décodé via crate `png` 0.18 au boot, uploadé en texture GL (`upload_rgba_texture`), rendu par `draw_textured_rect` (1 quad texturé/frame). Fallback ASCII "FLASHNX" si le decode échoue. |
| `assets/cacert.pem` | PEM | — | Bundle CA Mozilla pour libcurl HTTPS (import archive.org), embarqué via `include_bytes!` + écrit sur SD au 1er boot. |

`assets/screenshots/` n'est utilisé que par le README — pas embarqué dans le `.nro`.

## Build & netload

```bash
# 1. BUILD (depuis Git Bash, à la racine du repo)
./scripts/build.sh            # release : LTO=full, ~3 min, .nro officiel
./scripts/build.sh --dev      # release-dev : LTO=thin + codegen-units=16, ~30 s rebuild
#   Équivalents : `make` (= build.sh) / `make dev` (= build.sh --dev) /
#   `scripts\build.ps1 [--dev]` depuis PowerShell. Tous délèguent à build.sh.

# 2. NETLOAD (Switch : Homebrew Menu → Y pour passer en netloader)
nxlink -s cpp/FlashNX.nro     # push le .nro par WiFi + redirige stdout du Switch vers ce terminal
```

> ⚠️ `scripts/build.sh` est **le seul chemin de build supporté**. Ne pas appeler
> `cargo build --target aarch64-unknown-linux-gnu` (mauvais target → échec libc
> `target_env=gnu`). Le bon target (`aarch64-nintendo-switch-freestanding`,
> tier-3, build-std) est fourni par `rust/.cargo/config.toml` ; build.sh fait
> `cargo build` sans `--target`. Le `Makefile` racine et `build.ps1` ne sont que
> des wrappers fins autour de build.sh.

> 💡 Le flag `-s` de `nxlink` garde le terminal attaché au stdout du Switch :
> c'est là qu'apparaissent le heartbeat de perf (`f1234: fps=… tick=…ms
> render=…ms …`) et les lignes `SLOW f…` du détecteur de frame lente (voir
> [rust/src/lib.rs](rust/src/lib.rs) `render_frame_with_dt`).

Le script orchestre :
1. `cargo build --release` (ou `--profile release-dev`) côté Rust (target `aarch64-nintendo-switch-freestanding`, std-via-newlib, build-std nightly) → `rust/target/.../libruffle_switch.a`.
2. `make` côté C++ lancé **dans le bash MSYS2 de devkitPro** (pour que `switch_rules` résolve les paths) → link contre `libruffle_switch.a` + libnx + libEGL/libGLESv2 → `cpp/FlashNX.nro`.

Le Makefile a `libruffle_switch.a` comme dépendance explicite du `.elf`, donc tout changement Rust déclenche le relink C++ automatiquement (plus besoin de `make clean` manuel). Le profile `release-dev` est sélectionné via la variable d'env `RUST_PROFILE` que `build.sh --dev` exporte.

## Tester sur Switch

1. Copier tes `.swf` sur la SD dans **`sdmc:/flashnx/`** (ou `sdmc:/switch/flashnx/`). N'importe quel nom de fichier marche. Le legacy `sdmc:/ruffle/` est aussi scanné pour la backward-compat. Au boot le `.nro` ouvre la **library FlashNX** : liste des `.swf` détectés avec banner + color chip par jeu.
   - **Inputs LOCAL** : haut/bas = naviguer (D-pad **ou stick gauche ou stick droit** — **hold pour scroller vite**), **A** = JOUER, **X** = OPTIONS (TOUCHES + RENOMMER + RETOUR), **Y** = bascule en mode DISTANT (import archive.org), **−** = quitter le `.nro`.
   - **Mode DISTANT** : **A** = saisir une URL archive.org via le clavier soft (`swkbd`) ; sur DistantIdle l'historique est affiché, **L/R** cyclent dedans, **ZR** = fetch direct l'URL affichée sans rouvrir le clavier. Sur la liste des fichiers distants, badge `OK` à côté de ceux déjà sur SD, **A** lance le téléchargement (no-op silencieux sur les `OK`). Progress bar live, **B** annule en cours.
   - **Empty state** : si SD vide, la library affiche "AUCUN JEU" + instructions où poser les `.swf`. **Y** permet d'aller en DISTANT pour télécharger.
   - **Fallback ultime** (library init fail) : `ruffle_init` avec un SWF embarqué 43 octets (fond rouge). Le `.nro` ne refuse jamais de booter.
2. Switch en mode **netloader** : Homebrew Menu → `Y` (ou `R` sur anciennes versions).
3. PC : `nxlink -s cpp/FlashNX.nro`.

**Contrôles in-game** (binding par défaut platformer ; remappable via TOUCHES) :

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

Dans le menu pause : D-pad **ou stick gauche ou stick droit** pour naviguer (**hold pour scroller** dans l'éditeur TOUCHES), **A** valide, **B** ou **Minus** referme. **« QUITTER » revient à la library FlashNX** (depuis la library, **−** = exit du `.nro`). « REDEMARRER » recharge le SWF depuis zéro (conserve les `.sol`), « TOUCHES » ouvre l'éditeur de keymap.

## Customisation des touches

Deux moyens — l'éditeur in-game est de loin le plus simple.

### Éditeur in-game « TOUCHES » (recommandé)

Depuis le menu pause (**Minus** in-game) OU depuis OPTIONS dans la library (**X** sur un jeu avant de le lancer), sélectionne **TOUCHES** + A. Tu vois la liste des boutons Switch avec leur binding actuel entre `[ brackets ]`. Navigue avec haut/bas (**hold pour scroller vite**), **A** sur une ligne ouvre un dropdown des **48 touches Flash supportées** :

- **Modifiers / nav** : `(aucune)`, `Space`, `Enter`, `Escape`, `Shift`, `Control`, `Alt`, `Tab`, `Backspace`
- **Flèches** : `Up`, `Down`, `Left`, `Right`
- **Lettres** : `A`..`Z` · **Chiffres** : `0`..`9`

Le dropdown est scrollable (10 visibles, scrollbar à droite). **A** confirme, **B** annule. À chaque confirmation, le sidecar JSON est sauvé sur SD ET le binding s'applique immédiatement (pas besoin de REDEMARRER). Le sidecar écrit est **`sdmc:/flashnx/<basename>.keymap.json`** — par jeu, sans toucher au default global.

### Édition JSON manuelle (power users)

Le `.nro` lit / écrit des JSON sur la SD.

**Hiérarchie de lookup** (premier hit gagne) :
1. `sdmc:/flashnx/<basename>.keymap.json` — override par jeu
2. `sdmc:/ruffle/<basename>.keymap.json` — legacy backward-compat
3. `sdmc:/flashnx/keymap_default.json` — default global choisi par toi
4. `sdmc:/ruffle/keymap_default.json` — legacy
5. Fallback hardcodé dans le `.nro` — la table ci-dessus

Au premier boot, si aucun `keymap_default.json` n'existe nulle part, le `.nro` l'écrit dans `sdmc:/flashnx/keymap_default.json` avec le fallback hardcodé.

**Schema** (exemple) :
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

**Boutons Switch supportés** : `A`, `B`, `X`, `Y`, `L`, `R`, `ZL`, `Plus`, `Up`/`Down`/`Left`/`Right` (D-pad), `StickLUp`/`StickLDown`/`StickLLeft`/`StickLRight`. Boutons absents = unbound. `Minus` est réservé au menu pause. `ZR` est réservé au clic souris in-game (dispo dans la library en mode DISTANT pour fetch-without-keyboard).

À chaque boot le `.nro` logue via nxlink la résolution finale (`keymap: resolved 16 bindings: A=1 B=8 ...`).

## Customisation du nom d'affichage (RENOMMER)

Pour renommer un jeu **sans toucher au fichier `.swf` physique** (saves + keymap restent stables) :

1. Dans la library, sélectionne le jeu, **X** = OPTIONS → **RENOMMER** + A.
2. swkbd s'ouvre pré-rempli avec le nom actuel — édite.
3. Submit → écrit `sdmc:/flashnx/<basename>.meta.json` avec `{"display_name": "..."}`. Champ vide = supprime le sidecar = retour au basename.

La library lit ce sidecar à chaque scan SD. Le metadata panel continue d'afficher `[basename.swf]` en petit. **Pattern Steam/ScummVM/iTunes** : alias d'affichage uniquement. Saves `.sol`, keymap et SharedObject URLs (`http://flashforswitch.local/<basename>`) restent stables.

## Layout SD card

Tout vit **à plat** dans `sdmc:/flashnx/` :

```
sdmc:/flashnx/
├── Super_Mario_63_2010.swf                           ← le jeu lui-même
├── Super_Mario_63_2010.swf.keymap.json               ← keymap per-game
├── Super_Mario_63_2010.swf.meta.json                 ← display_name (rename)
├── Super_Mario_63_2010.swf.<SaveName>.sol            ← save (flat)
├── keymap_default.json                               ← default global keymap
└── ...
```

**Backward-compat** : les fichiers dans l'ancien `sdmc:/ruffle/` sont détectés et utilisés (scan + read-fallback). Les saves dans l'ancienne arbo nested `sdmc:/ruffle/saves/<host>/<basename>/<sol>.sol` sont lues puis migrées automatiquement vers le path flat à la prochaine sauvegarde.

**Fichiers système** (dans le dossier homebrew) :
- `sdmc:/switch/FlashNX/cacert.pem` — bundle CA Mozilla pour HTTPS
- `sdmc:/switch/FlashNX/distant_history.json` — historique URLs archive.org (persisté)
- `sdmc:/switch/ruffle-crash.log` — replay du dernier crash natif (vu au boot suivant via nxlink)

## Limitations connues

Pour jouer des SWF AS1/AS2 courants, c'est fonctionnellement quasi complet. Inventaire honnête de ce qui reste.

### Rendu (backend)

| Manque | Impact |
|---|---|
| **Filtres** GradientGlow / GradientBevel / Convolution / DisplacementMap | Droppés (passthrough). Faits : ColorMatrix / Blur / Glow / DropShadow / Bevel. |
| **`BitmapData.draw()`** : clear-transparent (pas de composite sur l'existant), texture temp par appel (pas de pool) | OK pour le pattern tile-engine (SMWF), pas fidèle pour tous les usages BitmapData. |
| **Blend modes** Alpha / Erase (besoin de layer tracking) ; blend imbriqué (FBO offscreen non récursif) | Trivial (Add/Screen…) et Complex (Multiply/Overlay…) sont faits. |
| **Perf des filtres en transitions menu** (N passes FBO/frame) | Hoquets sur menus très filtrés (Mario 63). Bornés par budget/frame + pool TTL. Vrai fix = batching des passes. |
| **Context3D / Stage3D**, **PixelBender** (AS3) | Non implémentés (stubs). Quasi nul pour du Flash 2D AS1/2. |

### Cœur Ruffle (hors de notre portée)

- **Perf des jeux lourds** — limite de l'**interpréteur Ruffle** (pas de JIT). Sur Mario 63 en scène dense, le `tick` de simulation peut atteindre des centaines de ms/frame et enfle avec le temps (fuite objets/mémoire Mario 63 + Ruffle documentée upstream) ; notre rendu reste à ~5-15 ms/frame. **Non corrigeable depuis le backend** (web-vérifié : Ruffle lague même sur i7). Levier hors-code : overclock CPU (mode dock / sys-clk).
- **AS3 / AVM2** — supporté partiellement par Ruffle, perf pire (pas de JIT). Badge `AS3` dans la library.

### Plateforme / distribution

- **Savestate** — sciemment skippé : Ruffle n'expose pas de `Player::serialize()` (le state est un graphe `gc-arena`). Un vrai savestate = chantier ~2 semaines upstream. Les saves `.sol` natives fonctionnent.
- **Library** — Supprimer un jeu (avec confirm), tri, jaquettes : non implémentés.
- **Packaging hb-app.store** + **doc utilisateur étendue** — pour une diffusion plus large.
- **Forwarders home menu** — doc-only ([Sphaira](https://github.com/ITotalJustice/sphaira) génère le NSP, pas de code chez nous). ⚠️ Les forwarders ressemblent à des jeux pour Nintendo : safe seulement sur emuNAND.

## Contraintes / faits à retenir

- **AS2 pur interprété** (ex. Mario 63) → AVM2 JIT non nécessaire, service Horizon `jit:u` non requis.
- **Deps Ruffle** : `cpal`/`reqwest`/`tokio`/`wgpu` ne sont **pas** dans `ruffle_core`, juste dans `ruffle_desktop` → rien à neutraliser. `flate2` workspace default = `miniz_oxide` (pure Rust). Tout linke direct.
- **FFI libnx utilisée** :
  - HID : `padConfigureInput`/`padInitializeDefault`/`padUpdate`/`padGetButtonsDown/Up`/`padGetStickPos`/`hidInitializeTouchScreen`/`hidGetTouchScreenStates` (dans `cpp/src/main.cpp`)
  - Applet : `appletMainLoop`, `nwindowGetDefault`, `appletSetCpuBoostMode(FastLoad)`. Le cycle suspend/resume (home button, veille) est géré implicitement par `appletMainLoop` — pas de hooks `appletHook` nécessaires en pratique.
  - Socket : `socketInitializeDefault`, `nxlinkStdio` (stdout réseau)
  - FS : `sdmc:/...` monté auto par crt0 libnx → `std::fs::read` marche depuis Rust (sauf `read_dir` qui bug — d'où le scan C++ `opendir`/`readdir`)
  - Thread : `threadCreate`/`threadStart`/`threadWaitForExit`/`threadClose` (worker GL + worker audio)
  - System : `svcGetInfo` (RAM via `ruffle_query_ram`), `armGetSystemTick` (pacing dt réel)
  - Audio : `audrenInitialize`/`audrvCreate`/`audrvVoiceInit`/`audrvVoiceAddWaveBuf`/`audrvUpdate`/… (cf. [cpp/src/audio.cpp](cpp/src/audio.cpp))
- **Bindgen** : non utilisé. FFI écrite à la main dans `rust/src/ffi/gl.rs` (subset GL 4.3 core) et `cpp/src/ruffle_bridge.cpp`.
- **Pattern d'architecture** : ScummVM `backends/platform/sdl/switch/` (séparation OSystem → OSystem_SDL → OSystem_Switch). Adapté ici : trait `RenderBackend` (de Ruffle) + impl `SwitchRenderBackend` mince.

## Toolchain

- **devkitPro** dans `C:\devkitPro\` avec packages `switch-dev`, `switch-mesa`, `switch-glm`, `switch-glad`, `switch-curl`, `switch-mbedtls`.
- **Rust** : toolchain pin via `rust/rust-toolchain.toml` → `nightly-x86_64-pc-windows-gnu` + `rust-src` (host GNU obligatoire — MSVC casse les build scripts sans Visual Studio Build Tools).
- **MinGW-w64** via `scoop install mingw` — pour `dlltool.exe` que Rust nightly GNU embarque buggé. `scripts/build.sh` ajoute `~/scoop/apps/mingw/current/bin` au PATH avant `cargo build`.
- **`third_party/jpeg-decoder-switchfork/`** : fork patchée de `jpeg-decoder` 0.3.2 (`select_worker` retourne toujours `Immediate`), référencée via `[patch.crates-io]` dans `rust/Cargo.toml`. Sans elle, les JPEG > 128×128 px font spawner un `std::thread` qui crashe la pthread shim newlib.

### Patches rust-src à ré-appliquer après chaque `rustup update`

**Patch 1** — `…\nightly-x86_64-pc-windows-gnu\lib\rustlib\src\rust\library\std\build.rs` : ajouter après la ligne `|| (target_vendor == "nintendo" && target_env == "newlib")` :

```rust
|| (target_vendor == "nintendo" && target_os == "horizon")
```

Sans ça, stdlib se compile en mode `restricted_std` → tous les crates std de crates.io (memchr, num-traits, thiserror…) refusent de compiler.

**Patch 2** — `…\nightly-x86_64-pc-windows-gnu\lib\rustlib\src\rust\library\std\src\hash\random.rs` : envelopper le corps de `RandomState::new()` dans un cfg-switch :

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

Sans ça, `HashMap::new()` puis `.insert()` crash sur hardware (le lazy thread_local de stdlib avec init par fonction crashe sur notre target). Hash-flooding DoS non pertinent pour un player Flash.

**Gotchas environnement** :
- Avast Web Shield (HTTPS scanning) intercepte pacman/pkg.devkitpro.org en injectant son root CA → désactiver le scan HTTPS avant `pacman -Sy`.
- Avast CyberCapture flag les build scripts cargo compilés en `.exe` → ajouter une exception sur le dossier du projet.
- Le `make` chocolatey ne gère pas les paths MSYS-style devkitPro → `scripts/build.sh` délègue à `/c/devkitPro/msys2/usr/bin/bash -lc 'make'`.
- Target `aarch64-nintendo-switch-freestanding` = tier-3 → pas de rust-std pré-built → `-Z build-std` → nightly requis.

## Hardware

- Switch moddée Atmosphère.
- nxlink pour stdout réseau (debug) + netloader (push `.nro` par WiFi).
- SD : copier le `.nro` dans `sdmc:/switch/FlashNX.nro` pour le mode SD (non requis si netload).
- SWFs cherchés en priorité dans `sdmc:/flashnx/` (voir « Tester sur Switch »).

## Références

- Ruffle : https://github.com/ruffle-rs/ruffle (`render/src/backend.rs` pour le trait à impl)
- aarch64-switch-rs : https://github.com/aarch64-switch-rs/{nx,cargo-nx}
- libnx doc : https://switchbrew.github.io/libnx/
- ScummVM Switch (pattern reference) : `backends/platform/sdl/switch/`
- GBAtemp Switch homebrew dev : https://gbatemp.net/forums/switch-homebrew-development.300/
