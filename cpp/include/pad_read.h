#pragma once
#include <switch.h>

// Reading the controllers, with a Joy-Con held on its own (issue #128).
//
// A Joy-Con given to a player by itself is held on its side, rail up, and the
// console reports it as it is upright: its stick a quarter turn off, its face
// buttons in other places, SL and SR where L and R would be. Nothing in HID
// turns it; devkitPro's SDL and RetroArch both turn it themselves, with the
// same tables used here. `flashnx_pad_update` reads like libnx's `padUpdate`
// and turns each lone Joy-Con before it is combined: its four face buttons
// become A B X Y by their place (right, bottom, top, left), its stick the left
// stick, its push the left stick's; SL, SR and its small button (- or +) stay
// what they are. The controls editor draws the Joy-Con with exactly these
// rows (`keymap::PadLayout`), so each button does what its row says.

// What `flashnx_pad_update` read, besides the state it left in the pad.
enum : u32 {
    PAD_READ_SINGLE = 1,  // at least one source was a Joy-Con on its own
};

// `flashnx_pad_update`'s flags.
enum : u32 {
    // Player 1's reads: a right Joy-Con on its own has no `-`, so its small
    // button, +, is its `-` and opens the pause, as the left one's `-` does.
    PAD_FLAG_PLAYER1 = 1,
    // Menu reads (the launcher, the pause frozen in a script): a lone
    // Joy-Con's SL / SR are its shoulders, L / R, which every menu of FlashNX
    // pages with (tabs, the editor's layers). In the game they stay SL / SR.
    PAD_FLAG_SHOULDERS = 2,
};

// What `PAD_FLAG_SHOULDERS` does, for a state read without it: SL as L, SR as R.
inline u64 pad_lone_shoulders(u64 m) {
    const u64 sl = HidNpadButton_LeftSL | HidNpadButton_RightSL;
    const u64 sr = HidNpadButton_LeftSR | HidNpadButton_RightSR;
    u64 out = m & ~(sl | sr);
    if (m & sl) out |= HidNpadButton_L;
    if (m & sr) out |= HidNpadButton_R;
    return out;
}

// `log`: say in the log what a lone Joy-Con sends, raw and turned, whenever it
// changes (the game loop's reads only: the `-` watch thread reads too).
u32 flashnx_pad_update(PadState* pad, u32 flags, bool log = false);

// What player `player` (1 to 4) holds now: 0 nothing, 1 a full controller (a
// Pro controller, a Joy-Con pair, the console in hand), 2 a left Joy-Con on
// its own, 3 a right one. The controls editor draws that controller.
extern "C" int flashnx_player_pad_kind(int player);

// The console's controller screen, where each player takes a controller (SL+SR
// on a Joy-Con on its side). Blocks until it closes. Returns the player count
// it confirmed, or -1 when it was left or could not open.
extern "C" int flashnx_controller_applet(int max_players);
