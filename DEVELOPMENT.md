# FlashNX — technical guide

**FlashNX** — homebrew Flash player for Nintendo Switch (`.nro`). Runs any AS1/AS2 `.swf` (and part of AS3) straight from the SD card.

**Powered by [Ruffle](https://github.com/ruffle-rs/ruffle)** (Apache-2.0 / MIT) — FlashNX embeds the `ruffle_core` core (SWF parsing + AVM1/AVM2 interpreter) and wires a native Switch stack onto it: custom OpenGL backend (switch-mesa), audren audio backend, SD storage, joycon-navigable library UI, in-game key editor, anti-fragmentation GL mega-arena, native libnx exception handler, patched fork of jpeg-decoder for newlib. The AVM1/AS2/AVM2 compatibility gaps come from the upstream Ruffle engine — see [ruffle.rs/compatibility](https://ruffle.rs/compatibility) (~99% of the AVM1 language, ~75-81% of the APIs).

> **About the name**: the repo, the toolchain, the build scripts and the Cargo crate keep the historical name `flash-for-switch` (to avoid a destructive switch-over). **FlashNX** is the user-facing name — the UI, the `.nacp`, the `.nro` and the SD folder (`sdmc:/flashnx/`).

> 🕘 The phase-by-phase development log (Phases 0 → 4, dates, estimates) now lives in the `git log` history and the [CHANGELOG](CHANGELOG.md). This document is a **reference guide** for building, testing and contributing — not a log.

## Architecture decision

**Chosen option: switch-mesa (OpenGL)**, rather than dawn-switch (WebGPU):
- switch-mesa is mature (`dkp-pacman -S switch-mesa`), used in production by ScummVM, PPSSPP, RetroArch.
- dawn-switch is a 1-commit POC, depends on NVK Switch that is not publicly sourced.
- wgpu's GL backend *"only seems to work under a Mesa context"* — switch-mesa **is** a Mesa context.

**Hybrid C++/Rust** strategy (Ruffle requires `std`, not `no_std` → newlib via devkitPro):

```
cpp/ (devkitPro)  →  rust staticlib (Ruffle + backends)  →  switch-mesa GL  →  GPU Tegra X1
```

## Project structure

```
flash-for-switch/
├── cpp/
│   ├── Makefile                  # template devkitPro switch + APP_TITLE/AUTHOR/VERSION + --icon/--nacp + optional --romfsdir
│   ├── romfs/                    # absent by default; one .swf here = self-contained single-game .nro (#106)
│   ├── src/
│   │   ├── main.cpp              # libnx init + worker thread + applet loop + joycon/touch input
│   │   ├── gl_context.cpp        # EGL/GL via switch-mesa, EGL_STENCIL_SIZE=8
│   │   ├── input.cpp             # (empty — placeholder, input lives in main.cpp)
│   │   ├── audio.cpp             # libnx audren wrapper + worker thread + synthesised interface sounds (voices 1-3)
│   │   ├── exception.cpp         # native __libnx_exception_handler (symbolizable crash log)
│   │   ├── swf_picker.cpp        # SD scan via opendir/readdir (works around the Horizon read_dir bug)
│   │   ├── nsp.cpp               # HOME-menu tiles: builds the three NCAs and installs them via ncm/ns
│   │   ├── net.cpp               # swkbd prompts + HTTPS: sync GET/POST, async download + metadata GET + isolated thumbnail GET + HEAD size
│   │   └── ruffle_bridge.cpp     # ruffle_log_cstr + getrandom + sysconf stubs + svcGetInfo RAM
│   └── include/ruffle_bridge.h
├── forwarder/                    # the program a HOME-menu tile runs: nx-hbloader (ISC) pointed at FlashNX
│   ├── forwarder.json            # NPDM for npdmtool; the program ID here is a placeholder, rewritten per tile
│   └── source/main.c             # the only change from upstream: which NRO to load, and with which argument
├── rust/
│   ├── Cargo.toml                # crate-type = ["staticlib"], ruffle_core features=[audio,mp3,default_font] + jpeg-decoder patch
│   ├── rust-toolchain.toml       # nightly-x86_64-pc-windows-gnu + rust-src
│   ├── .cargo/config.toml        # target aarch64-nintendo-switch-freestanding + rustflags
│   └── src/
│       ├── lib.rs                # FFI exports + PlayerBuilder + SWF loader + input handlers + tick/render profiling
│       ├── library.rs            # Library UI state + banner/icon embed + SWF header parse + meta/keymap sidecars + sort + bug-report/suggestion flows
│       ├── bugreport.rs          # In-app bug report / suggestion: build JSON + POST to the relay (Cloudflare Worker in tools/bug-report-worker)
│       ├── playtime.rs           # Per-game play time (playtime.json) + most-played sort
│       ├── net.rs                # HTTPS transport: sync GET/POST + async download/metadata/thumbnail GET + HEAD size + swkbd prompts
│       ├── sources/             # Multi-source: classify (archive.org / direct .swf) + Flashpoint search/cover logos + GameZIP game download (gamezip.rs)
│       ├── covers.rs            # Local cover art: sidecar/cache/default resolution + PNG/JPEG decode + Flashpoint fetch (manual + auto-on-download)
│       ├── keymap.rs             # JSON keymap (sidecar + default + fallback) + mutation API
│       ├── menu.rs               # TOUCHES sub-screen state machine (pages beside the pause + the controls editor)
│       ├── loc.rs                # UI localization (9 languages) + settings.json persistence
│       ├── loc_ui.rs             # strings of the screens rebuilt on the interface kit (9 languages)
│       ├── sfx.rs                # which interface sound a press deserves, read from the screen before and after it
│       ├── ffi/gl.rs             # OpenGL FFI subset (hand-written, no bindgen) + glReadPixels/PACK_ALIGNMENT
│       └── backend/
│           ├── render.rs         # SwitchRenderBackend : atlas + UV wrap + GlStateCache + mega-arena +
│           │                     #   stencil masking INCR/DECR + Glow/DropShadow/Blur/ColorMatrix/Bevel filters +
│           │                     #   FilterTexturePool TTL + render_offscreen/resolve_sync_handle (BitmapData.draw) + UI overlays
│           ├── render/kit/       # interface bricks: console fonts + button glyphs (text.rs), SDF shapes (shapes.rs),
│           │                     #   cover rule (cover.rs), windows (window.rs), scrolling list + Row (list.rs),
│           │                     #   row controls, chips, help box (controls.rs), side panels + cascade (sheet.rs),
│           │                     #   a panel's rows in any rectangle (pane.rs),
│           │                     #   Lucide icon font subset (icons.rs + icons.ttf, built by scripts/make_icon_font.py), theme.rs
│           ├── render/pages/     # whole screens drawn from the kit: settings, importer, flashpoint, game options, windows
│           ├── audio.rs          # SwitchAudioBackend (port CpalAudioBackend → libnx audren)
│           ├── storage.rs        # SwitchStorageBackend (port DiskStorageBackend → sdmc:/flashnx/ flat)
│           ├── tracing.rs        # Routes Ruffle's tracing events to nxlink stdout
│           ├── log.rs            # SwitchLogBackend → ruffle_log_cstr
│           └── navigator.rs       # SidecarNavigator : multi-file games, serves sibling SWFs from <game>.files/
├── patches/
│   ├── README.md                 # How to re-apply after git submodule update
│   ├── 0001-mario63-zero-scale-hit-test.patch  # Fix Toad castle #6906
│   ├── 0002-pixelbender-shaderjob-run-noop.patch  # PixelBender games don't crash
│   └── 0003-amf-cycle-serialize-crash.patch  # No crash on cyclic SharedObject save
├── third_party/
│   ├── ruffle/                   # git submodule + patches/*.patch applied
│   └── jpeg-decoder-switchfork/  # vendored jpeg-decoder-0.3.2, select_worker → Immediate forced
├── assets/{icon.jpg, banner.png, cacert.pem, screenshots/, *.nacp}
└── scripts/{build.sh, build.ps1, setup-env.ps1, setup-env.sh}
```

**Interface.** Every launcher and pause screen draws through the kit in `backend/render/kit/`: text in the console's shared fonts (`pl`: Standard, NintendoExt for the button glyphs, Chinese Simplified on demand), rasterised at the size it is shown into a per-renderer atlas; rounded panels, rings and shadows from one SDF shader; icons from a Lucide subset compiled into the binary. Colours, sizes and radii live in `kit/theme.rs`. A new screen goes in `render/pages/` and is built from `kit_scroll_rows` / `Row`, `draw_modal_frame`, `draw_confirm`, `Sheet` and `kit_hints*` rather than drawing its own rows; new strings go in `loc_ui.rs`, in all nine languages. The hand-drawn 5x7 font remains only as the fallback when the console fonts cannot be loaded.

A row is a `Row` (`kit/list.rs`): an icon, a label, a control on the right (`Ctrl` in `kit/controls.rs`: a value to read, a value stepped once its row is armed, a choice with every option on show, a switch), and a chevron when A opens another page, or a keyboard when A brings up the Switch keyboard. The chevron is the promise that A goes somewhere; a row without either acts where it is. A choice or a number changes only once A has armed its row: Left and Right then move it (live, in game), A keeps it, B puts back exactly what was there, Up and Down keep it and move on; a switch flips on A, and Left and Right change nothing on a row that is not armed. The armed control is outlined in gold with an arrow on each side (`kit_ctrl_parts`); in game the armed row lives in `pause_values.rs`, in the launcher in `library::Armed`. This answers the classic complaint about console menus, settings changed by a stick pushed slightly sideways on the way down a list (a RetroArch user asking for "button activation everywhere": forums.libretro.com/t/request-friendlier-settings-management/4680; one consistent interaction model, Xbox Accessibility Guidelines 112 and 114). The menu loops also keep each stick and the d-pad to one axis at a time (dominant-axis latch, `nav_filter` in `main.cpp`). Lists wrap on a fresh press and stop under a held direction (`list_step_wrap`).

A side panel is a `Sheet` (`kit/sheet.rs`), described as data and drawn by `kit_sheet_at`: a caption with the path, the title, status chips, the rows in sections, a help box for the focused row, the hints. A Sheet lists every help it can show (`helps`), so its help box is sized once for the page and never moves with the cursor, and a game's name in its title (`title_is_name`) is cut in the middle, where episodes of a series differ. The in-game pause is one at the left edge (`with_pause_sheet` in `render.rs`); a game's options in the launcher are one at the right edge (`library/options_panel.rs`). A row with a chevron opens its page beside the panel rather than over it: the panel folds to a column of icons with the branch marked, the page is uncovered next to it, and B folds it back (`kit_cascade_on`). The pages shared by both doors are built once: Touches et curseur, the shared profiles and the before / after tables in `menu.rs` (`with_touches_view`, `with_profiles_view`, `with_diff_view`), Écran in `render.rs` (`with_screen_view`, from a `ScreenView`; only its zoom differs, the framing mode in game, presets armed by A in the launcher).

**Turned games.** When the picture is turned a quarter (`rotation_swaps_axes`), everything drawn over the game is drawn in the game's logical viewport, which is then portrait (720 x 1280). Every in-game surface reads `self.dimensions` and has a layout of its own for it rather than the landscape one squeezed: the controls editor puts the controller over its rows, the key picker stacks the numpad under the main block (`keymap::keyboard_slot`, shared by drawing, touch and navigation), the zoom legend splits over several plates, and the pause panels hug their content.

The Réglages tab is laid out like the console's own System Settings (`library/settings_tab.rs` for its sections, input and flows, `render/pages/settings.rs` for its bricks): the sections in a column on the left, the open section's settings in a pane beside it, changed where they are shown. The game defaults are a panel's rows drawn in that pane (`kit_pane_rows`); a deeper level (the games folder browser, a bug report's game list) opens inside the pane with its path above it, and only questions and the controls editor sit on top of the page.

