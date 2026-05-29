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
│       ├── lib.rs                # FFI exports + PlayerBuilder + SWF loader + input handlers + ruffle_set_swf_path (Phase 2.6) + TOUCHES FFI (Phase 3.3) + tick/render profiling (accum + max)
│       ├── library.rs            # Library UI state + banner/icon embed + SWF header parse + meta/keymap sidecars (Phase 3.4)
│       ├── net.rs                # Import distant archive.org : fetch JSON + download async curl multi (Phase 3.7)
│       ├── keymap.rs             # JSON keymap (sidecar + default + fallback) + mutation API (Phase 3.3)
│       ├── menu.rs               # TOUCHES sub-screen state machine (list + dropdown) (Phase 3.3)
│       ├── ffi/gl.rs             # OpenGL FFI subset (no bindgen — hand-written) + glReadPixels/PACK_ALIGNMENT (Phase 2.7)
│       └── backend/
│           ├── render.rs         # SwitchRenderBackend (~5300 lignes) : atlas + edge replication + UV wrap + GlStateCache (2.5) + mega-arena (2.1.b) + masquage stencil INCR/DECR (2.7) + filtres Glow/DropShadow/Blur/ColorMatrix/Bevel + FilterTexturePool TTL (2.3) + render_offscreen/resolve_sync_handle BitmapData.draw (2.7) + library/menu overlays
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

## Assets branding FlashNX ✓ livrés 2026-05-26 + ✓ embarqués 2026-05-26 nuit (Phase 3.4)

Deux logos en place dans `assets/`, tous deux **wired et actifs dans le `.nro`** :

