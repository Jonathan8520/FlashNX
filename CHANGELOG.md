# Changelog — FlashNX

Homebrew Flash player for Nintendo Switch (`.nro`), powered by [Ruffle](https://github.com/ruffle-rs/ruffle).

## v1.2.1 (2026-06-11)

Small fix release: games that use PixelBender shaders no longer crash.

### Fixes

- **PixelBender games no longer crash**: some games build a Flash `Shader` / `ShaderFilter` at runtime (for example **The Terminal**). They used to abort the app the moment the shader was created. They now run normally; the shader's visual effect itself is skipped (this renderer does not run PixelBender), but gameplay and input work. As a bonus, crash messages from the game thread are now captured to the crash log instead of being lost.

## v1.2.0 — 2026-06-10

Big library update: a tabbed navbar, a cover-art gallery, a list-based importer, Flashpoint game downloads, library sorting, playtime, and in-app bug reports.

### Features

- **Tabbed navigation**: a top navbar switched with **L / R** between **Play** (your games), **Import**, and **Settings**.
- **Cover gallery**: the Play tab is a grid of cover art, 5 per row (covers are cropped to fill the tile). Games with no cover get a generated tile (color + initials).
- **Your own covers**: drop a `<game>.png` or `.jpg` next to the `.swf` and it shows up as the cover.
- **Flashpoint covers**: a game's options has a **Cover** action that searches the Flashpoint Archive by name and shows the candidates as thumbnails to pick from. The search name is cleaned up automatically (download-id suffixes such as `game-15938d603` are dropped), and **−** lets you retype the title when the filename does not match the catalog (for example `catmario` to `cat mario`).
- **Download games from Flashpoint**: in the Import tab, **X** searches the Flashpoint Archive and shows the results as a cover grid; **A** downloads a game's `.swf` directly. Its cover is fetched automatically, and its real title is kept even when the filename cannot hold characters like `:`. Press **+** on a result to see its full title, developer, publisher, release date and download size.
- **Import as a list**: the Import tab is a list of your saved URLs. Press **A** to launch one, use the **+ Add a URL** row to enter a new one, and **+** on a URL to edit or delete it. It accepts archive.org items and direct `.swf` URLs.
- **Sort your library** (**Y** in the Play tab): by name, date added, last played, most played, or size. **X** reverses the order, and the choice is saved.
- **Playtime**: each game tracks how long you have played it (shown under the selected game, and used by the "most played" sort).
- **Report a bug or send a suggestion** (Settings tab): flag a game that renders or plays wrong, or send a feature idea. It opens an issue on the FlashNX repository, with no account and no login.

### Changes

- **Controls**: **−** is search, **+** is the selected game's options; default controls, language, bug report, suggestion and **Quit** all live in the Settings tab. Switching tabs is **L / R** only, and **B** always just backs out of a modal (the redundant "Back" rows were removed).
- **Audio level**: the in-app sound now matches the rest of the Switch (it used to be noticeably louder).

### Fixes

- **Large backgrounds no longer turn white**: games whose backdrop or floor is a bitmap wider or taller than 2048px (for example Mario Combat's sky and ground) used to render as solid white blocks. They now draw correctly.
- **Deleting a game cleans up everything**: removing a game now also deletes its cached online cover and the cover sidecars saved under the plain game name (on top of the `.swf` and its keymap/rename/save files), and clears the leftover Import-list "downloaded" badge and the on-screen cover, so re-importing the same game later starts fresh.
- **Flashpoint cover grids no longer freeze the UI**: logos load in the background, so a broad search with dozens of results stays responsive while the thumbnails fill in.
- **Missing accents** restored on several labels (the sort options, "edit", "download").

### Notes

- Covers and downloads use the public Flashpoint Archive APIs (metadata, logos, GameZIP). Downloading a game is always something you choose, one game at a time.
- Bug reports and suggestions are anonymous: they go through a small relay that opens a GitHub issue, so you never need an account or to log in.

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
