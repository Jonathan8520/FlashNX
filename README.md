# FlashNX

**Lecteur Flash homebrew pour Nintendo Switch.** Fais tourner tes jeux Flash (`.swf` — AS1/AS2, et une partie de l'AS3) directement depuis la carte SD. Propulsé par **[Ruffle](https://github.com/ruffle-rs/ruffle)**.

> Le repo, la toolchain et le crate Rust gardent le nom historique `flash-for-switch` ; **FlashNX** est le nom de l'application (ce que voient hbmenu et le `.nro`).

## Installation

1. Télécharge **`FlashNX.nro`** depuis les [Releases](https://github.com/Jonathan8520/flash-for-switch/releases).
2. Copie-le dans **`sdmc:/switch/`**.
3. Copie tes fichiers **`.swf`** dans **`sdmc:/flashnx/`**.
4. Lance **FlashNX** depuis le Homebrew Menu.

*(Switch moddée Atmosphère requise.)*

## Utilisation

**Dans la library**
Haut/bas (D-pad ou sticks) = naviguer · **A** = jouer · **X** = options (touches, renommer) · **Y** = importer depuis archive.org · **−** = quitter.

**En jeu**
Stick gauche / D-pad = flèches · **A/B/X/Y** = touches Flash (remappables) · stick droit = curseur souris · **ZR** / tactile = clic · **−** = menu pause.

- **Sauvegardes automatiques** pour les jeux qui sauvegardent (SharedObject `.sol`), sur la SD.
- **Éditeur de touches** intégré (48 touches Flash, réglable par jeu).

## Jeux testés

Super Mario 63 · Super Mario World Flash · Mario Forever · Tetris'd · Flappy Bird · Pursuit of Hat 2 · Mario 3D Racing… La plupart tournent à 55-60 fps.

## Limites connues

- **Jeux lourds** : les chutes de framerate viennent de l'**interpréteur AVM1/AVM2 de Ruffle** (CPU-bound, pas de JIT) — pas de notre rendu. Levier hors-application : overclock CPU (sys-clk).
- **Compatibilité AS3** : partielle, héritée de Ruffle (voir [compatibilité Ruffle](https://ruffle.rs/compatibility)). Les jeux AS3 affichent un badge dans la library.
- **Pas de savestate / rewind** : Ruffle n'expose pas de snapshot de l'état d'exécution. Les sauvegardes natives `.sol` des jeux fonctionnent.
- **Audio** : léger crackle occasionnel sur les scènes *très* denses (à affiner).

## Crédits & licences

FlashNX n'est qu'une **couche d'intégration Switch** au-dessus de projets remarquables — tout le mérite de l'émulation Flash revient à **Ruffle**.

**Cœur**
- **[Ruffle](https://github.com/ruffle-rs/ruffle)** — *Apache-2.0 / MIT* — moteur Flash (parsing SWF + interpréteur AVM1/AVM2). FlashNX intègre `ruffle_core`, `ruffle_render`, `swf`.
- **[devkitPro](https://devkitpro.org/) / libnx** — *ISC* — toolchain Switch + API Horizon (audio **audren**, input **hid**, threads, exception handler).
- **[switch-mesa](https://github.com/devkitPro/pacman-packages)** — *MIT* — OpenGL/GLES via Nouveau, le backend graphique.

**Réseau (import archive.org en HTTPS)**
- [libcurl](https://curl.se/) · [Mbed-TLS](https://github.com/Mbed-TLS/mbedtls) · [zlib](https://zlib.net/) · bundle de certificats CA [Mozilla](https://curl.se/docs/caextract.html).

**Bibliothèques Rust**
- [jpeg-decoder](https://github.com/image-rs/jpeg-decoder) (fork patché pour newlib) · `png` · `serde` / `serde_json` · `tracing` · `flate2` · `getrandom` · + dépendances transitives de Ruffle (`gc-arena`, `dasp`…).

**Remerciements** — projets de l'écosystème homebrew Switch consultés pendant le R&D du port (sans code repris) : [ScummVM](https://www.scummvm.org/) (pattern de port `OSystem`), le port Switch de PPSSPP (référence switch-mesa GL), [Tico](https://github.com/ticohq/tico), [dawn-switch](https://github.com/dantiicu/dawn-switch) *(l'alternative WebGPU évaluée)*, et la communauté devkitPro / GBAtemp.

**Licence** : le code d'intégration FlashNX est distribué sous licence **MIT** (voir [`LICENSE`](LICENSE)). Ruffle et les autres dépendances conservent leurs licences respectives.

---

📖 **Architecture, build, roadmap & notes techniques** → [DEVELOPMENT.md](DEVELOPMENT.md)
📦 **Changelog** → [CHANGELOG.md](CHANGELOG.md)
