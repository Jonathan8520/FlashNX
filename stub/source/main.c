// FlashNX shortcut stub.
//
// A few hundred kilobytes of nothing whose entire job is to hand one `.swf`
// path back to FlashNX and get out of the way. FlashNX writes a copy of this
// per game into `sdmc:/switch/`, so hbmenu and Sphaira each list the game under
// its own name, and launching it starts the game directly.
//
// Why a stub at all, rather than installing a real HOME-menu tile: a tile needs
// an NCA forwarder, which means Atmosphere's ACID signature check (broken by
// every firmware update, and not disableable on retail) and lifting GPLv3 code
// into an MIT project. This has none of that. No `ncm`, no `ns`, no crypto, no
// sigpatches, nothing to ban. Deleting a shortcut is deleting a file.
//
// The stub is NOT rebuilt per game: FlashNX patches the target path into the
// copy it writes, then rewrites the copy's asset section so the name shown is
// the game's. Nothing compiles on the console.

#include <stdio.h>
#include <string.h>
#include <switch.h>

// Patched in place by FlashNX when it writes a copy (see
// `shortcut_write` in cpp/src/shortcut.cpp). The marker is what the writer
// searches for, so it must survive into the binary verbatim: `used` keeps the
// optimiser off it, `volatile` stops the compiler folding reads of it, and its
// own section keeps it out of a merged string pool where the writer could not
// tell it from a neighbour.
//
// Sized so a path can be long without a second thought: Horizon's own limit is
// 0x301 bytes and our games live under `sdmc:/flashnx/`.
#define TARGET_MARKER "FLASHNX_SHORTCUT_TARGET_V1:"
__attribute__((used, section(".rodata.flashnx_target")))
volatile const char g_target[1024] = TARGET_MARKER;

// Where FlashNX itself lives. Fixed, because the shortcut has to keep working
// after FlashNX is updated in place, which is the whole reason a shortcut
// delegates instead of embedding a player.
#define FLASHNX_NRO "sdmc:/switch/FlashNX/FlashNX.nro"

int main(int argc, char** argv) {
    (void)argc;
    (void)argv;

    // Read past the marker. Cast away volatile for the read: the qualifier is
    // there to defeat constant folding at compile time, not to describe the
    // memory.
    const char* target = ((const char*)g_target) + sizeof(TARGET_MARKER) - 1;

    // An unpatched stub (someone copied the template by hand) must not pretend.
    // Falling through to FlashNX with no argument opens the library, which is
    // the friendliest wrong answer available.
    char args[1152];
    if (target[0] == '\0') {
        strcpy(args, "FlashNX.nro");
    } else {
        // argv[0] is conventionally the program itself; the quoted path becomes
        // argv[1]. hbloader honours the quotes, so a game name with spaces
        // arrives as ONE argument, which is what FlashNX's scan expects.
        snprintf(args, sizeof(args), "FlashNX.nro \"%s\"", target);
    }

    // No graphics, no console, no romfs: this process exists for the length of
    // one syscall. If the environment cannot chain (started from a context with
    // no next-load support) there is nothing useful to do but leave.
    if (envHasNextLoad()) {
        envSetNextLoad(FLASHNX_NRO, args);
    }
    return 0;
}
