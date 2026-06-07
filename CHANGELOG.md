# Changelog — FlashNX

Homebrew Flash player for Nintendo Switch (`.nro`), powered by [Ruffle](https://github.com/ruffle-rs/ruffle).

## v1.2.0 (unreleased)

Library redesign: a tabbed navbar, a cover-art gallery, and a list-based importer.

### Features

- **Tabbed navigation**: a top navbar switched with **L / R** between **Play** (your games), **Import**, and **Settings**.
- **Cover gallery**: the library is now a gallery of cover art (justified rows, each cover at its natural size) instead of a text list. Games with no cover get a generated tile (color + initials).
- **Your own covers**: drop a `<game>.png` or `.jpg` next to the `.swf` and it shows up as the cover.
- **Flashpoint covers**: a game's options has a **Cover** action that searches the Flashpoint Archive by name and shows the candidates as thumbnails to pick from.
- **Import as a list**: the Import tab is now a list of your saved URLs. Press **A** to launch one, use the **+ Add a URL** row to enter a new one, and **+** on a URL to edit or delete it. It accepts archive.org items and direct `.swf` URLs.

### Changes

- **Controls**: **−** is search, **+** is the selected game's options; the default-controls, language and **Quit** actions all live in the Settings tab. Switching tabs is **L / R** only.

### Notes

- Covers use the public Flashpoint metadata and logo API to enrich games you already have. FlashNX never downloads games from Flashpoint.

## v1.1.1 (2026-06-05)

Data-safety and import-diagnostics fixes, plus library search.

### Fixes

- **URL history no longer disappears in applet mode**: history (and saves, settings, renamed-game sidecars) is now read with a bounded reader and committed to the SD card after every write, so it survives switching between applet (album takeover) and full title-takeover modes. Previously the history could read empty in applet mode, or get overwritten by the next change.
- **HTTPS import errors are now readable**: a failed import shows the real cause (libcurl code and message, HTTP status) instead of an opaque "code -2", so you can tell whether it is the console clock, DNS, the certificate bundle, or a blocked link.

### Features

- **Search the local library**: press **X** to filter the game list by name (empty input clears the filter), the same way the archive.org screen already works.
- **Clear applet-mode notice**: trying to launch a game without the full app memory now shows a readable message ("launch via title takeover") instead of a red screen.

### Changes

- **More consistent controls**: **X** is search on every list, and **ZL** manages the selected item (game options in the library, delete URL in the import history). The on-screen footers reflect the new layout.
- The default example URL is now a neutral placeholder.

## v1.1.0 — 2026-06-04

Localization update + UI polish.

### Features

- **Multi-language UI**: the whole interface is now available in **English, French, Spanish and Russian**. The language is auto-detected from the console's system language on first boot, and can be changed at any time.
- **Settings modal (`+` in the library)**: a new global settings screen with two entries — **default controls** (edit the global default keymap used by every game without a per-game override) and **language**.
- **URL history management**: in the archive.org import screen, **X** removes the currently-shown URL from the history (with a confirmation modal).
- **Quit returns to the right row**: leaving a game (pause menu → QUIT) lands the cursor back on the game you were playing, instead of jumping to the top of the list.
- **Pause menu shows the game's name** under "PAUSE" (like the OPTIONS modal).
- **Library shows 8 games** at once (was 6).
- Pixel font extended with **uppercase Cyrillic** (Russian), **French/Spanish accents** (É È À Ç / Á Í Ó Ú Ñ ¿ ¡), the **apostrophe** (`'`), and previously-missing punctuation (`,` `?` `+` `(` `)` `[` `]` `<` `%` `…`), which also improves the existing locales.

### Notes

- The chosen language is persisted to `sdmc:/flashnx/settings.json`.
- Flash key names (`Space`, `Shift`, `A`…`Z`) are technical identifiers and remain untranslated; only UI labels and messages are localized.
- Opening the settings modal returns to the previously-selected game row (like the OPTIONS modal).

## v1.0.0 — 2026-05-31

First official release. FlashNX runs AS1/AS2 Flash games (and part of AS3) straight from your Switch's SD card.

### Features

- **Full Flash player**: Ruffle core (SWF parsing + AVM1/AVM2 interpreter) wired onto a native Switch stack — OpenGL rendering (switch-mesa), audio (audren), joycon + mouse input (right stick / touchscreen).
- **FlashNX library**: joycon-navigable interface, banner + per-game thumbnail, `AS3` badge for AVM2 games, game renaming (without touching the file), automatic `.swf` detection on SD.
- **archive.org remote import**: download `.swf` files over HTTPS directly from the Switch (software keyboard, URL history, progress bar).
- **In-game key editor**: remap the 48 supported Flash keys per game, from the pause menu or the library.
- **Native saves**: games that save via `SharedObject` (`.sol`) keep your progress on the SD card.
- **Robustness**: anti-fragmentation GL mega-arena, handling of bitmaps > 2048 px, native exception handler with a symbolizable crash log. The `.nro` never refuses to boot (built-in fallback).

### Games tested on hardware

Super Mario 63, Super Mario World Flash, Mario Forever Flash, Tetris'd, Flappy Bird, There Is Only One Level, Mario 3D Racing, Pursuit of Hat 2, and others. Most run at 55-60 fps.

### Known limitations (acknowledged)

- **Heavy-game performance**: on some titles (Mario 63 in dense scenes, complex AS3 games like Pursuit of Hat 2), frame-rate drops come from **Ruffle's AVM2/AVM1 interpreter** (CPU-bound, no JIT), not from rendering — not fixable from the backend. Measured: our rendering stays around ~5 ms/frame while the game logic can take >1 s on a single frame. Out-of-app lever: CPU overclock (sys-clk).
- **Partial AS3/AVM2 compatibility**: inherited from the upstream Ruffle engine (see [ruffle.rs/compatibility](https://ruffle.rs/compatibility)). Games showing an `AS3` badge in the library have variable support.
- **No savestate / rewind**: Ruffle does not expose a snapshot of the execution state (the state is a `gc-arena` object graph, not trivially serializable). Games' native saves (`.sol`) do work.
- **Audio**: the sound is now soft-limited (loud, without hard clipping). On **very** dense scenes (Mario 63), a slight occasional crackle may remain (peak compression) — to be refined in an update.

### Installation

1. Copy `FlashNX.nro` into `sdmc:/switch/` (or `sdmc:/switch/FlashNX/`).
2. Copy your `.swf` files into `sdmc:/flashnx/`.
3. Launch FlashNX from the Homebrew Menu.

### Credits

- **Author**: Jonathan8520
- **Powered by Ruffle** (Apache-2.0 / MIT) — the Flash emulation core.
- Native Switch stack via devkitPro / libnx / switch-mesa.
