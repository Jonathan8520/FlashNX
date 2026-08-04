<p align="center">
  <img src="assets/banner.png" alt="FlashNX" width="480">
</p>

<p align="center">
  <strong>Homebrew Flash player for Nintendo Switch.</strong><br>
  Run your Flash games (<code>.swf</code> — AS1/AS2, and part of AS3) straight from the SD card.<br>
  Powered by <a href="https://github.com/ruffle-rs/ruffle">Ruffle</a>.
</p>

<p align="center">
  <a href="https://hb-app.store/switch/FlashNX"><img src="https://img.shields.io/badge/Homebrew%20App%20Store-FlashNX-2ea44f" alt="Homebrew App Store"></a>
  <a href="https://github.com/Jonathan8520/FlashNX/releases"><img src="https://img.shields.io/github/v/release/Jonathan8520/FlashNX?label=release" alt="Release"></a>
  <img src="https://img.shields.io/badge/platform-Nintendo%20Switch-e60012" alt="Platform">
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="License">
</p>

> The repo, the toolchain and the Rust crate keep the historical name `flash-for-switch`; **FlashNX** is the application name (what hbmenu and the `.nro` show).

## Overview

| Play | Flashpoint | In game | Settings |
|:---:|:---:|:---:|:---:|
| ![FlashNX library](assets/screenshots/library.png) | ![Flashpoint download](assets/screenshots/flashpoint.png) | ![In game + pause menu](assets/screenshots/in-game.png) | ![Settings](assets/screenshots/settings.png) |
| Cover gallery, L/R tabs | Download games from Flashpoint | Game + pause menu / key editor | Language, controls, bug report |

## Installation

