# game-music-emu (FlashNX copy)

Blargg's video game music emulator library, run natively by FlashNX in place
of the copy a game compiled to ActionScript with CrossBridge (FlasCC).

- Upstream: https://github.com/libgme/game-music-emu, release `0.6.5`
  (2026-04-20), files from its `gme/` directory.
- License: LGPL 2.1 or later, in `LICENSE.txt` (the library) and each source
  file's header; `ext/emu2413.c` is MIT (`ext/LICENSE-emu2413`). FlashNX's own
  source is public, which is what the LGPL asks of a program it is linked into
  statically: anyone can rebuild it against a changed copy of this library.

## Why

Super Mario Bros. Crossover plays all its music and records its sound effects
with libgme, compiled to ActionScript (Adobe's CrossBridge `sample.libgme`).
Emulating the SNES sound chip that way took ~60 ms for every 186 ms of music on
the Switch at 1785 MHz, in one `sampleData` handler: the game stalled every ~11
frames and ran at 0.6-0.9x its speed with its SNES music. Native, the same
library plays an SPC at ~7 % of a 400 MHz Mac (snes_spc's README).

Our Ruffle replaces the static methods of the game's `libgme` facade class with
natives (`core/src/avm2/flashnx_gme.rs`), which call this library through
`rust/src/gme.rs`. The `gmenative.off` marker keeps the ActionScript copy.

## What is here

- The emulators the game asks for: GBS, KSS, NSF, NSFE, SAP, SPC
  (`gme_types.h`, which replaces the file CMake generates). The `.cpp` files are
  the ones upstream's `CMakeLists.txt` lists for those types; every header of
  `gme/` is kept.
- `blargg_config.h`: `NDEBUG` is defined first, so the library is built without
  asserts as upstream's release builds are (FlashNX's C++ keeps its own), and a
  failed `require` does not abort the app over a game's music file. It also
  defines `BLARGG_LITTLE_ENDIAN`, which upstream's CMake provides. Nothing else
  differs from upstream.