The UI/Video backends use the `Null*` implementations provided by default by `ruffle_core` (no dedicated file). **Audio** = `SwitchAudioBackend`. **Storage** = `SwitchStorageBackend`. **Navigator** = `SidecarNavigator`: for multi-file games it resolves relative loads (`loadMovie` / `GetURL` into `_levelN`) against the movie URL and reads the sibling SWF from `<game>.files/` on the SD card; the spawned loader futures are pumped once per frame by a `NullExecutor` kept in `State` (after the player lock is released, since they re-lock the player). Genuine network fetches stay rejected.

### Assets embedded in the `.nro`

| Asset | Format | Dimensions | Usage |
|---|---|---|---|
| `assets/icon.jpg` | JPEG baseline sRGB | 256×256 | `.nro` icon picked up by hbmenu / Sphaira via `elf2nro --icon=` ([cpp/Makefile](cpp/Makefile)). |
| `assets/banner.png` | PNG RGBA | 720×144 (5:1 ratio) | Banner at the top of the library UI: embedded via `include_bytes!` in [rust/src/library.rs](rust/src/library.rs), decoded via the `png` 0.18 crate at boot, uploaded to a GL texture (`upload_rgba_texture`), rendered by `draw_textured_rect` (1 textured quad/frame). ASCII "FLASHNX" fallback if the decode fails. |
| `assets/cacert.pem` | PEM | — | Mozilla CA bundle for libcurl HTTPS (archive.org import), embedded via `include_bytes!` + written to SD on first boot. |

