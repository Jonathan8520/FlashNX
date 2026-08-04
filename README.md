<p align="center">
  <img src="assets/banner.png" alt="FlashNX" width="480">
</p>

<p align="center">
  <strong>Homebrew Flash player for Nintendo Switch.</strong><br>
  Run your Flash games (<code>.swf</code>, AS1/AS2 and part of AS3) straight from the SD card.<br>
  Powered by <a href="https://github.com/ruffle-rs/ruffle">Ruffle</a>.
</p>

<p align="center">
  <a href="https://hb-app.store/switch/FlashNX"><img src="https://img.shields.io/badge/Homebrew%20App%20Store-FlashNX-2ea44f" alt="Homebrew App Store"></a>
  <a href="https://github.com/Jonathan8520/FlashNX/releases"><img src="https://img.shields.io/github/v/release/Jonathan8520/FlashNX?label=release" alt="Release"></a>
  <img src="https://img.shields.io/badge/platform-Nintendo%20Switch-e60012" alt="Platform">
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="License">
</p>

| Play | Flashpoint | In game | Settings |
|:---:|:---:|:---:|:---:|
| ![FlashNX library](assets/screenshots/library.png) | ![Flashpoint download](assets/screenshots/flashpoint.png) | ![In game + pause menu](assets/screenshots/in-game.png) | ![Settings](assets/screenshots/settings.png) |
| Cover gallery, L/R tabs | Download games from Flashpoint | Game + pause menu / key editor | Language, controls, bug report |

## Installation

