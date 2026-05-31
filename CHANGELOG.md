# Changelog — FlashNX

Lecteur Flash homebrew pour Nintendo Switch (`.nro`), propulsé par [Ruffle](https://github.com/ruffle-rs/ruffle).

## v1.0.0 — 2026-05-31

Première release officielle. FlashNX fait tourner des jeux Flash AS1/AS2 (et une partie de l'AS3) directement depuis la carte SD de ta Switch.

### Fonctionnalités

- **Lecteur Flash complet** : core Ruffle (parsing SWF + interpréteur AVM1/AVM2) branché sur un stack natif Switch — rendu OpenGL (switch-mesa), audio (audren), input joycon + souris (stick droit / écran tactile).
- **Library FlashNX** : interface navigable au joycon, bannière + vignette par jeu, badge `AS3` pour les jeux AVM2, renommage des jeux (sans toucher au fichier), détection auto des `.swf` sur SD.
- **Import distant archive.org** : télécharge des `.swf` par HTTPS directement depuis la Switch (clavier soft, historique d'URLs, barre de progression).
- **Éditeur de touches in-game** : remappe les 48 touches Flash supportées par jeu, depuis le menu pause ou la library.
- **Sauvegardes natives** : les jeux qui sauvegardent via `SharedObject` (`.sol`) conservent ta progression sur la SD.
- **Robustesse** : mega-arena GL anti-fragmentation, gestion des bitmaps > 2048 px, handler d'exception natif avec log de crash symbolisable. Le `.nro` ne refuse jamais de booter (fallback intégré).

### Jeux testés sur hardware

Super Mario 63, Super Mario World Flash, Mario Forever Flash, Tetris'd, Flappy Bird, There Is Only One Level, Mario 3D Racing, Pursuit of Hat 2, et d'autres. La plupart tournent à 55-60 fps.

### Limites connues (assumées)

- **Performance des jeux lourds** : sur certains titres (Mario 63 en scène dense, jeux AS3 complexes comme Pursuit of Hat 2), les chutes de framerate viennent de l'**interpréteur AVM2/AVM1 de Ruffle** (CPU-bound, pas de JIT), pas du rendu — non corrigeable depuis le backend. Mesuré : notre rendu reste à ~5 ms/frame pendant que la logique de jeu peut prendre >1 s sur une frame. Levier hors-app : overclock CPU (sys-clk).
- **Compatibilité AS3/AVM2 partielle** : héritée du moteur Ruffle upstream (voir [ruffle.rs/compatibility](https://ruffle.rs/compatibility)). Les jeux affichant un badge `AS3` dans la library ont un support variable.
- **Pas de savestate / rewind** : Ruffle n'expose pas de snapshot de l'état d'exécution (l'état est un graphe d'objets `gc-arena`, non sérialisable). Les sauvegardes natives des jeux (`.sol`) fonctionnent.
- **Audio** : le son est désormais soft-limité (fort, sans écrêtage dur). Sur les scènes **très** denses (Mario 63), un léger crackle occasionnel peut subsister (compression des pics) — à affiner dans une mise à jour.

### Installation

1. Copie `FlashNX.nro` dans `sdmc:/switch/` (ou `sdmc:/switch/FlashNX/`).
2. Copie tes `.swf` dans `sdmc:/flashnx/`.
3. Lance FlashNX depuis le Homebrew Menu.

### Crédits

- **Auteur** : Jonathan8520
- **Propulsé par Ruffle** (Apache-2.0 / MIT) — le core d'émulation Flash.
- Stack natif Switch via devkitPro / libnx / switch-mesa.
