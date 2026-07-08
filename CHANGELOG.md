# Changelog — FlashNX

Homebrew Flash player for Nintendo Switch (`.nro`), powered by [Ruffle](https://github.com/ruffle-rs/ruffle).

## v1.5.1 (2026-07-08)

A small fixes release.

### Fixes

- **Games no longer turn to a red screen after a long session** (#62, #63): after playing one game for a while, launching a different one could show a full red screen. The game file was being read in a single large operation that could spuriously fail once memory had fragmented; it is now read in small chunks like the rest of the app, so the next game loads reliably.
- **Caps Lock can now be mapped to a button** (#61): a few games use the Caps Lock key for a mechanic (for example, a stage in *This Is the Only Level* that only opens the exit while Caps Lock is held). The visual keyboard in the control editor (TOUCHES) now has a **Caps** key you can bind.

## v1.5.0 (2026-07-02)

A rework of the control editor: a visual keyboard, more keys, button combos, and per-game cursor options.

### Added

- **Visual keyboard for remapping** (#55): the control editor now shows a full PC keyboard to pick a key, instead of a long scrolling list. Navigate it with the D-pad or stick and press **A**. Ctrl, Alt, Tab and the like are now easy to find, and the board covers the whole layout. Keys already used by the current mapping are highlighted, so you can see at a glance which ones are taken.
- **More keys**: F1-F12, the symbol keys (`-` `=` `[` `]` `;` `'` `,` `.` `/` `\` `` ` ``) and the numpad operators (`+` `-` `*` `/`) can now be bound.
- **Button combos, one layer per modifier** (#57): for games that need more inputs than the controller has buttons, hold a modifier button and every other button sends a different key, so `ZL + A` can send `F1`. Each of the four modifiers (ZL, ZR, L, R) has its **own** combo layer and they all work at once in a game, so `ZL + A` and `R + A` can be different keys. The editor has a sub-tab (move with **L/R**) across `NORMAL / ZL / ZR / L / R`: NORMAL is the base bindings, and picking a modifier edits that modifier's own layer (rows then read `ZL+A`). **X** switches the Player 1 / Player 2 tab, each with its own layers. A button with no combo key keeps its normal one while a modifier is held, so movement never breaks.
- **Show or hide the mouse cursor per game**: a toggle in a game's controls options (TOUCHES) hides the on-screen pointer for games played with the pad or keyboard where it just gets in the way. Clicks still work, only the pointer is hidden.
- **Shared profiles now carry the whole setup** (#20): sharing or applying a community control profile also transfers the combo layers, the cursor speed, and the show-cursor choice, not just the base bindings. The before/after preview shows these too.

### Fixes

- **Very large games no longer go to a white screen** (#56): games with thousands of unique vector shapes (e.g. the Henry Stickmin titles like *Infiltrating the Airship*) filled the shape buffer partway in, after which the rest of the art stopped drawing. The buffer is now large enough to hold these, so they render fully.
- **Fewer white screens / out-of-memory on heavy games** (e.g. *Super Bowser World*): the dedicated bitmap layers are now freed as soon as they empty and sized to what they actually hold, instead of piling up and exhausting memory.
- **Water and distortion effects now render correctly**: games that ripple graphics with a displacement-map filter (e.g. underwater levels) used to show garbled stripes, or the effect did nothing. The filter is now supported, and a texture-packing bug that striped these scenes is fixed.
- **A game no longer appears twice in the shared-profile catalog**: the `.swf` file extension is no longer part of the title used to match profiles, so entries for the same game line up instead of splitting (this was showing *Super Mario 63* twice).
- **The on-screen cursor is easier to see**: the pointer now has a black outline so it stays visible over both light and dark game art.
- **Changing only the cursor speed re-enables sharing**: after applying a community profile, adjusting just the pointer speed used to still say there was nothing to share; that change now counts.

## v1.4.1 (2026-06-28)

A follow-up to the Chinese support in v1.4.0, plus wider import support.

### Fixes

- **Chinese (and Japanese/Korean) text now shows inside games**: v1.4.0 added Chinese to the app's own menus, but text drawn by a running game still came out blank when the game used a system font for it. Games now fall back to the Switch's built-in fonts for any character a game's font is missing, so in-game CJK text renders. (#54)
- **Import from Wayback Machine links**: a `web.archive.org` snapshot URL of a `.swf` is now accepted and downloads the actual game (it used to be treated as an archive.org item and fail).

### Changed

- The control-profile catalog is now fully community-driven: the one bundled profile (Super Mario 63) was removed, as it only mirrored the default controls anyway. Share and apply profiles from a game's options as before.

## v1.4.0 (2026-06-25)

Community control profiles, more languages, and Flashpoint games that were impossible to import before.

### Added

- **Community control profiles**: share your key bindings for a game and download other players' setups (#20). In a game's options (**+**), pick "Share my controls" to publish your profile, or "Apply a profile" to browse what the community has shared for that game and try it; applying a profile is non-destructive and can be reverted. Profiles you shared can be deleted again. Verified and most-applied profiles sort to the top.
- **More languages**: German, Italian, and Brazilian Portuguese join the menus, plus **Simplified Chinese** (#41) rendered from the Switch's own shared font.
- **Numpad keys in the controls editor**: the editor now offers Num0–Num9 (the numeric keypad), listed first, for games that read keypad keys separately from the top-row digits. Player 2 defaults to the numpad.

### Fixes

- **Flashpoint games with a non-ASCII title now import and launch**: a game whose file name uses non-Latin characters (for example *包丁少女幻窓曲*) failed to download with an error -2. Its address is now encoded correctly. (#51)
- **Flashpoint games that load their assets on the fly now play**: some games build the paths to their data and art files while running, so those files could not be fetched ahead of time and the game stayed on a blank screen (for example *Racing is Magic*). Missing files are now pulled from the Flashpoint mirror on demand and cached, for games imported from the Flashpoint search. (#51)

## v1.3.1 (2026-06-19)

Local two-player, a touch-driven launcher, and a batch of game fixes.

### Added

- **Local two-player (two controllers)**: a second controller now drives Player 2 through its own set of key bindings, for Flash games where two players share one keyboard (for example *Fireboy & Watergirl*, *Dragon Ball Z Devolution*). The controls editor has a Player 1 / Player 2 toggle (press X), and both players' keys are saved per game. Player 2 defaults to WASD so it does not clash with Player 1's arrows. Needs two full controllers (a Pro controller or a Joy-Con pair each). (#40)
- **Touch controls in the launcher**: in handheld mode you can drag the game gallery to scroll, tap a game to select it, and tap it again to launch it.
- **Flashpoint content filter toggle**: press ZL+ZR in the Flashpoint search results to turn the content filter on or off. Importing a game also fetches its cover automatically. (#33)
- **Download of non-zipped Flashpoint games**: games served loose (not as a single archive) now download through the htdocs mirror. (#26)

### Fixes

- **Super Smash Flash**: the announcer now plays, and the game no longer freezes on a blank screen after a fight instead of showing COMPLETE. Its voices and most of its sound effects use the Nellymoser audio format, which was not enabled. (#29)
- **No more crash when some games save**: a game saving a self-referential object (for example *Hemp Tycoon*) used to crash the app. The save now completes. (#33)
- **Color speckle on translucent effects fixed**: semi-transparent effects (for example the smoke in *Offroaders*) showed cyan and magenta speckle. (#38)
- **Flashpoint games with a space in their name now launch the right file** instead of the first one found in the archive.
- **The "&" character now shows in the menus**: it was missing from the UI font, so titles like "Fireboy & Watergirl" dropped it.

## v1.3.0 (2026-06-14)

A big one: multi-file games, Flashpoint downloads that actually start, an in-game keyboard, favorites, and a much more configurable control editor.

### Added

- **Multi-file game support**: a game that loads other `.swf` files at runtime (`loadMovie` / `loadMovieNum` into a level) now finds them in a `<game>.files/` folder next to the `.swf`. Download a game from the Flashpoint search (X) and its companion files are fetched automatically; for a game added another way, drop the companions in that folder yourself. *Garfield's Scary Scavenger Hunt* now plays from start to finish.
- **Multi-file indicator**: the launch screen shows a "MULTI-FILE (N)" label when a game pulls in companion files, so you can tell at a glance.
- **Flashpoint downloads now bring the whole game**: a download from the Flashpoint search now unpacks the game's full bundled set of files (alternate versions, ad-network stubs, data files) and launches the exact version the archive intends, instead of guessing. Games that used to get stuck on a sponsor or "Download the latest Adobe Flash Player" screen (for example *Papa Louie 2: When Burgers Attack*) now start and play.
- **In-game keyboard**: when a Flash game wants text (a player name, a level password, high-score initials, a text adventure), the Switch keyboard opens when you click the text field, pre-filled with its current text and set to the right type (numbers, password, multi-line). Games that were unplayable with a controller alone now work.
- **Favorites**: in a game's options (**+**), mark it as a favorite. Favorites are pinned to the top of the Play gallery with a gold marker, whatever the sort order.
- **Assignable mouse clicks**: the controls editor now has **Left click** and **Right click** actions you can bind to any button. By default **ZR** is left click and **ZL** is right click. The touchscreen still left-clicks.
- **More mappable inputs**: SL / SR (Joy-Con side buttons), the stick presses (L3 / R3), and the **right stick as a d-pad** (bind its directions and it stops being the mouse cursor; the touchscreen stays the cursor).
- **Translated control labels**: the keys shown in the controls editor (clicks, Space, Enter, arrows, and so on) now follow your language (English, French, Spanish, Russian).
- **Adjustable cursor speed**: the right-stick mouse cursor now has a speed setting (x0.5 to x2.5), in Settings and in the in-game pause menu where it cycles live as you press it. Handy for games that need fast mouse movement (for example *Spank the Monkey*). Your choice is saved across games and launches. (#17)
- **Much faster Flashpoint downloads**: downloads now batch their writes to the SD card and pump the network harder, turning what used to be a roughly two-minute download into about ten seconds for a large game.
- **Home-menu shortcuts for a single game**: FlashNX can now be launched straight into one game when its `.swf` path is passed as a launch argument — it skips the library and returns to the Home menu when you quit. With a homebrew forwarder tool this lets you put a specific Flash game on your Switch Home menu, with its own cover as the icon. If you use **Sphaira**, FlashNX registers a `.swf` association on launch, so you can pick a `.swf` in its file browser, choose "Create a Forwarder", select FlashNX, and the shortcut boots straight into that game.

### Fixes

- **Buttons mapped to letter or number keys now trigger games that read them as keyboard shortcuts**: a controller button bound to a letter (or a digit or space) now fires a game's keyboard shortcuts, not just its held-key checks. For example *Scooby-Doo: Mayan Monster Mayhem* (H for help, S/T to switch the held item) now responds; before, only movement and pickup worked.
- **Deleting a game also removes its `<game>.files/` companion folder and its favorite mark**, so nothing is left behind on the SD card.
- **Games that rendered as a blank white screen now display correctly**: very art-heavy games (thousands of on-screen vector shapes at once, for example *The Binding of Isaac*) exhausted the renderer's geometry buffers, which silently dropped the extra shapes and left most of the game invisible behind a few stray text glyphs. The buffers are now much larger, and a buffer overflow is reported instead of failing silently. (#16, #23)
- **Large multi-file games now download and run**: big Flashpoint games (over the old 64 MB download limit, loading dozens of companion files by relative path, for example *Super Brawl 2*) used to show a full download bar and then do nothing, or launch to a black screen. They now extract fully and run. This took a few fixes: a larger download limit, launching the game under its original URL so its relative asset loads resolve, and doing the extraction and asset reads through the C++ filesystem layer (the Rust one drops some files on the Switch).

## v1.2.1 (2026-06-11)

Small fix release: games that use PixelBender shaders no longer crash.

### Fixes

- **PixelBender games no longer crash**: some games build a Flash `Shader` / `ShaderFilter` at runtime (for example **The Terminal**). They used to abort the app the moment the shader was created. They now run normally; the shader's visual effect itself is skipped (this renderer does not run PixelBender), but gameplay and input work. As a bonus, crash messages from the game thread are now captured to the crash log instead of being lost.

### Changes

- **Clearer bug reports**: a report now includes the game's import URL when it was added from a link, so a game imported under an arbitrary filename can still be identified. The report also reminds you that it opens a public issue on the FlashNX repository, and you can add your GitHub handle if you want a follow-up.

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