| Asset | Format | Dimensions | État | Usage |
|---|---|---|---|---|
| `assets/icon.jpg` | JPEG baseline, sRGB, no alpha | **256×256** px | ✓ livré + embarqué | Icône `.nro` repris automatiquement par hbmenu / Sphaira / forwarders home menu via `elf2nro --icon=...` (cf. [cpp/Makefile](cpp/Makefile)). `.nacp` correspondant : `APP_TITLE = "FlashNX"`, `APP_AUTHOR = "flash-for-switch contributors"`. Test à la production : check lisibilité de `icon.jpg` réduit à 100×100 px (taille hbmenu/Sphaira) — si bruit du fond éclair fatigue à cette échelle, simplifier le background plus tard. |
| `assets/banner.png` | PNG RGBA | **720×144** px (ratio 5:1) | ✓ livré + embarqué | Banner du top de la library UI, embarqué via `include_bytes!` dans [rust/src/library.rs](rust/src/library.rs), décodé via crate `png` 0.18 au boot, uploadé en texture GL via `SwitchRenderBackend::upload_rgba_texture` ([rust/src/backend/render.rs](rust/src/backend/render.rs)), rendu par `draw_textured_rect` qui réutilise le `bitmap_prog` existant (1 quad textured par frame au lieu de ~120 draw_rect d'un titre ASCII). Auto-scale si banner > viewport - 64 px. Fallback ASCII "FLASHNX" si le decode échoue. |

## Build

```bash
./scripts/build.sh            # release : LTO=full, ~3 min, .nro ~12.5 MB (officiel)
./scripts/build.sh --dev      # release-dev : LTO=thin + codegen-units=16, ~30 s rebuild, .nro ~13.2 MB
```

Le script orchestre :
1. `cargo build --release` (ou `--profile release-dev`) côté Rust (target `aarch64-nintendo-switch-freestanding`, std-via-newlib, build-std nightly) → `rust/target/.../libruffle_switch.a` (~13-14 MB avec features audio+mp3)
2. `make` côté C++ lancé **dans le bash MSYS2 de devkitPro** (pour que `switch_rules` résolve les paths correctement) → link contre `libruffle_switch.a` + libnx + libEGL/libGLESv2 → `cpp/flash-for-switch.nro` (~12.2 MB)

Le Makefile a `libruffle_switch.a` comme dépendance explicite du `.elf`, donc tout changement Rust déclenche le relink C++ automatiquement (plus besoin de `make clean` manuel après chaque modif Rust). Le profile `release-dev` est sélectionné via la variable d'env `RUST_PROFILE` que `build.sh --dev` exporte.

**État 2026-05-29 — plateforme jouable end-to-end, pipeline de rendu Flash quasi complet** :

Highlights de la session 2026-05-29 (détails dans Phase 2.3 / 2.7 ci-dessous) :
- **Super Mario World Flash entièrement jouable** (titre → overworld/sélection de niveau → niveaux). SMWF est un **moteur de tuiles BitmapData** : il rastérise son monde dans des `BitmapData` via `draw()` + `copyPixels`, puis l'affiche. Fix = implémentation de `render_offscreen` (BitmapData.draw) + `resolve_sync_handle` (readback GPU→CPU). Le terrain s'affiche.
- **Masquage stencil réécrit** (schéma INCR/DECR) — l'ancien schéma bit-OR/REPLACE rejetait tout le maskee sur du contenu réel (overworld SMWF = écran tout bleu). Désormais correct.
- **Filtres Flash dé-PARKED et fonctionnels** : Glow / DropShadow / Blur / ColorMatrix **+ Bevel** (le « reflet/relief » sur les textes Mario 63). Pool de textures borné par récence (TTL) + garde `glGenTextures==0` + budget de filtres par frame.
- **Crash Mario 63 réglé** : c'était le pool de filtres non borné → épuisement GL textures → `glGenTextures` retourne 0 → NULL-deref Mesa. Mario 63 atteint le jeu sans crash.
- **Font fallback embarqué** (`default_font` → Noto Sans) : plus de texte HTML invisible (`verdana`/`Arial` device fonts).
- **Limite connue assumée** : Mario 63 **en jeu dense** est CPU-bound côté **interpréteur Ruffle** (sim `tick` jusqu'à ~385 ms/frame, qui enfle avec le temps = fuite objets/mémoire Mario 63+Ruffle documentée). Notre rendu reste à ~5-15 ms/frame. **Non corrigeable depuis le backend** (web-vérifié : Ruffle lague même sur i7 sur ce jeu). Levier hors-code : mode dock (CPU ~1.78 vs ~1.02 GHz).

**Acquis antérieurs — Phases 2.x + 3.1/3.2/3.3/3.4/3.4.bis/3.7 ✓** :
- **Library UI FlashNX** ([rust/src/library.rs](rust/src/library.rs)) — banner.png + icon.jpg embarqués, color chip + animations sin, scrollable, OPTIONS modal (TOUCHES + RENOMMER + RETOUR), empty state, metadata panel
- **Import distant archive.org Phase 3.7** ([rust/src/net.rs](rust/src/net.rs) + [cpp/src/net.cpp](cpp/src/net.cpp)) — Y depuis LOCAL → DistantIdle → swkbd URL → fetch JSON archive.org → liste files (badge `OK` sur ceux déjà sur SD) → download async via curl multi handle avec progress bar live → auto-add à la library. Historique URLs persisté (`distant_history.json`), L/R cycle, **ZR** = re-fetch sans rouvrir le clavier
- **QUITTER pause menu → back to library** (refactor OnceLock→Mutex<Option> de OVERRIDE/CACHED/keymap pour permettre la ré-init) — joue un jeu, QUITTER pour revenir à la library, choisis-en un autre, etc.
- **Hold-to-scroll** sur D-pad + stick gauche dans library + TOUCHES editor (400ms initial + 80ms repeat). Face buttons restent one-shot
- **RENOMMER (Phase 3.4.bis)** — sidecar `.meta.json` next to the `.swf` avec `{"display_name": "..."}`, le `.swf` n'est jamais renommé. swkbd pré-rempli avec le display_name actuel
- **Stage rendering forcé** : `ShowAll` + `align=centered` + `Letterbox::On` — fix les SWF qui se rendaient en mini-rectangle dans le coin (NoScale par AS) ou décalés à gauche (Align L par AS) ou avec leak hors-stage sur les côtés
- **48 touches Flash supportées** dans la dropdown TOUCHES : A-Z, 0-9, Space, Enter, Escape, Shift, Control, Alt, Tab, Backspace, flèches. Dropdown scrollable (10 visibles avec scrollbar)
- **Layout SD à plat** : `sdmc:/flashnx/<basename>.<sol>.sol` pour les saves (au lieu de nested `<host>/<basename>/<sol>.sol`), `<basename>.keymap.json`, `<basename>.meta.json`. Tous les sidecars + saves dans un seul dossier flat à côté du `.swf`. Backward-compat read-fallback sur `sdmc:/ruffle/` legacy
- **Root SD renommé** `sdmc:/ruffle/` → **`sdmc:/flashnx/`** (downloads + writes vont là). Scan order : `flashnx/ → ruffle/ → switch/flashnx/ → switch/ruffle/` pour pas casser les users post-rename
- `.nro` ~12.8 MB (filtres + Bevel + font fallback embarqué inclus)
- Toutes les phases 2.x ✓ (sprites + audio + mega-arena GL + storage + Toad #6906 + GL state cache + CpuBoostMode + file picker + **filtres/Bevel + BitmapData.draw + masquage INCR/DECR**)
- **Mario 63 + Super Mario World Flash (jouable de bout en bout) + Mario Forever Flash + Tetris'd + Flappy Bird + Flash Equestria + There Is Only One Level + Mario 3D Racing** : tous testés sur hardware. fps stable 55-60 sur la plupart ; Mario 63 en jeu dense est limité par la simulation Ruffle (voir Phase 2.7)

## Tester sur Switch

1. Copier tes `.swf` sur la SD dans **`sdmc:/flashnx/`** (ou `sdmc:/switch/flashnx/`). N'importe quel nom de fichier marche. Le legacy `sdmc:/ruffle/` est aussi scanné pour la backward-compat. Au boot le `.nro` ouvre la **library FlashNX** : liste des `.swf` détectés avec banner + color chip par jeu + animations sin sur le curseur/sélection.
   - **Inputs LOCAL** : haut/bas = naviguer (D-pad **ou stick gauche ou stick droit** — **hold pour scroller vite**), **A** = JOUER, **X** = OPTIONS (TOUCHES + RENOMMER + RETOUR), **Y** = bascule en mode DISTANT (import archive.org), **−** = quitter le `.nro`. *(Le stick droit ne pilote la souris qu'en jeu ; dans la library il sert aussi à naviguer.)*
   - **Mode DISTANT** : **A** = saisir une URL archive.org via le clavier soft Switch (`swkbd`) ; sur DistantIdle l'historique est affiché et **L/R** cyclent dedans, **ZR** = fetch direct l'URL affichée sans rouvrir le clavier. Sur la liste des fichiers distants, badge `OK` à côté de ceux déjà sur ta SD, **A** lance le téléchargement (bloqué silencieusement sur les `OK` pour éviter le re-DL). Progress bar live, **B** annule en cours.
   - **Empty state** : si SD vide, la library affiche "AUCUN JEU" + instructions où poser les `.swf`. **Y** te permet quand même d'aller en DISTANT pour télécharger via archive.org.
   - **Fallback ultime** (library init fail) : ruffle_init avec le SWF embarqué 43-octet (fond rouge). Le `.nro` ne refuse jamais de booter.
2. Switch en mode **netloader** : Homebrew Menu → `Y` (ou `R` sur anciennes versions)
3. PC : `nxlink -s cpp/flash-for-switch.nro`

**Contrôles in-game** (binding par défaut Mario 63 platformer ; remappable via TOUCHES) :

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

Dans le menu pause : D-pad **ou stick gauche ou stick droit** haut-bas pour naviguer (**hold pour scroller** dans l'éditeur TOUCHES), **A** valide, **B** ou **Minus** referme sans rien faire. **« QUITTER » revient à la library FlashNX** (depuis la library, **−** = exit du `.nro`). « REDEMARRER » recharge le SWF depuis zéro (conserve les sauvegardes `.sol`), « TOUCHES » ouvre l'éditeur de keymap (voir ci-dessous).

## Customisation des touches

Deux moyens, l'éditeur in-game est de loin le plus simple :

### Éditeur in-game « TOUCHES » (recommandé)

Depuis le menu pause (**Minus** in-game) OU depuis OPTIONS dans la library (**X** sur un jeu avant de le lancer), sélectionne **TOUCHES** + A. Tu vois la liste des boutons Switch avec leur binding actuel entre `[ brackets ]`. Navigue avec haut/bas (**hold pour scroller vite**), **A** sur une ligne ouvre un dropdown des **48 touches Flash supportées** :

- **Modifiers / nav** : `(aucune)`, `Space`, `Enter`, `Escape`, `Shift`, `Control`, `Alt`, `Tab`, `Backspace`
- **Flèches** : `Up`, `Down`, `Left`, `Right`
- **Lettres** : `A`..`Z` (les 26)
- **Chiffres** : `0`..`9`

Le dropdown est scrollable (10 visibles, scrollbar à droite, hold-to-scroll). **A** confirme, **B** annule.

À chaque confirmation, le sidecar JSON est sauvé sur SD ET le binding s'applique immédiatement en jeu (pas besoin de REDEMARRER pour tester). Le sidecar écrit est **`sdmc:/flashnx/<basename>.keymap.json`** — par jeu, sans toucher au default global.

### Édition JSON manuelle (power users)

Si tu préfères tout faire au clavier depuis ton PC, le `.nro` lit / écrit des JSON sur la SD.

**Hiérarchie de lookup** (premier hit gagne) :
1. `sdmc:/flashnx/<basename>.keymap.json` — override par jeu (ex. `sdmc:/flashnx/Super_Mario_63_2010.swf.keymap.json`)
2. `sdmc:/ruffle/<basename>.keymap.json` — legacy backward-compat
3. `sdmc:/flashnx/keymap_default.json` — default global choisi par toi
4. `sdmc:/ruffle/keymap_default.json` — legacy
5. Fallback hardcodé dans le `.nro` — la table ci-dessus

Au premier boot, si aucun `keymap_default.json` n'existe nulle part, le `.nro` l'écrit dans `sdmc:/flashnx/keymap_default.json` avec le fallback hardcodé — ouvre-le dans Notepad pour voir le schema et adapter.

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

**Noms de boutons Switch supportés** : `A`, `B`, `X`, `Y`, `L`, `R`, `ZL`, `Plus`, `Up`/`Down`/`Left`/`Right` (D-pad), `StickLUp`/`StickLDown`/`StickLLeft`/`StickLRight` (stick gauche directionnel). Boutons absents = unbound. `Minus` est réservé pour le menu pause et ne peut pas être remappé. `ZR` est réservé au clic souris in-game (mais dispo dans la library en mode DISTANT pour fetch-without-keyboard).

**Noms de touches Flash supportées** (48) : voir la liste plus haut. Toute valeur inconnue dans le JSON est ignorée avec un warning nxlink.

**Vérification** : à chaque boot le `.nro` logue via nxlink la résolution finale (`keymap: resolved 16 bindings: A=1 B=8 ...`) — utile pour confirmer que ton fichier est bien pris en compte.

## Customisation du nom d'affichage (RENOMMER, Phase 3.4.bis)

Pour renommer un jeu **sans toucher au fichier `.swf` physique** (saves + keymap restent stables) :

1. Dans la library, sélectionne le jeu, **X** = OPTIONS
2. **RENOMMER** + A
3. swkbd s'ouvre pré-rempli avec le nom actuel — édite à ta convenance
4. Submit → écrit `sdmc:/flashnx/<basename>.meta.json` avec `{"display_name": "..."}`. Champ vide = supprime le sidecar = retour au basename.

La library lit ce sidecar à chaque scan SD et affiche le `display_name` à la place du basename. Le metadata panel en bas continue d'afficher `[basename.swf]` en petit pour que tu voies toujours le vrai fichier.

**Pattern Steam/ScummVM/iTunes** : alias d'affichage uniquement, jamais de rename de fichier. Saves `.sol`, keymap `.keymap.json` et SharedObject URLs (`http://flashforswitch.local/<basename>`) restent stables peu importe combien de fois tu renommes.

## Layout SD card

Tous tes fichiers FlashNX vivent **à plat** dans `sdmc:/flashnx/` (depuis le rename 2026-05-26) :

```
sdmc:/flashnx/
├── Super_Mario_63_2010.swf                           ← le jeu lui-même
├── Super_Mario_63_2010.swf.keymap.json               ← keymap per-game (3.3)
├── Super_Mario_63_2010.swf.meta.json                 ← display_name (3.4.bis)
├── Super_Mario_63_2010.swf.SuperMarioSunshine128SavedFile.sol  ← save (flat 3.4)
├── Mario_Forever_Flash.swf
├── Mario_Forever_Flash.swf.keymap.json
├── keymap_default.json                               ← default global keymap
└── ...
```

Tout est à plat, scannable d'un coup d'œil, déplaçable par drag-drop depuis le PC.

**Backward-compat** : si tu as des fichiers dans l'ancien `sdmc:/ruffle/`, le `.nro` les détecte et les utilise quand même (scan + read-fallback). Pour les saves, l'ancienne arbo nested `sdmc:/ruffle/saves/<host>/<basename>/<sol>.sol` est lue puis migrée automatiquement vers le nouveau path flat à la prochaine sauvegarde (auto-flatten on write). Tu peux supprimer `sdmc:/ruffle/` une fois que tu as déplacé tes `.swf` / `.keymap.json` / `.meta.json` et joué chaque jeu une fois (pour que les saves migrent).

**Autres fichiers système** (cachés du user, dans le dossier homebrew) :
- `sdmc:/switch/flash-for-switch/cacert.pem` — bundle CA Mozilla pour HTTPS (Phase 3.7)
- `sdmc:/switch/flash-for-switch/distant_history.json` — historique URLs archive.org (Phase 3.7, persisté)
- `sdmc:/switch/ruffle-crash.log` — replay du dernier crash natif (vu au boot suivant via nxlink)

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
| 1.3.5 ✓ Masking via stencil (validé hardware 2026-05-23) | ~2 h inclus dans le rush 1.3 | **Résolu**. La config EGL ([cpp/src/gl_context.cpp:32](cpp/src/gl_context.cpp#L32)) demande déjà `EGL_STENCIL_SIZE=8` donc aucun changement C++ requis. State machine 4-temps : `push_mask` → `glColorMask(false×4)` + `glStencilFunc(ALWAYS, value, 0xFF)` + `glStencilOp(KEEP, KEEP, REPLACE)` (dessin du masque dans le stencil seul) ; `activate_mask` → réactive la couleur, `glStencilFunc(EQUAL, value, 0xFF)` + `glStencilOp(KEEP, KEEP, KEEP)` (le maskee passe seulement où le masque a écrit) ; `deactivate_mask` revient en mode "dessiner masque" pour pop propre ; `pop_mask` désactive le stencil si plus de mask actif. Démo : rect orange 400x140 visible seulement à travers un petit masque carré 100x80. **⚠️ Réécrit 2026-05-29** (voir Phase 2.7) : le schéma original bit-OR + `REPLACE` + `glClear` par push marchait sur la démo mais **rejetait tout le maskee sur du contenu réel** (overworld SMWF = écran tout bleu) — la valeur écrite ne matchait pas le `EQUAL` de gating. Remplacé par le schéma standard **INCR/DECR + `EQUAL depth`** (push=INCR la couverture, activate=`glStencilFunc(EQUAL, depth)`, deactivate=DECR, pas de `glClear` par push). |
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
| 2.3 ✓ Filtres Flash (Glow / DropShadow / Blur / ColorMatrix **+ Bevel**) | **Livré 2026-05-29** (3e fois la bonne). Port fidèle des shaders WGSL→GLSL + infra FBO + texture pool + dispatcher + boucle cache_entries dans submit_frame. Les 3 bugs de la 2e tentative résolus : **(1) boutons invisibles** = en fait deux causes distinctes — le **masquage** cassé (cf. Phase 1.3.5/2.7) ET le **Bevel** non implémenté (le « reflet » des textes Mario 63) ; Bevel maintenant porté ([bevel.wgsl](third_party/ruffle/render/wgpu/shaders/filter/bevel.wgsl) → `BEVEL_VERT/FRAG` + `apply_bevel_raw`, double-offset highlight/shadow). **(2) fps 30→5** = le `FilterTexturePool` non borné gardait une texture par taille unique → des centaines de textures → épuisement GL → `glGenTextures` retourne 0 → NULL-deref Mesa (= **le crash Mario 63**). Fix : pool borné par **récence (TTL 2 frames)** au lieu d'un cap fixe (qui thrashait), + `make_standalone_texture` retourne `None` si `glGenTextures==0` (skip propre), + alloc sans zéroing CPU, + **budget de filtres par frame** (cap les transitions chargées), + blur cappé à 1 passe. **(3) text/perf** = font fallback embarqué (`default_font`). is_filter_supported couvre maintenant ColorMatrix/Blur/Glow/DropShadow/Bevel. **Trade-off** : sur du contenu très filtré et animé (menu Mario 63), les transitions hoquettent un peu — coût irréductible de N passes FBO/frame ; le budget + TTL le bornent. | ✓ |
| 2.4 ✓ Bump submodule Ruffle | **Validé 2026-05-25 nuit**. Submodule passé de `e41992ab` (2026-05-20) à `71280cd1` (2026-05-25), +42 commits dont fixes notables : `core: Fix looping for movie clips without End tag`, `core: Fix looping for one-frame movies`, `core: Normalize "blank" target to "_blank"`, `avm1: Do not trace an error on with(undefined)/with(null)`, AVM2 stack trace improvements. Patch Toad 2.4.a ré-appliqué proprement après bump. Issues Mario 63 spécifiques (#13198/#1909/#4690/#11077/#2448) toujours non fixées upstream — restent à diagnostiquer en jeu (peut-être que certaines sont impactées par les fixes de looping). | Faible |
| 2.4.a ✓ Toad manquant dans le château (#6906) | **Validé 2026-05-25 nuit**. Fix appliqué dans [third_party/ruffle/core/src/display_object.rs:2587](third_party/ruffle/core/src/display_object.rs#L2587) + 2614 (`hit_test_bounds` + `hit_test_shape` default impl) : return `false` quand `self.local_to_global_matrix().determinant().abs() <= f32::EPSILON`. Parity Adobe Flash Player. Patch dumpé dans [patches/0001-mario63-zero-scale-hit-test.patch](patches/0001-mario63-zero-scale-hit-test.patch) pour survivre à un futur bump du submodule. **TODO** : PR upstream Ruffle (+ tests SWF référence) — bénéficie à tout l'écosystème. | Faible |
| 2.4.bis ✓ SharedObject / URL invalide / fs::read bug | **Validé 2026-05-25 soir (hardware end-to-end, Mario 63 affiche "Continuer")**. Trois fixes empilés : (a) **URL fix** dans [rust/src/lib.rs](rust/src/lib.rs) : `file://sdmc:/...` → `http://flashforswitch.local/<basename>` (le parser URL Ruffle rejetait l'IDN "sdmc"). (b) **`SwitchStorageBackend`** dans [rust/src/backend/storage.rs](rust/src/backend/storage.rs) — port direct du `DiskStorageBackend` upstream, pointe sur **`sdmc:/ruffle/saves/`** (à côté du jeu, plus simple à backup/restore depuis Windows). Layout final : `sdmc:/ruffle/saves/<host>/<swf_path>/<savename>.sol`. (c) **Chunked 4 KB read** — debug en 3 itérations sur hardware a révélé que `std::fs::read` ET `read_to_end` retournent `OutOfMemory` sur Switch alors que le fichier existe ; root cause : le syscall `read()` newlib retourne `ENOMEM` quand appelé avec un buffer ≥ 32 KB (defaut de `read_to_end`). Workaround : lire par chunks de 4 KB dans un buffer fixe (voir [[reference-horizon-fs-quirks]]). **Bonus** : compat AMF Adobe → glisser ses `.sol` Windows sur la SD marche aussi. | Faible |
| 2.5 ✓ Performance — GL state cache + FPS heartbeat + CpuBoostMode | **Validé 2026-05-25 soir**. Trois fixes empilés : (a) `GlStateCache` ([rust/src/backend/render.rs](rust/src/backend/render.rs)) cache `last_program` / `last_texture` / `last_wrap_mode` / `last_vao` via `Cell<>` — court-circuite les `glUseProgram`/`glBindTexture`/`glBindVertexArray` redondants ; sampler `u_tex` set une fois au link. (b) **FPS heartbeat avec tick=Xms render=Yms** dans submit_frame — accumule via [rust/src/lib.rs](rust/src/lib.rs) `TICK_TICKS_ACCUM`/`RENDER_TICKS_ACCUM`, log toutes les 60 frames. Profile sur hardware a révélé que **le bottleneck est l'AVM1 interpréteur, pas notre backend GL** (tick=50ms/frame vs render=5ms/frame en scène lourde). (c) **`appletSetCpuBoostMode(ApmCpuBoostMode_FastLoad)`** dans [cpp/src/main.cpp](cpp/src/main.cpp) — bascule l'enveloppe power Tegra X1 vers le CPU au prix du GPU (qu'on n'utilise qu'à 30%). **Résultat hardware** : scène lourde 17.8 fps → 30.9 fps (+73%), tick par frame 50 ms → 27.8 ms (-44%). Parité avec PC. Pas un overclock — clocks Nintendo stock. Thread prio 0x20 testé sans gain (Switch pas loaded en autres threads), reverté à 0x2C default. | Faible |
| 2.6 ✓ File picker C++ libnx | **Validé 2026-05-25 nuit**. [cpp/src/swf_picker.cpp](cpp/src/swf_picker.cpp) — `opendir`/`readdir` côté C (newlib bypasse le bug Rust `std::fs::read_dir`). Scan dans `sdmc:/ruffle/` puis `sdmc:/switch/ruffle/` ; premier `.swf` trouvé → push à Rust via nouvelle FFI `ruffle_set_swf_path`. Rust override la candidates list. Plus besoin de renommer son SWF pour matcher un nom hardcodé. **UI sélection** (liste joycon-navigable) = Phase 3.4 (library UI ScummVM-style). | Faible |
| 2.7 ✓ `BitmapData.draw()` + `resolve_sync_handle` (moteur de tuiles SMWF) | **Livré 2026-05-29**. Super Mario World Flash est un **moteur de tuiles BitmapData** (découvert en analysant le constant pool de son DoAction racine : `tileEngine`, `map_bmp`, `copyPixels`, `tmp_tileset_bmp`) : il rastérise le tileset vectoriel dans un `BitmapData` via `draw()`, carrelle le monde via `copyPixels`, puis affiche. Avant : `render_offscreen` retournait `None` sur les handles atlas (BitmapData) → `draw()` no-op silencieux → terrain vide (« juste le ciel »). Et `resolve_sync_handle` (readback GPU→CPU dont `copyPixels` a besoin) = `Unimplemented`. **Fix** : `render_offscreen` rend les commandes de `draw()` dans une texture standalone temporaire portée par un `BitmapDataSyncHandle` ; `resolve_sync_handle` fait `glReadPixels` de la région dirty (pas de Y-flip — texel row 0 = haut Flash ; un-premultiplie l'alpha) → buffer CPU → closure Ruffle. Le terrain SMWF s'affiche. Validé hardware (« le niveau apparaît parfaitement »). Limites v1 : draw() = clear transparent (pas de composite sur l'existant), texture temp par appel (OK si draw() rare = init niveau). | ✓ |
| 2.7.bis ✓ Crash Mario 63 + verdict perf en jeu | **2026-05-29**. **Crash** = le `FilterTexturePool` (Phase 2.3) → cf. fix dans la ligne 2.3. **Perf en jeu dense** : instrumentation par-frame (`tickMax`/`rndMax`) prouve que le lag est **100% côté simulation Ruffle** (`tickMax` jusqu'à ~385 ms/frame, qui **enfle avec le temps** même immobile = fuite objets/mémoire Mario 63+Ruffle documentée) — notre rendu reste à `rndMax` ~15 ms. **Web-vérifié** : Mario 63 lague dans Ruffle même sur i7/16 GB ([issues Ruffle](https://github.com/ruffle-rs/ruffle/issues/20846), [perf Android #680](https://github.com/ruffle-rs/ruffle-android/issues/680)), Ruffle est un interpréteur sans JIT « still far from Flash Player speeds ». **Non corrigeable depuis le backend graphique.** Levier hors-code : mode dock (CPU ~1.78 vs ~1.02 GHz). | (limite Ruffle) |

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
| 3.2 ✓ `SwitchStorageBackend` (.sol persistés) | **Fait via 2.4.bis, refactorisé à plat 2026-05-26 fin de soirée (Phase 3.9)** ([rust/src/backend/storage.rs](rust/src/backend/storage.rs)). Sauvegardes `.sol` persistées **à plat** dans `sdmc:/flashnx/<basename>.<sol>.sol`, avec read-fallback sur l'ancien nested `sdmc:/ruffle/saves/<host>/<basename>/<sol>.sol` pour compat. Import depuis AppData Windows / Ruffle desktop fonctionnel (format AMF cross-platform). | Faible |
| 3.3 ✓ Menu pause in-game + éditeur TOUCHES (48 touches scrollable) | **Livré 2026-05-25 nuit → 2026-05-26 fin de soirée**. Modal custom GL natif via `SwitchRenderBackend::draw_menu_overlay` (font 5×7 pixel-art hand-encodée `GLYPHS`, backdrop semi-transparent, sélection ambre). 4 entrées : **REPRENDRE / TOUCHES / REDEMARRER / QUITTER**. REDEMARRER = `ruffle_restart()` drop+rebuild Player (cache SWF static contre OOM heap). **QUITTER = back to library** (refactor 2026-05-26 nuit, voir Phase 3.4). Plus = touche `P`. **Éditeur TOUCHES** (2e modal) : liste scrollable des boutons Switch avec binding actuel + hold-to-scroll D-pad (400ms initial, 80ms repeat), A ouvre un **dropdown scrollable** (10 visibles + scrollbar) des **48 touches Flash** : A-Z + 0-9 + Space + Enter + Escape + Shift + Control + Alt + Tab + Backspace + flèches + (aucune). Bumped 2026-05-26 fin de soirée du subset 12-keys platformer vers le clavier complet pour jeux comme Mario Forever Flash qui veulent W ou Flash Equestria qui veut A pour sauter. A confirme → save sidecar `sdmc:/flashnx/<basename>.keymap.json` + live reload via `consume_dirty` flag. State machine TOUCHES vit en Rust ([rust/src/menu.rs](rust/src/menu.rs)). Aussi accessible **pré-launch** via OPTIONS > TOUCHES dans la library (configure les touches sans booter Ruffle). | ✓ |
| 3.4 ✓ Library UI ("FlashNX launcher") + back-to-library flow | **Livré 2026-05-26 nuit + iteré jusqu'à fin de soirée** ([rust/src/library.rs](rust/src/library.rs) + [rust/src/backend/render.rs](rust/src/backend/render.rs) `draw_library_*` + [cpp/src/swf_picker.cpp](cpp/src/swf_picker.cpp) + [cpp/src/main.cpp](cpp/src/main.cpp)). C++ scan SD via `opendir`/`readdir` newlib (contourne le bug Rust `read_dir` Horizon) sur 4 candidates : `sdmc:/flashnx/` (primary) + `sdmc:/ruffle/` (legacy) + `sdmc:/switch/flashnx/` + `sdmc:/switch/ruffle/`. Push chaque path dans le Rust library state via `ruffle_library_add_path`. Rust parse l'entête SWF inline : compression (FWS/CWS/ZWS), version SWF, dims via RECT en bits MSB-first — `flate2::read::ZlibDecoder` pour les CWS (déjà dans le tree via Ruffle, binary cost ~0). **Boot flow** : `gl_context_init` → outer loop `while (!exit_nro)` { `ruffle_library_init` (alloue un `SwitchRenderBackend` standalone hors-Player) → `swf_picker_run` → `ruffle_library_open` → boucle input/render jusqu'à pick ou quit → si pick : `ruffle_set_swf_path` + `ruffle_library_shutdown` + `ruffle_init` + game loop → si **QUITTER pause menu** : `ruffle_shutdown` + `ruffle_library_reset` + re-loop ; si **−** depuis library : break outer loop → exit `.nro` }. **Back-to-library** a nécessité un refactor `OnceLock → Mutex<Option<>>` de `OVERRIDE_SWF_PATH` + `CACHED_SWF` + `ACTIVE_KEYMAP` + `ACTIVE_BASENAME` pour permettre la ré-init avec un autre game. **Layout** : banner PNG `assets/banner.png` en haut (uploadé en texture via crate `png` 0.18 + `upload_rgba_texture`), liste de 6 rows scrollables avec **hold-to-scroll** (400ms initial + 80ms repeat sur D-pad + L-stick, [cpp/src/main.cpp](cpp/src/main.cpp) `menu_repeat_step` helper) et curseur `>` animé `sin(time)`, color chip 16×16 px par jeu (HSV hash basename → couleur stable), ligne sélectionnée pulse amber↔bright-amber. Metadata panel en bas (display_name + `<size> // SWF V<version> <compression> // <WxH>` + `[basename.swf]` en petit). Fallback ASCII "FLASHNX" avec drop shadow si le banner decode échoue. **Inputs** : **A**=JOUER, **X**=OPTIONS, **Y**=DISTANT (cf. 3.7), **−**=QUITTER, Up/Down/StickL = nav. **Empty state** : instructions + Y = aller en DISTANT pour download. **Modal OPTIONS** = **TOUCHES** + **RENOMMER** (3.4.bis) + RETOUR. Remplace définitivement Phase 2.6 (file picker hardcodé). | ✓ |
| 3.4.bis ✓ RENOMMER (display name override) | **Livré 2026-05-26 fin de soirée**. Depuis OPTIONS > RENOMMER, swkbd_prompt_rename ouvre le clavier soft Switch pré-rempli avec le `display_name` actuel ([cpp/src/net.cpp](cpp/src/net.cpp) helper). Submit → écrit `sdmc:/flashnx/<basename>.meta.json` avec `{"display_name": "..."}` ([rust/src/library.rs](rust/src/library.rs) `MetaSidecar`). Champ vide = supprime le sidecar = revert au basename. Le `.swf` n'est **jamais** renommé — saves `.sol` + keymap `.keymap.json` + URL Ruffle (`http://flashforswitch.local/<basename>`) restent stables. Pattern Steam/ScummVM/iTunes. La library lit le sidecar dans `add_path` à chaque scan SD. Le metadata panel affiche toujours `[basename.swf]` en petit pour qu'on voie le vrai fichier. **TODO** : Supprimer (avec confirm, préserve `.sol`), tri, jaquettes — pas de demande user pour l'instant. | ✓ |
| 3.4.ter Forwarders home menu (doc-only, pas de code) | Pour avoir l'icône FlashNX (ou jaquette custom Mario 63) sur le **home menu Switch** à côté des vrais jeux : l'user passe par [Sphaira](https://github.com/ITotalJustice/sphaira) → "Create forwarder" → notre `.nro` → Sphaira lui demande icône custom + nom + génère le NSP. **Pas de code chez nous** — Sphaira fait tout. Doc dans README avec ⚠️ **warning ban sysNAND** explicite (les forwarders ressemblent à des jeux piratés pour Nintendo, safe seulement sur emuNAND). | Doc 30 min |
| 3.4 polish "vie" (pack léger) | Après le v1 fonctionnel, **pass de polish visuel low-cost** pour que la library ne soit pas terne : (1) **drop shadow sur le titre uniquement** (~120 draw_rect doublés, depth instant), (2) **curseur animé** `►` qui pulse couleur via `sin(time)` (~0 coût), (3) **pulsing ligne sélectionnée** modulation sin sur la couleur ambre des pixels déjà dessinés (~0 coût), (4) **banner logo FlashNX en PNG bitmap** (~720×144 px) bundlé via `include_bytes!` + décodé via `image` crate au boot + texture upload + render via shader `bitmap_prog` existant — **1 quad textured par frame au lieu de 120 draw_rect du pixel font ASCII** = gain perf ET visuel énorme (typo custom, anti-alias, gradient). Ouvre la porte à d'autres assets bitmap (splash, fond library) sans surcoût marginal. **+1-2 h pour l'asset pipeline** (décode PNG une fois au boot, garder texture vivante, alpha blending). (5) **color chip par jeu** carré 16×16 px à gauche de chaque ligne, couleur dérivée d'un hash du basename — chaque jeu a sa signature visuelle unique sans config (1 draw_rect par row, gratuit visuellement). **Skip explicite** : drop shadow sur toutes les lignes (~3000 draw_rect / frame, ROI faible), ASCII art multi-ligne (trop lourd), background gradient animé. **Note perf** : menu library est paused (Ruffle ne tick pas), donc même à ~15-30 fps c'est fluide à naviguer. Si on ressent vraiment de la latence, **batching text rendering** (~1-2h optim : tous les quads d'une string dans un seul `glBufferData`+`glDrawArrays`, ×10 moins de GL calls) est l'escape hatch — à garder en tête, pas urgent. | Faible, ~3-4 h (avec asset pipeline) |
| 3.5 ✗ Savestate — **décidé skip 2026-05-26 nuit** | Discussion + arbitrage avec user : pas d'entrée Save / Load dans le menu pause. **3 raisons** : (1) Ruffle n'a PAS de `Player::serialize()` natif (verified 2026-05-25, 0 résultat grep + 0 GitHub issue). Le Player tient un `gc_arena::Gc<>` graph non sérialisable trivialement (display list + AVM1 stack + scope + timers + audio positions). Vrai savestate = 2-3 semaines upstream Ruffle (Phase 4 optionnelle). (2) Le compromis "savestate light" (SharedObject + frame + `_root.foo` capture + injection au reload) **n'ajoute pas de valeur** : pour Mario 63 le `.sol` capture déjà tout ce que le jeu sait restaurer (checkpoints, étoiles), et frame + `_root.*` ne suffit pas à reconstruire un mid-jump/mid-cutscene — donc "Save" ≈ ce que le `.sol` fait déjà, "Load" ≈ REDEMARRER qu'on a déjà. (3) Variante slots multiples (`.sol` dupliqués en `.slot1.sol` etc.) — granularité reste celle du jeu (Mario 63 = points de save fixes), ROI faible. **Conclusion** : skip dans le scope Phase 3, attendre vrai savestate upstream Ruffle (Phase 4 optionnel) si la demande remonte. Le menu pause reste à 4 entrées (Reprendre / Touches / Redemarrer / Quitter). | (skip) |
| 3.6 ◐ Compat globale autres SWF AS1/AS2 | **En cours**. Déjà validés sur hardware : Mario 63, **Super Mario World Flash (de bout en bout — a forcé l'implémentation BitmapData.draw + masquage INCR/DECR, cf. Phase 2.7)**, Mario Forever Flash, Tetris'd, Flappy Bird, Flash Equestria, There Is Only One Level, Mario 3D Racing. Chaque jeu expose ses propres exigences Ruffle/backend (SMWF = tile engine BitmapData ; Mario 63 = filtres Bevel + limite perf interpréteur). Reste à tester : Madness, Newgrounds classics. Reuse de la diag infra (exception handler natif, crash log, heartbeat avec compteurs live `offscreen`/`sync`/`fpool`/`tickMax`/`rndMax`). | Variable |
| 3.7 ✓ Import distant (LOCAL ↔ DISTANT, archive.org) + URL history persisté | **Livré 2026-05-26 nuit, itéré jusqu'à fin de soirée** ([rust/src/net.rs](rust/src/net.rs) + [cpp/src/net.cpp](cpp/src/net.cpp) + extensions de [rust/src/library.rs](rust/src/library.rs)). **Stack** : libcurl 7.69 + mbedtls 2.28 (statiques via `switch-curl` + `switch-mbedtls` pacman devkitPro), CA bundle Mozilla `assets/cacert.pem` (228 KB) embarqué via `include_bytes!` côté Rust, écrit à `sdmc:/switch/flash-for-switch/cacert.pem` au 1er boot (idempotent). libcurl 7.69 n'a pas `CURLOPT_CAINFO_BLOB` (ajouté en 7.77) donc on passe par un path SD via `CURLOPT_CAINFO`. **Flow** : depuis LOCAL list ou empty state, **Y** = entrer en DISTANT. Modal DistantIdle : si historique vide → "A: saisir URL" ; si historique non vide → affiche l'URL courante avec badge `[N / total]`, **L/R** cyclent dans l'historique, **ZR** = re-fetch direct l'URL affichée sans rouvrir le clavier, **A** ouvre swkbd pré-rempli avec l'URL courante (édition rapide d'un voisin item-id). Historique persisté à `sdmc:/switch/flash-for-switch/distant_history.json` (~20 entrées max, LRU, dedup). On extrait l'item-id de l'URL (accepte `https://archive.org/details/<id>`, `https://archive.org/download/<id>[/<file>]`, ou `<id>` bare). HTTPS GET sync sur `https://archive.org/metadata/<item-id>` (~1-3 s) → JSON parsé via `serde_json`. On filtre `format == "Shockwave Flash"`. **Liste des fichiers distants** : badge **`OK`** vert à côté des fichiers déjà sur SD (union de session-downloaded + entries scannées au boot). **A sur un OK = NO-OP silencieux** (pas de re-DL, pas de launch surprise — l'user bascule en LOCAL avec Y pour jouer le jeu déjà téléchargé). **A sur un non-OK** → download asynchrone via curl **multi handle** (`https_download_start` + `https_download_tick` poll-once-per-frame) qui ne bloque pas la UI, progress bar live. À la fin du DL, l'entrée est auto-pushée dans `entries` (`add_or_replace_path`) et **on reste sur DistantFiles** avec le `OK` qui apparaît à côté du fichier (et pas auto-bascule en LOCAL — l'user peut DL plusieurs fichiers d'affilée du même item sans retaper l'URL). **B/Y depuis DistantFiles** → retour DistantIdle. **B annule** en cours de DL. **Erreurs** propres : URL invalide, HTTPS échec, JSON cassé, item sans `.swf` → DistantError screen avec message + A/B = retour DistantIdle. **.nro size delta** : +1.4 MB (libcurl ~750 KB + mbedtls ~500 KB + cacert.pem 228 KB + net.cpp + nouveau Rust code). **Hors scope** : URLs génériques non-archive.org, multi-file checkboxes. | ✓ |
| 3.8 ✓ Stage rendering forcé (scale + align + letterbox) | **Livré 2026-05-26 fin de soirée**. PlayerBuilder construit avec : `with_scale_mode(StageScaleMode::ShowAll, force=true)` + `with_align(StageAlign::empty(), force=true)` + `with_letterbox(Letterbox::On)`. **Why** : observed sur Super Mario World Flash (480×320) et Flappy Bird (500×700), les SWF font `Stage.scaleMode = "noScale"` via AS pour avoir un layout responsive — résultat sur notre viewport 1280×720 : mini-rectangle dans le coin. `ShowAll` force le scale-to-fit avec préservation du ratio. `StageAlign::empty()` centre horizontalement (Mario Forever Flash mettait `Stage.align = "L"`). `Letterbox::On` clip + dessine les bandes noires latérales (Flappy Bird avait du contenu off-stage qui leak). `force=true` bloque le SWF de remettre ses valeurs en AS. **Trade-off** : SWF qui implémentent leur propre responsive layout via NoScale tournent maintenant en taille fixe letterboxée — acceptable pour une console portable. | ✓ |
| 3.9 ✓ Saves layout à plat + rename `ruffle/` → `flashnx/` | **Livré 2026-05-26 fin de soirée**. **Saves** ([rust/src/backend/storage.rs](rust/src/backend/storage.rs)) : nouveau path `sdmc:/flashnx/<basename>.<sol_name>.sol` (à plat, sidecar-style), au lieu du legacy nested `sdmc:/ruffle/saves/<host>/<basename>/<sol_name>.sol`. `SwitchStorageBackend::new(flat_root, legacy_root)` lit en priorité le flat path, fallback sur le nested si absent (compat). Writes uniquement vers le flat path — auto-migration on next write. `remove_key` nettoie les deux. **Root SD renommé** `sdmc:/ruffle/` → **`sdmc:/flashnx/`** pour matcher la marque ; scan order [flashnx, ruffle, switch/flashnx, switch/ruffle]. Sidecars (`.keymap.json` + `.meta.json` + `keymap_default.json`) : helpers `find_user_path` (read avec fallback) + `primary_path` (write vers flashnx). Mkdir `sdmc:/flashnx/` au boot. | ✓ |

### Phase 4 — polish + distribution

- ✓ **`.nacp` + icône custom embarqués 2026-05-26 nuit** : `cpp/Makefile` wired avec `APP_TITLE = "FlashNX"`, `APP_AUTHOR = "flash-for-switch contributors"`, `APP_ICON = $(TOPDIR)/../assets/icon.jpg`, `NROFLAGS += --nacp=... --icon=...`. hbmenu / Sphaira affichent désormais le bon titre + icône.
- ✓ **Font fallback embarqué** (livré 2026-05-29) : feature `default_font` de `ruffle_core` activée dans [rust/Cargo.toml](rust/Cargo.toml) → Ruffle embarque Noto Sans comme fallback des device fonts (`verdana`, `Arial`, etc.). Plus de `Fallback font not found` ni de texte HTML invisible. Observed initialement 2026-05-26 sur Super Mario World Flash + Mario Forever Flash, fixé depuis.
- Packaging hb-app.store officiel (reste à faire)
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
- Phase 2.3 filtres Glow/DropShadow/Blur/ColorMatrix/Bevel : **✓ livré 2026-05-29** (3e tentative). Les 3 bugs hardware de la 2e tentative diagnostiqués + corrigés : boutons invisibles = masquage cassé + Bevel manquant (les deux fixés) ; fps 30→5 = `FilterTexturePool` non borné → épuisement GL → crash Mario 63 (fix : pool TTL + garde glGen + budget/frame) ; text = font fallback. Bonus session : `BitmapData.draw()` (Phase 2.7) qui débloque Super Mario World Flash.
- Phase 2.4 bump submodule : **fait en ~5 min** après désac temporaire Avast HTTPS scan. Submodule passé de `e41992ab` à `71280cd1` (+42 commits). Patch Toad 2.4.a ré-appliqué proprement via `git apply ../../patches/*.patch`. Rebuild Rust complet 6 min, .nro final 12.25 MB.
- **Phase 3.1 cycle applet** : ~~estimation ½ j~~ → **0 jour, déjà OK** (validé empiriquement 2026-05-25 nuit, home-button + mise en veille marchent sans crash via `appletMainLoop` libnx implicit)
- **Phase 3.3 menu pause + éditeur TOUCHES** : ~~estimation 2-3 j (avec FreeType + libnx pl)~~ → **fait en ~5 h total sur 2 nuits** (25 nuit modal + REDEMARRER, 26 nuit éditeur TOUCHES live + keymap JSON system). Font pixel-art 5×7 hand-encodée suffisante (pas besoin FreeType). Save state / Load state / Back to library = dépendent de 3.5 / 3.4 respectivement, pas vraiment du scope 3.3.
- **Phase 3.5 savestate** : ~~estimation 1 j (light)~~ → **skip décidé 2026-05-26 nuit** (cf. ligne 3.5 du tableau Phase 3 ci-dessus). Pas de Save/Load dans le menu — light savestate = pas mieux que ce que le `.sol` + REDEMARRER font déjà, vrai savestate = chantier upstream Ruffle 2-3 semaines (Phase 4 optionnelle, attendre demande).
- **Phase 3 restant (library uniquement)** : estimation **2-3 jours** (3.4 library scan SD multi-SWFs + UI joycon). Tout le reste Phase 3 = ✓ ou ✗-skip-justifié.
- Release publique propre v1 (Phase 4) : **+2-3 semaines**
- Savestate "vrai" (upstream contribution Ruffle) : **+2 semaines** optionnel post-v1

La sous-estimation initiale (6-12 mois) venait surtout de la peur autour de `std-via-newlib` (Phase 1.1) qui s'est résolue en 1h au lieu de 1-2 semaines, car upstream stdlib avait déjà les branches `target_os = "horizon"` pour le 3DS. La même surprise s'est répétée Phase 2.4.bis : on imaginait un effort `StorageBackend` complexe, en fait l'API Ruffle est minimale (3 méthodes) et un `DiskStorageBackend` est directement réutilisable.

## MANQUANT / limitations connues (état 2026-05-29)

Pour **jouer des SWF AS1/AS2 courants, c'est fonctionnellement quasi complet**. Voici l'inventaire honnête de ce qui reste, par catégorie et impact réel.

### A. Lacunes de rendu (backend) — c'est là que mordront les prochains jeux

| Manque | Impact | Fréquence |
|---|---|---|
| **`render_alpha_mask`** complètement skippé (`warn_once` + return) | Les **masques alpha/luminance doux** (≠ masques vectoriels stencil qu'on gère) font **disparaître leur contenu masqué** | Moyen — prochain « élément/écran vide » mystère viendra probablement de là |
| **Blend modes réels** — `blend()` inline juste les commandes en Normal | Multiply / Screen / Overlay / Add / Difference… rendus comme Normal → couleurs fausses sur effets de lumière/ombre | Moyen |
| **Filtres restants** : GradientGlow, GradientBevel, Convolution, DisplacementMap | Droppés (passthrough). Faits : ColorMatrix / Blur / Glow / DropShadow / **Bevel** | Faible-moyen |
| **`BitmapData.draw()` v1 incomplet** : clear-transparent (pas de composite sur l'existant), texture temp par appel (pas de pool), affichage-après-draw-sans-lecture-CPU = périmé | OK pour le pattern tile-engine (SMWF), pas fidèle pour tous les usages BitmapData (effets dynamiques, captures in-game) | Faible |
| **Perf des filtres en transitions menu** (N passes FBO/frame) | Hoquets sur menus très filtrés/animés (Mario 63). Bornés par budget/frame + pool TTL mais pas éliminés. Vrai fix = batching des passes | Visible sur Mario 63 |
| **Context3D / Stage3D**, **PixelBender** (AS3) | Non implémentés (`render_stage3d`/pixelbender = stubs) | Quasi nul (Flash 2D AS1/2) |

### B. Compat / cœur Ruffle (souvent hors de notre portée)

- **Perf Mario 63 en jeu dense** — limite de l'**interpréteur Ruffle** (sim `tick` jusqu'à ~385 ms/frame, qui enfle avec le temps = fuite objets/mémoire Mario 63+Ruffle documentée). Notre rendu reste ~5-15 ms/frame. **Non corrigeable depuis le backend** (web-vérifié : Ruffle lague même sur i7). Levier hors-code : mode dock.
- **Compat globale** (Phase 3.6) — chaque nouveau SWF exposera ses propres écarts Ruffle/backend. Travail continu, pas un « truc à finir ».
- **AS3 / AVM2** — supporté par Ruffle mais non testé chez nous ; perf pire (pas de JIT).

### C. Plateforme / distribution (la « Phase 4 »)

- **Savestate** — sciemment skippé (cf. Phase 3.5) ; vrai savestate = ~2 semaines upstream Ruffle.
- **Library** : Supprimer un jeu (avec confirm, préserve `.sol`), tri, jaquettes — pas demandés pour l'instant.
- **Packaging hb-app.store** + **doc utilisateur** (import saves, mappings, troubleshooting) — pour une release publique.
- **Forwarders home menu** — doc-only (Sphaira fait tout, pas de code).

### D. Nettoyage / dette technique

- **Instrumentation de debug** laissée dans le heartbeat (`pushmask`/`amask`/`maskeddraw`/`maskshape`/`fpool`/`tickMax`/`rndMax`/`cacheMax`) + bindings FFI `glGetFramebufferAttachmentParameteriv`/`GL_STENCIL`/`GL_FRAMEBUFFER_ATTACHMENT_STENCIL_SIZE` devenus inutilisés (self-test stencil retiré). À nettoyer pour une release, ou à garder (cohérent avec le style « heartbeat instrumenté » du projet).

**TL;DR** : les deux manques de rendu les plus susceptibles de casser un nouveau jeu = **`render_alpha_mask`** et **les blend modes**. Le reste de (A) est rare, (B) est Ruffle, (C) est de la distribution.

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