**Easiest, via the [Homebrew App Store](https://hb-app.store/switch/FlashNX):** open **hb-appstore** on your Switch, search for **FlashNX**, install. Updates follow automatically.

**Or manually:** download **`FlashNX.nro`** from the [Releases](https://github.com/Jonathan8520/FlashNX/releases) and copy it to **`sdmc:/switch/FlashNX/FlashNX.nro`**. *(Modded Switch with Atmosphère required.)*

### Launching it, the part that matters

**Hold R while opening a game from the Home menu**, then pick FlashNX in the Homebrew Menu.

Opened the usual way from the Album, homebrew only gets a small share of the console's memory, which is not enough to run a Flash game: FlashNX starts but refuses to launch anything and tells you why. Holding R gives it the whole console instead. A forwarder does the same, and **[Sphaira](https://github.com/ITotalJustice/sphaira)** users can go further: FlashNX registers a `.swf` association, so you can pick a game in Sphaira's file browser and create a Home menu shortcut that boots straight into it, cover art as the icon.

## Getting games

**You do not need a PC.** In the **Import** tab, press **X** to search the **[Flashpoint Archive](https://flashpointarchive.org/)**, the Flash preservation project, and download a game straight to your console. Its real title and cover art come along, multi-file games bring their companion files, and **+** on a result shows the details and the download size before you commit.

The same tab also imports from a URL: a direct `.swf`, an archive.org item, or a Wayback Machine snapshot. And you can still drop your own `.swf` files into **`sdmc:/flashnx/`** from a computer.

## Controls

**In the library**

| | |
|---|---|
| **L / R** | Switch tabs (Play / Import / Settings) |
| D-pad or sticks | Move in the cover gallery |
| **A** | Play |
| **Y** | Sort by name, date added, last played, most played or size (**X** reverses) |
| **−** | Search by name |
| **+** | Game options: favorite, controls, rename, cover, delete |

Your library size is shown under the logo. Quit, bug report and suggestions live in the Settings tab.

**Import tab**

| | |
|---|---|
| **A** | Launch or download the selected entry |
| **X** | Search the **Flashpoint Archive** and download a game |
| **−** | Search your saved URLs |
| **Y** | Sort by date added, name, source or file count |
| **+** | Entry details, plus edit, favorite and delete |

Each row shows a readable name, a `SWF` or `LIST` tag, and how many of that source's files are already on your card. Accepts archive.org items and direct `.swf` URLs. The list scrolls smoothly and can be dragged with a finger.

**In game**

| | |
|---|---|
| Left stick or D-pad | Arrow keys |
| **A B X Y** | Flash keys (remappable) |
| Right stick | Mouse cursor |
| **ZR** or touch | Left click |
| **ZL** | Right click |
| **−** | Pause menu |

The control editor binds any of 88 Flash keys (letters, digits, arrows, F1-F12, symbols, numpad) or a mouse click from a visual keyboard, per game, with a global default in Settings. It also covers SL/SR, the stick presses, and turning the right stick into a d-pad. Clicking a text field in a game opens the Switch keyboard.

For games needing more inputs than the pad has buttons, a **combo layer** lets you hold a shoulder button so every other button sends a second key (`ZL + A` sends `F1`). The four modifiers are independent.

Plug in a second controller and you get **two players**, each with their own bindings, for Flash games built around two people sharing one keyboard. Press **X** in the editor to switch between the Player 1 and Player 2 tabs.

## Features

**Your library**

- **Cover art**: a grid of covers, 5 per row. Drop a `<game>.png` or `.jpg` next to the `.swf`, or fetch artwork from the Flashpoint Archive in a game's options and pick from thumbnails. Games without one get a generated tile.
- **Favorites** pinned to the top, **playtime** tracked per game, and sorting by name, date added, last played, most played or size.
- **Rename** a game for display without touching the file, and deleting one cleans up everything it left on the card.
- **Touch**: in handheld, drag the gallery to scroll, tap a game to select it, tap again to launch.

**Controls**

- **Community control profiles**: share your bindings for a game and download what other players have shared for it. Applying a profile is non-destructive and can be reverted, and verified or most-applied ones come first. Your combo layers and cursor settings travel with it.
- **Cursor**: adjustable speed (x0.5 to x2.5, cycling live from the pause menu) and a per-game show or hide toggle for games played entirely on the pad.

**In game**

- **Automatic saves** on the SD card for games that save (SharedObject `.sol`).
- **Pause menu** (**−**): resume, controls, restart, quit.
- **Multi-file games**: some games load companion `.swf` files. A Flashpoint download fetches them automatically into a `<game>.files/` folder; for a game added another way, put them there yourself.
- **Games packaged as a web page**: a few Flashpoint entries ship an `index.html` instead of a plain `.swf`. FlashNX reads its configuration and runs the game, which covers Disney/Yamago minigames (*Agent P Strikes Back*, *Tron Uprising*) and titles such as *Dragon City*.

**The app itself**

- **9 languages**: English, French, Spanish, Russian, German, Italian, Portuguese, Turkish and Simplified Chinese, auto-detected from the console language. Chinese needs the memory a HOME-menu entry gets; started from the Album, FlashNX falls back to English and says why.
- **Report a bug or send a suggestion** from the Settings tab. It opens an issue on this repository through a small relay, with no account and no login, and a report carries the game's details so a broken game can actually be identified.

## Tested games

Super Mario 63 · Super Mario World Flash · Mario Forever · Tetris'd · Flappy Bird · Mario 3D Racing · Icy Tower · Papa Louie 2 and 3 · Newgrounds Rumble · Garfield's Scary Scavenger Hunt · Scooby-Doo: Mayan Monster Mayhem · The Binding of Isaac · Super Smash Flash · Super Brawl 2 · Infiltrating the Airship · This Is the Only Level · Fireboy & Watergirl 2 · Cat Mario · Agent P Strikes Back · Dragon City · Hemp Tycoon · Tron Uprising

How fast a game runs varies a lot, and can vary between scenes of the same game: some are smooth throughout, others drop well below their nominal frame rate. See the first limitation below for why.

## Known limitations

- **Heavy games**: frame-rate drops come from **Ruffle's AVM1/AVM2 interpreter** (CPU-bound, no JIT), not from our rendering. Out-of-app lever: CPU overclock (sys-clk).
- **AS3 compatibility**: partial, inherited from Ruffle (see [Ruffle compatibility](https://ruffle.rs/compatibility)). AS3 games show a badge in the library.
- **No savestate or rewind**: Ruffle does not expose a snapshot of the execution state. Games' native `.sol` saves do work.
- **Audio**: occasional light crackle on *very* dense scenes.

## Credits & licenses

FlashNX is only a **Switch integration layer**. All credit for the Flash emulation goes to **Ruffle**.

- **[Ruffle](https://github.com/ruffle-rs/ruffle)** *(Apache-2.0 / MIT)*: the Flash engine, SWF parsing and the AVM1/AVM2 interpreter. FlashNX embeds `ruffle_core`, `ruffle_render` and `swf`.
- **[devkitPro](https://devkitpro.org/) / libnx** *(ISC)*: Switch toolchain and Horizon API.
- **[switch-mesa](https://github.com/devkitPro/pacman-packages)** *(MIT)*: OpenGL/GLES via Nouveau, the graphics backend.
- **Networking**: [libcurl](https://curl.se/) · [Mbed-TLS](https://github.com/Mbed-TLS/mbedtls) · [zlib](https://zlib.net/) · [Mozilla](https://curl.se/docs/caextract.html) CA bundle. **Rust crates**: [jpeg-decoder](https://github.com/image-rs/jpeg-decoder) (fork patched for newlib) · `png` · `serde` · `tracing` · `flate2` · `getrandom`, plus Ruffle's own dependencies.
- **[Flashpoint Archive](https://flashpointarchive.org/)**: the Flash preservation project. FlashNX uses its public APIs so you can find cover art and download the games you choose. Preserved games remain the property of their original creators, and FlashNX only fetches what you explicitly request.

Thanks to the Switch homebrew ecosystem, whose projects were studied during the port's R&D without reusing code: [ScummVM](https://www.scummvm.org/), the PPSSPP Switch port, [Tico](https://github.com/ticohq/tico), [dawn-switch](https://github.com/dantiicu/dawn-switch), and the devkitPro / GBAtemp community.

**License**: the FlashNX integration code is under the **MIT** license (see [`LICENSE`](LICENSE)). Ruffle and the other dependencies keep their own.

---

📖 **Architecture, build & technical notes** → [DEVELOPMENT.md](DEVELOPMENT.md)
📦 **Changelog** → [CHANGELOG.md](CHANGELOG.md)