**Easiest — via the [Homebrew App Store](https://hb-app.store/switch/FlashNX):** open the **hb-appstore** app on your Switch, search for **FlashNX**, install. Updates follow automatically.

**Or manually:**
1. Download **`FlashNX.nro`** from the [Releases](https://github.com/Jonathan8520/FlashNX/releases).
2. Copy it to **`sdmc:/switch/FlashNX/FlashNX.nro`** (the Homebrew Menu also accepts a loose `sdmc:/switch/FlashNX.nro`).

Either way: copy your **`.swf`** files into **`sdmc:/flashnx/`**, then launch **FlashNX** from the Homebrew Menu.

*(Modded Switch with Atmosphère required.)*

## Usage

**In the library**
**L / R** = switch tabs (Play / Import / Settings) · D-pad or sticks = move in the cover gallery · **A** = play · **Y** = sort (name / added / played / size, **X** reverses) · **−** = search (filter by name) · **+** = game options (favorite, controls, rename, cover, delete). Favorites stay pinned to the top of the gallery, and the size of your library is shown under the logo. Quit, plus bug report and suggestions, from the Settings tab.

**Import tab**
Your saved URLs, each row showing a readable name, a tag for what it is (`SWF` for a single file, `LIST` for a multi-file source) and how many of that source's files are already on your SD card. **A** = launch or download · **+ Add a URL** stays pinned at the top of the list · **−** = search · **Y** = sort (added, name, source, file count) · **+** on a URL = its details (type, files on the card, date you added it, full URL) plus edit, favorite and delete. A favorited URL pins to the top like a favorited game, and the list scrolls smoothly or can be dragged with a finger. Accepts archive.org items and direct `.swf` URLs. **X** searches the **Flashpoint Archive** and downloads a game (cover and real title included; **+** on a result shows its details + download size).

**In game**
Left stick / D-pad = arrows · **A/B/X/Y** = Flash keys (remappable) · right stick = mouse cursor · **ZR** / touch = left click, **ZL** = right click · clicking a text field opens the keyboard · **−** = pause menu. The control editor picks any Flash key (letters, digits, arrows, F1-F12, symbols, numpad) or mouse click from a visual keyboard, and can turn the right stick into a d-pad (it also covers SL/SR and the stick presses). For games that need more inputs than the pad has buttons, a **combo layer** lets you hold one shoulder button (the modifier) so every other button sends a second key, e.g. `ZL + A` sends `F1`. In the editor, **L/R** move a sub-tab across `NORMAL / ZL / ZR / L / R`: NORMAL edits the base bindings, and picking a modifier (ZL/ZR/L/R) edits that player's combo layer (rows then read `ZL+A`, etc.). **X** switches the Player 1 / Player 2 tab (each has its own combo layer).

- **Cover art**: the library is a grid of covers, 5 per row (cropped to fill). Drop a `<game>.png` or `.jpg` next to the `.swf` to set one, or fetch artwork from the Flashpoint Archive in a game's options (pick from thumbnails).
- **Multi-file games**: some games load companion `.swf` files (a title screen, a minigame, music). When you download a game from the **Flashpoint search (X)**, FlashNX fetches these companions automatically into a `<game>.files/` folder. For a game added another way (URL import, hand-copied), put the companions in that folder yourself (so `Foo.swf` uses `Foo.files/`, and `Foo.swf` loading `top.swf` reads `Foo.files/top.swf`).
- **Games packaged as a web page**: a few Flashpoint entries do not ship a plain `.swf` but an `index.html` that feeds settings to the player. FlashNX reads that page's configuration itself and runs the game, which covers Disney/Yamago minigames (*Agent P Strikes Back*, *Tron Uprising: Escape from Argon City*) and HTML-wrapped titles such as *Dragon City*.
- **Home-menu shortcuts**: launch FlashNX with a `.swf` path as its argument and it boots straight into that game (and returns to the Home menu when you quit). With a forwarder tool you can put a single Flash game on your Switch Home menu, with its cover as the icon. **Sphaira** users: FlashNX registers a `.swf` association, so you can select a game in Sphaira's file browser and "Create a Forwarder" → FlashNX.
- **Automatic saves** for games that save (SharedObject `.sol`), on the SD card.
- **Built-in key editor** (88 Flash keys plus the mouse clicks, configurable per game), plus a **global default** layout in the Settings tab.
- **Languages**: English, French, Spanish, Russian, German, Italian, Portuguese, Chinese (Simplified, rendered from the console's own shared font), auto-detected from the console's system language, switchable from the Settings tab. Chinese needs the memory a HOME-menu entry gets: started from the Album, FlashNX falls back to English and the language list says why.

## Tested games

Super Mario 63 · Super Mario World Flash · Mario Forever · Tetris'd · Flappy Bird · Pursuit of Hat 2 · Mario 3D Racing · Icy Tower · Papa Louie 2 and 3 · Newgrounds Rumble… Most run at 55-60 fps. Heavier AS3 titles (*Cat Mario*, *Agent P Strikes Back*, *Dragon City*) do run, but well below that, for the reason just below.

## Known limitations

- **Heavy games**: frame-rate drops come from **Ruffle's AVM1/AVM2 interpreter** (CPU-bound, no JIT) — not from our rendering. Out-of-app lever: CPU overclock (sys-clk).
- **AS3 compatibility**: partial, inherited from Ruffle (see [Ruffle compatibility](https://ruffle.rs/compatibility)). AS3 games show a badge in the library.
- **No savestate / rewind**: Ruffle does not expose a snapshot of the execution state. Games' native `.sol` saves do work.
- **Audio**: occasional light crackle on *very* dense scenes (to be refined).

## Credits & licenses

FlashNX is only a **Switch integration layer** on top of remarkable projects — all credit for the Flash emulation goes to **Ruffle**.

**Core**
- **[Ruffle](https://github.com/ruffle-rs/ruffle)** — *Apache-2.0 / MIT* — Flash engine (SWF parsing + AVM1/AVM2 interpreter). FlashNX embeds `ruffle_core`, `ruffle_render`, `swf`.
- **[devkitPro](https://devkitpro.org/) / libnx** — *ISC* — Switch toolchain + Horizon API (audio **audren**, input **hid**, threads, exception handler).
- **[switch-mesa](https://github.com/devkitPro/pacman-packages)** — *MIT* — OpenGL/GLES via Nouveau, the graphics backend.

**Game & cover data**
- **[Flashpoint Archive](https://flashpointarchive.org/)**: the Flash game preservation project. FlashNX uses its public APIs (search, logos, GameZIPs) so you can find cover art and download the games you choose. All preserved games stay the property of their original creators, and FlashNX only fetches what you explicitly request.

**Network (archive.org import over HTTPS)**
- [libcurl](https://curl.se/) · [Mbed-TLS](https://github.com/Mbed-TLS/mbedtls) · [zlib](https://zlib.net/) · [Mozilla](https://curl.se/docs/caextract.html) CA certificate bundle.

**Rust libraries**
- [jpeg-decoder](https://github.com/image-rs/jpeg-decoder) (fork patched for newlib) · `png` · `serde` / `serde_json` · `tracing` · `flate2` · `getrandom` · + Ruffle's transitive dependencies (`gc-arena`, `dasp`…).

**Acknowledgements** — Switch homebrew ecosystem projects consulted during the port's R&D (no code reused): [ScummVM](https://www.scummvm.org/) (`OSystem` port pattern), the PPSSPP Switch port (switch-mesa GL reference), [Tico](https://github.com/ticohq/tico), [dawn-switch](https://github.com/dantiicu/dawn-switch) *(the WebGPU alternative we evaluated)*, and the devkitPro / GBAtemp community.

**License**: the FlashNX integration code is distributed under the **MIT** license (see [`LICENSE`](LICENSE)). Ruffle and the other dependencies keep their respective licenses.

---

📖 **Architecture, build & technical notes** → [DEVELOPMENT.md](DEVELOPMENT.md)
📦 **Changelog** → [CHANGELOG.md](CHANGELOG.md)