`assets/screenshots/` is only used by the README — not embedded in the `.nro`.

## Self-contained build: ship one game as its own `.nro` (#106)

A `.nro` whose RomFS holds a `.swf` boots that game directly: no library, no SD
card scan, the game is on screen as soon as Ruffle is up. It is the same code
path a Sphaira forwarder takes, with the game read from `romfs:/` instead of the
card. There are two ways to make one.

### Without building anything: `scripts/make_standalone.py`

```bash
python scripts/make_standalone.py FlashNX.nro MyGame.swf --name "My Game" --author "Me" --icon cover.png
```

It takes `FlashNX.nro` as released (1.9.0 or later), keeps its program part
byte for byte and writes a new asset section after it (the format is on
[switchbrew](https://switchbrew.org/wiki/NRO)): the picture as the icon, laid out
like FlashNX's tiles (cropped to the square only when that cuts at most 15 % of
it, otherwise whole over a blurred, darkened copy of itself), FlashNX's NACP with
the new name, author and version, and a RomFS holding the game. Its output is
byte for byte what `elf2nro --icon --nacp --romfsdir` makes from the same files
(checked against devkitPro's `elf2nro` and `build_romfs`, multi-file trees,
accents and spaces included), and it reads the result back the way libnx does
before keeping it. A `<game>.files` folder next to the `.swf` goes in as the
companions. Python's standard library is all it needs; Pillow lets the picture be
any size or format, without it the picture must be a 256x256 JPEG. Run without
arguments (or double-clicked), it opens a window doing the same, with the
FlashNX logo and a preview of the app as the Homebrew Menu will list it. Given
`latest` instead of a path (or "Download the latest" in the window), it first
downloads the latest release's `FlashNX.nro` from GitHub, next to itself
(`scripts/*.nro` is git-ignored), so a creator only needs the script. The
picture can also be a web address (a Google Images result link points at the
picture it shows, and its `data:` thumbnails work too), downloaded for the
preview and the icon. On Windows the window takes drops anywhere, with nothing
to install: a `.swf`, a picture, a `FlashNX.nro`, a companion folder, or a
picture dragged straight from a web page. Tk has no drag and drop of its own,
and the simple `WM_DROPFILES` only carries files that exist on disk, so the
script registers an OLE `IDropTarget` through `ctypes` (`windows_drop`), the
interface browsers drag with: Explorer's `CF_HDROP` gives the files, and for a
picture from a page the `<img src>` of the HTML the browser hands over (the URL
may be the link around it), else the URL itself. Elsewhere, drops work only if
the `tkinterdnd2` package is installed; pasting the picture's address works
everywhere. Its fields take Ctrl+Z and Ctrl+Y. Choosing the game fills its
companion folder and its cover when they sit beside it, named as FlashNX looks
for them (`<game>.files`, `<game>.png`/`.jpg`, or `<game>.swf.png`/`.jpg`), never
over a picture already chosen. A Flashpoint GameZIP unpacked as is
(`content.json` beside `content/`) works as a companion folder too: only what is
under `content/` goes in, `<host>/<path>`, the way FlashNX lays it out. The name,
author and version fields stop at what the NACP holds (511, 255 and 15 bytes of
UTF-8). The version of the `FlashNX.nro` chosen shows
beside its field, and by default the window takes the newest one it finds
(next to the script, where it runs, then `cpp/FlashNX.nro` in a checkout).

It names the file and its folder without spaces (`My_Game.nro` in
`sdmc:/switch/My_Game/`): a HOME Menu forwarder made with Sphaira does not start
an `.nro` whose path has a space in it ("the software was closed because an
error occurred", before the app even runs; Sphaira's `owo.cpp` passes the path
unquoted). The Homebrew Menu and Sphaira show the app's name, not the file's.

### From source: `cpp/romfs/`

A `.swf` dropped into **`cpp/romfs/`** is built into the binary.

```bash
mkdir -p cpp/romfs
cp ~/games/MyGame.swf cpp/romfs/     # exactly one .swf at the root
./scripts/build.sh                   # cpp/FlashNX.nro now embeds it
```

`cpp/romfs/` is absent from a fresh clone and git-ignored, so the normal build is
unaffected: the Makefile only adds `--romfsdir=` to the `elf2nro` line when that
folder exists **and** holds something.

To make it look like its own app rather than a copy of FlashNX, edit the identity
block at the top of [cpp/Makefile](cpp/Makefile): `TARGET` (the output file
name), `APP_TITLE`, `APP_AUTHOR`, `APP_VERSION`, `ICON`.

Multi-file games work: put the companions in `cpp/romfs/<name>.files/`, exactly
the layout the `SidecarNavigator` already expects on the SD card.

What to know before shipping one:

- **The file name is the identity.** Per-game controls, display mode, zoom,
  cursor speed and play time are filed on the SD card under the SWF's basename
  (`sdmc:/flashnx/<name>.prefs`, `<name>.keymap.json`, ...), never inside the
  RomFS, which is read-only. Two self-contained apps built from files with the
  same name would share those files on one console, so give the game a name of
  its own. That is also why the boot code scans `romfs:/` instead of expecting a
  fixed `game.swf`.
- **Global settings are still shared.** `sdmc:/switch/FlashNX/` is hardcoded
  throughout, so UI language and the global REGLAGES defaults come from the same
  files FlashNX itself uses. Only the per-game layer is separate. A player who
  changes FlashNX's defaults, or a later FlashNX that writes those files
  differently, can therefore change how the game starts.
- **It stays on the FlashNX version it was made from.** Later fixes reach it
  only when it is made again from a newer `FlashNX.nro`.
- **Opened from the Album** (applet mode, a small part of the memory), it says
  so and how to start it with all of it (hold R while starting a game, then pick
  it in the Homebrew Menu), instead of running out of memory. Apps made from
  1.9.0 still open the library there, which can crash.
- **A launch argument still wins.** A `.swf` passed on the command line (a
  Sphaira forwarder) overrides the embedded game, which is a cheap way to test
  another file against the build without rebuilding it.
- **QUITTER exits to the HOME menu**, since there is no library to return to.
- **The Sphaira `.swf` association is not registered** by a self-contained build:
  it can only play the game it carries.
- **Size.** The SWF is stored uncompressed, so the `.nro` grows by roughly the
  size of the game on top of the ~16 MB player. It is read out of the `.nro`
  file at load time, not held in memory before that, so RAM use matches reading
  the same game off the card.
- **Swapping the game re-runs `elf2nro` on its own.** With a RomFS configured the
  Makefile drops the `.nro` before recursing, because neither the `.elf` nor any
  `.o` changes when you replace the `.swf` and make would otherwise report
  nothing to do while shipping the old game.

## HOME-menu tiles

A tile is a real installed title, not an entry in a homebrew launcher. `cpp/src/nsp.cpp`
builds three NCAs and hands them to `ncm`, then adds an application record through `ns`,
which is what the home screen actually reads.

- **program**: two sections. An ExeFS, built by `forwarder/` and embedded with `bin2s` by
  `scripts/build.sh`, and a RomFS holding the game's path. The binary is the same for
  every tile and holds no game path: it reads its own program ID and passes
  `sdmc:/switch/FlashNX/tiles/<id>.cfg` to FlashNX, which reads the `.swf` path out of
  that file. Argv is re-split on spaces by the homebrew ABI and game names have spaces in
  them; a program ID never does.
- **control**: a RomFS with `control.nacp` (the name) and the cover as a 256x256 JPEG.
- **meta**: a PFS0 with the `.cnmt` listing the other two.

Five things cost a whole debugging session each. They share one symptom, which is why:
the install succeeds, the tile appears with its name and its cover, and launching it does
nothing at all -- no process is ever created, so there is no crash report either.

**The program NCA needs a RomFS, not just an ExeFS.** Both working forwarders on hand
carry one (Sphaira: ExeFS + RomFS; hacBrewPack's: ExeFS + RomFS + logo). The logo really
is optional -- hacBrewPack documents `--nologo`.

**`forwarder.json` descends from `hbl.json`, and HBL runs as an applet.** It therefore
declares `"application_type": 2` (Applet). An installed application needs **1**. In the
built NPDM this is the `MiscParams` capability.

**An IVFC hash layer is declared at its padded size**, `0x4000`, not at the size of the
hashes it holds, and the master hash covers that whole padded block. The data layer keeps
its real size. The bytes are identical either way, so anything reading them back the way
they were written agrees with itself -- the console does not.

**The CNMT exists in two shapes.** The one inside the meta NCA has a 0x20 header, carries
a hash per content, and does not list the meta NCA itself; its content records are a 0x20
hash followed by an `NcmContentInfo`, so `attr` is at 0x35 and `content_type` at 0x36. The
one handed to `ncmContentMetaDatabaseSet` has a 0x8 header, no hashes, does list the meta
NCA, and has those fields one byte earlier.

**The NACP is read before the process is created.** `rating_age` set to 0xFF everywhere
means "rated nowhere", which parental control has nothing to compare against. `logo_type`,
`logo_handling`, `seed_for_pseudo_device_id` and `local_communication_id` are all copied
from a forwarder the console accepts rather than reasoned about.

Program IDs use the `0x05` prefix, which is what Sphaira uses today. DBI warns that such an
ID "belongs to an add-on"; that warning also fires on forwarders that launch perfectly, so
it is not a lead.

### Known limitation: quitting shows an error

Closing a game from a tile returns to the HOME menu, but the console shows "the software
was closed because an error occurred" first. This is the long-standing complaint about NSP
forwarders generally. The cause is structural: nx-hbloader has a hand-written `__appInit`
that never opens an applet session, because it is normally loaded by an applet that
already owns one. A tile IS the application, so once FlashNX closes its own session on the
way out, the process briefly exists holding none.

Sphaira's forwarder does not have the problem: its loader is a plain libnx program, which
opens a session at startup. Simply opening one in `forwarder/` does not work -- the loader
and FlashNX each link their own libnx, so FlashNX then asks for a second session in the
same process and fails to start at all. Replacing nx-hbloader with a minimal loader would
hit the same wall. Whatever Sphaira's 20 KB loader does, it is not that, and finding out
means disassembling it.

### Checking the output without installing it

Create `sdmc:/switch/FlashNX/nsp_dump.on`. Installing a tile then also writes
`sdmc:/switch/FlashNX/tiles/<id>.nsp`, before anything touches the console's storage, so
the file exists even when the install that follows fails. Copy it off and run `hactool`
over it: an independent implementation reading the same bytes is the point, which is why
there is no home-grown verifier here. It also installs with any other tool, which
separates "the file is wrong" from "this console would not take it".

The faster move, when a tile misbehaves, is to dump a forwarder that works (DBI, "dump to
SD card") and compare the two files field by field. Every one of the five above was found
that way, and none of them by reasoning about the format.

## Build & netload

```bash
# 1. BUILD (from Git Bash, at the repo root)
./scripts/build.sh            # release: LTO=full, ~3 min, official .nro
./scripts/build.sh --dev      # release-dev: LTO=thin + codegen-units=16, ~30 s rebuild
#   Equivalents: `make` (= build.sh) / `make dev` (= build.sh --dev) /
#   `scripts\build.ps1 [--dev]` from PowerShell. All delegate to build.sh.

# 2. NETLOAD (Switch: Homebrew Menu → Y to switch to netloader)
nxlink -s -p FlashNX/FlashNX.nro cpp/FlashNX.nro
#   The netloader does NOT run from RAM — it WRITES the received .nro to the SD
#   card under the hbmenu root (sdmc:/switch) and runs it from there. Without
#   `-p`, nxlink sends just the basename → it lands as sdmc:/switch/FlashNX.nro
#   (loose) and overwrites any installed copy there. `-p FlashNX/FlashNX.nro`
#   saves it to sdmc:/switch/FlashNX/FlashNX.nro (next to its config dir).
#   `-s` keeps the terminal attached to the Switch's stdout.
```

> ⚠️ `scripts/build.sh` is **the only supported build path**. Do not call
> `cargo build --target aarch64-unknown-linux-gnu` (wrong target → libc
> `target_env=gnu` failure). The correct target (`aarch64-nintendo-switch-freestanding`,
> tier-3, build-std) is provided by `rust/.cargo/config.toml`; build.sh runs
> `cargo build` without `--target`. The root `Makefile` and `build.ps1` are just
> thin wrappers around build.sh.

> 💡 The `-s` flag of `nxlink` keeps the terminal attached to the Switch's stdout:
> that's where the perf heartbeat (`f1234: fps=… tick=…ms
> render=…ms …`) and the `SLOW f…` lines from the slow-frame detector show up (see
> [rust/src/lib.rs](rust/src/lib.rs) `render_frame_with_dt`).

The script orchestrates:
1. `cargo build --release` (or `--profile release-dev`) on the Rust side (target `aarch64-nintendo-switch-freestanding`, std-via-newlib, build-std nightly) → `rust/target/.../libruffle_switch.a`.
2. `make` on the C++ side launched **inside devkitPro's MSYS2 bash** (so that `switch_rules` resolves the paths) → links against `libruffle_switch.a` + libnx + libEGL/libGLESv2 → `cpp/FlashNX.nro`.

The Makefile has `libruffle_switch.a` as an explicit dependency of the `.elf`, so any Rust change triggers the C++ relink automatically (no more manual `make clean`). The `release-dev` profile is selected via the `RUST_PROFILE` env var that `build.sh --dev` exports.

## Testing on Switch

1. Copy your `.swf` files to the SD card in **`sdmc:/flashnx/`** (or wherever you pointed SETTINGS > GAMES FOLDER). Any file name works. The legacy `sdmc:/ruffle/` is also scanned for backward-compat. At boot the `.nro` opens the **FlashNX library**: list of detected `.swf` files with banner + per-game color chip.
   - **LOCAL inputs**: up/down = navigate (D-pad **or left stick or right stick** — **hold to scroll fast**), **A** = PLAY, **X** = OPTIONS (TOUCHES + RENAME + BACK), **Y** = toggle to REMOTE mode (archive.org import), **−** = quit the `.nro`.
   - **REMOTE mode**: **A** = enter an archive.org URL via the soft keyboard (`swkbd`); on DistantIdle the history is shown, **L/R** cycle through it, **ZR** = directly fetch the displayed URL without reopening the keyboard. On the remote file list, an `OK` badge next to the ones already on SD, **A** starts the download (silent no-op on the `OK` ones). Live progress bar, **B** cancels in progress.
   - **Empty state**: if the SD card is empty, the library shows "NO GAME" + instructions on where to put the `.swf` files. **Y** lets you go to REMOTE to download.
   - **Ultimate fallback** (library init fail): `ruffle_init` with a 43-byte embedded SWF (red background). The `.nro` never refuses to boot.
2. Switch in **netloader** mode: Homebrew Menu → `Y` (or `R` on older versions).
3. PC: `nxlink -s -p FlashNX/FlashNX.nro cpp/FlashNX.nro`.

**In-game controls** (default platformer binding; remappable via TOUCHES):

| Joycon | Flash action |
|---|---|
| A | Space (main jump) |
| B | Z (alt jump) |
| X | X (run/dive) |
| Y | Shift (alt run) |
| Left stick / D-pad | Arrows |
| **Right stick** | **Mouse cursor** (visible crosshair) |
| **ZR** or **touchscreen** | **Mouse click** |
| R | Enter ("Press Start") |
| L | Escape |
| Plus | P (standard pause key) |
| **Minus** | **Opens the pause menu** (RESUME / TOUCHES / RESTART / QUIT) |

In the pause menu: D-pad **or either stick** to navigate (a held direction scrolls and stops at the ends; a fresh Up on the first row goes to the last), **A** acts on a row or arms a setting (Left and Right then change it, live, **A** keeps it, **B** puts back what was there), **B** goes back a level, **Minus** resumes the game from any page. **"QUIT" returns to the FlashNX library** (from the library, **−** = exit the `.nro`). "RESTART" reloads the SWF from scratch (keeps the `.sol` files), "TOUCHES" opens the controls page and its editor.

## Key customization

Two ways — the in-game editor is by far the simplest.

### In-game "TOUCHES" editor (recommended)

From the pause menu (**Minus** in game) or from a game's options in the library (**+** on its cover), open **Touches et curseur**, then **Modifier les touches**. The editor draws the controller with one row per button beside it (under it when the game is turned), and lights the selected button in both places. **A** on a row opens a PC keyboard: any of the 88 Flash keys, a mouse click, the Switch keyboard, or nothing. **X** switches between player 1 and player 2, **L** and **R** between the Normal layer and the four combo layers (hold ZL, ZR, L or R in game), **Y** opens the shared profiles, **B** goes back.

Each binding applies at once (no need to RESTART) and is saved to **`sdmc:/flashnx/<basename>.keymap.json`**, per game, without touching the global default (Settings › Game defaults › Controls edits that one).

### Manual JSON editing (power users)

The `.nro` reads / writes JSON files on the SD card.

**Lookup hierarchy** (first hit wins):
1. `sdmc:/flashnx/<basename>.keymap.json` — per-game override
2. `sdmc:/ruffle/<basename>.keymap.json` — legacy backward-compat
3. `sdmc:/flashnx/keymap_default.json` — global default chosen by you
4. `sdmc:/ruffle/keymap_default.json` — legacy
5. Hardcoded fallback in the `.nro` — the table above

On first boot, if no `keymap_default.json` exists anywhere, the `.nro` writes it to `sdmc:/flashnx/keymap_default.json` with the hardcoded fallback.

**Schema** (example):
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

**Supported Switch buttons**: `A`, `B`, `X`, `Y`, `L`, `R`, `ZL`, `Plus`, `Up`/`Down`/`Left`/`Right` (D-pad), `StickLUp`/`StickLDown`/`StickLLeft`/`StickLRight`. Buttons not listed = unbound. `Minus` is reserved for the pause menu. `ZR` is reserved for the in-game mouse click (available in the library in REMOTE mode for fetch-without-keyboard).

On each boot the `.nro` logs the final resolution via nxlink (`keymap: resolved 16 bindings: A=1 B=8 ...`).

## Display name customization (RENAME)

To rename a game **without touching the physical `.swf` file** (saves + keymap stay stable):

1. In the library, select the game, **X** = OPTIONS → **RENAME** + A.
2. swkbd opens pre-filled with the current name — edit it.
3. Submit → writes `sdmc:/flashnx/<basename>.meta.json` with `{"display_name": "..."}`. Empty field = deletes the sidecar = back to the basename.

The library reads this sidecar on every SD scan. The metadata panel still shows `[basename.swf]` in small text. **Steam/ScummVM/iTunes pattern**: display alias only. `.sol` saves, keymap and SharedObject URLs (`http://flashforswitch.local/<basename>`) stay stable.

## SD card layout

Everything lives **flat** in `sdmc:/flashnx/`:

```
sdmc:/flashnx/
├── Super_Mario_63_2010.swf                           ← the game itself
├── Super_Mario_63_2010.swf.keymap.json               ← per-game keymap
├── Super_Mario_63_2010.swf.meta.json                 ← display_name (rename)
├── Super_Mario_63_2010.swf.<SaveName>.sol            ← save (flat)
├── Garfield's Scary Scavenger Hunt.files/            ← companion SWFs for a multi-file game (loaded on demand)
│   ├── top.swf
│   └── scavengerhunttitle.swf, books.swf, ...
├── keymap_default.json                               ← global default keymap (edited via + → DEFAULT CONTROLS)
├── settings.json                                     ← UI language (EN/FR/ES/RU), set via + → LANGUAGE
└── ...
```

The **`+` button** in the library opens a global Settings modal: **DEFAULT CONTROLS** (edits `keymap_default.json` with the same editor as the per-game TOUCHES screen) and **LANGUAGE** (writes `settings.json`; auto-detected from the console language on first boot — see [rust/src/loc.rs](rust/src/loc.rs)).

**Backward-compat**: files in the old `sdmc:/ruffle/` are detected and used (scan + read-fallback). Saves in the old nested tree `sdmc:/ruffle/saves/<host>/<basename>/<sol>.sol` are read and then automatically migrated to the flat path on the next save.

**Multi-file games** (companion `<game>.files/` folder): a game that loads other SWFs (`loadMovie` / `GetURL` into `_levelN`) reads them from `sdmc:/flashnx/<game-stem>.files/`. For `Foo.swf` that is `Foo.files/`, and a relative load of `top.swf` resolves to `Foo.files/top.swf`. Per-game (not flat) so two games can each ship their own `top.swf` without colliding. Sourcing the companions for a Flashpoint game: they are usually NOT in the db-api GameZIP (`/get?id=` may return only the main SWF). The full set lives in the Legacy htdocs, served public at `https://infinity.unstable.life/Flashpoint/Legacy/htdocs/<host>/<path>/<file>`. **Auto-fetched on download**: `gamezip::fetch_siblings` (called from `finish_gamezip`) does this automatically when a game is downloaded via the Flashpoint search (X), so no manual step is needed for that path. It derives the htdocs base from the GameZIP entry path, scans the main SWF for `.swf` references, fetches each into `.files/`, and follows companions breadth-first (a companion that loads further SWFs is scanned too). Manual placement is only needed for games added another way (URL import, hand-copied). See [rust/src/backend/navigator.rs](rust/src/backend/navigator.rs) and [rust/src/sources/gamezip.rs](rust/src/sources/gamezip.rs).

**System files** (in the homebrew folder):
- `sdmc:/switch/FlashNX/cacert.pem` — Mozilla CA bundle for HTTPS
- `sdmc:/switch/FlashNX/distant_history.json` — archive.org URL history (persisted)
- `sdmc:/switch/ruffle-crash.log` — replay of the last native crash (seen at the next boot via nxlink)

## Known limitations

For playing common AS1/AS2 SWFs, it's functionally near-complete. Honest inventory of what's left.

### Rendering (backend)

| Missing | Impact |
|---|---|
| **Filters** GradientGlow / GradientBevel / Convolution / DisplacementMap | Dropped (passthrough). Done: ColorMatrix / Blur / Glow / DropShadow / Bevel. |
| **`BitmapData.draw()`**: clear-transparent (no composite over the existing content), temp texture per call (no pool) | OK for the tile-engine pattern (SMWF), not faithful for all BitmapData uses. |
| **Blend modes** Alpha / Erase (need layer tracking); nested blend (non-recursive offscreen FBO) | Trivial (Add/Screen…) and Complex (Multiply/Overlay…) are done. |
| **Filter perf in menu transitions** (N FBO passes/frame) | Hiccups on heavily filtered menus (Mario 63). Bounded by budget/frame + pool TTL. Real fix = batching the passes. |
| **Context3D / Stage3D** (AS3) | Implemented since v1.8.0: AGAL shaders are translated to GLSL ES and run on the GL backend. |
| **PixelBender** (AS3 `Shader` / `ShaderFilter` / `ShaderJob`) | No GL execution (wgpu-only upstream). Degrades gracefully: `compile_pixelbender_shader` returns a parsed-shader handle so AVM2 construction succeeds, the renderer skips `Filter::ShaderFilter`, `ShaderJob` no-ops (patch 0002). The effect is absent but the game runs (e.g. The Terminal). |

### Ruffle core (out of our scope)

- **Perf of heavy games**: what is left is mostly script execution. AS3 is compiled to native ARM code (a baseline JIT, see `patches/README.md`), AS2 still runs in Ruffle's interpreter, and the pause menu's OVERCLOCK (AUTO by default) raises the CPU when a game falls behind. Two known walls: physics-heavy AS3 (Box2D) is bound by memory accesses to Ruffle's objects, and games that draw into a BitmapData and read it back every frame pay a GPU round trip (Super Bowser World).
- **AS3 / AVM2** — partially supported by Ruffle, worse perf (no JIT). `AS3` badge in the library.

### Platform / distribution

- **Savestate** — knowingly skipped: Ruffle does not expose a `Player::serialize()` (the state is a `gc-arena` graph). A real savestate = a ~2-week upstream effort. The native `.sol` saves work.
- **Library** — Deleting a game (with confirm), sorting, box art: not implemented.
- **hb-app.store packaging** + **extended user documentation** — for wider distribution.
- **Home menu forwarders** — doc-only ([Sphaira](https://github.com/ITotalJustice/sphaira) generates the NSP, no code on our side). ⚠️ Forwarders look like games to Nintendo: safe only on emuNAND.

## Constraints / facts to keep in mind

- **Pure interpreted AS2** (e.g. Mario 63) → AVM2 JIT not needed, Horizon `jit:u` service not required.
- **Ruffle deps**: `cpal`/`reqwest`/`tokio`/`wgpu` are **not** in `ruffle_core`, only in `ruffle_desktop` → nothing to neutralize. `flate2` workspace default = `miniz_oxide` (pure Rust). Everything links directly.
- **libnx FFI used**:
  - HID: `padConfigureInput`/`padInitializeDefault`/`padUpdate`/`padGetButtonsDown/Up`/`padGetStickPos`/`hidInitializeTouchScreen`/`hidGetTouchScreenStates` (in `cpp/src/main.cpp`)
  - Applet: `appletMainLoop`, `nwindowGetDefault`, `appletSetCpuBoostMode(FastLoad)`. The suspend/resume cycle (home button, sleep) is handled implicitly by `appletMainLoop` — no `appletHook` hooks needed in practice.
  - Socket: `socketInitializeDefault`, `nxlinkStdio` (network stdout)
  - FS: `sdmc:/...` auto-mounted by the libnx crt0 → `std::fs::read` works from Rust (except `read_dir` which is buggy — hence the C++ `opendir`/`readdir` scan)
  - Thread: `threadCreate`/`threadStart`/`threadWaitForExit`/`threadClose` (GL worker + audio worker)
  - System: `svcGetInfo` (RAM via `ruffle_query_ram`), `armGetSystemTick` (real dt pacing)
  - Audio: `audrenInitialize`/`audrvCreate`/`audrvVoiceInit`/`audrvVoiceAddWaveBuf`/`audrvUpdate`/… (cf. [cpp/src/audio.cpp](cpp/src/audio.cpp))
- **Bindgen**: not used. FFI hand-written in `rust/src/ffi/gl.rs` (GL 4.3 core subset) and `cpp/src/ruffle_bridge.cpp`.
- **Architecture pattern**: ScummVM `backends/platform/sdl/switch/` (OSystem → OSystem_SDL → OSystem_Switch separation). Adapted here: `RenderBackend` trait (from Ruffle) + thin `SwitchRenderBackend` impl.

## Toolchain

- **devkitPro** in `C:\devkitPro\` with packages `switch-dev`, `switch-mesa`, `switch-glm`, `switch-glad`, `switch-curl`, `switch-mbedtls`.
- **Rust**: toolchain pinned via `rust/rust-toolchain.toml` → `nightly-x86_64-pc-windows-gnu` + `rust-src` (GNU host required — MSVC breaks the build scripts without Visual Studio Build Tools).
- **MinGW-w64** via `scoop install mingw` — for `dlltool.exe` which Rust nightly GNU ships in a buggy state. `scripts/build.sh` adds `~/scoop/apps/mingw/current/bin` to the PATH before `cargo build`.
- **`third_party/jpeg-decoder-switchfork/`**: patched fork of `jpeg-decoder` 0.3.2 (`select_worker` always returns `Immediate`), referenced via `[patch.crates-io]` in `rust/Cargo.toml`. Without it, JPEGs > 128×128 px spawn a `std::thread` that crashes the newlib pthread shim.

### rust-src patches to re-apply after each `rustup update`

> **Shortcut**: `bash scripts/patch-rust-src.sh` re-applies both patches below (idempotent, safe to re-run) and runs `cargo clean` if anything changed. Run it after any `rustup update` / toolchain reinstall, then `bash scripts/build.sh`.

**Patch 1** — `…\nightly-x86_64-pc-windows-gnu\lib\rustlib\src\rust\library\std\build.rs`: add after the line `|| (target_vendor == "nintendo" && target_env == "newlib")`:

```rust
|| (target_vendor == "nintendo" && target_os == "horizon")
```

Without it, stdlib compiles in `restricted_std` mode → all the std crates from crates.io (memchr, num-traits, thiserror…) refuse to compile.

**Patch 2** — `…\nightly-x86_64-pc-windows-gnu\lib\rustlib\src\rust\library\std\src\hash\random.rs`: wrap the body of `RandomState::new()` in a cfg-switch:

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
        // ... original code (thread_local) ...
    }
}
```

Without it, `HashMap::new()` then `.insert()` crashes on hardware (the lazy stdlib thread_local with per-function init crashes on our target). Hash-flooding DoS not relevant for a Flash player.

**Environment gotchas**:
- Avast Web Shield (HTTPS scanning) intercepts pacman/pkg.devkitpro.org by injecting its root CA → disable HTTPS scanning before `pacman -Sy`.
- Avast CyberCapture flags the cargo build scripts compiled to `.exe` → add an exception on the project folder.
- The chocolatey `make` doesn't handle devkitPro MSYS-style paths → `scripts/build.sh` delegates to `/c/devkitPro/msys2/usr/bin/bash -lc 'make'`.
- Target `aarch64-nintendo-switch-freestanding` = tier-3 → no pre-built rust-std → `-Z build-std` → nightly required.

## Hardware

- Atmosphère-modded Switch.
- nxlink for network stdout (debug) + netloader (push `.nro` over WiFi).
- SD: copy the `.nro` to `sdmc:/switch/FlashNX.nro` for SD mode (not required if netloading).
- SWFs looked up first in `sdmc:/flashnx/` (see "Testing on Switch").

## References

- Ruffle: https://github.com/ruffle-rs/ruffle (`render/src/backend.rs` for the trait to impl)
- aarch64-switch-rs: https://github.com/aarch64-switch-rs/{nx,cargo-nx}
- libnx docs: https://switchbrew.github.io/libnx/
- ScummVM Switch (pattern reference): `backends/platform/sdl/switch/`
- GBAtemp Switch homebrew dev: https://gbatemp.net/forums/switch-homebrew-development.300/
