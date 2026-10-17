#include <switch.h>
#include <cstdio>
#include <cstdlib>
#include <cstdint>
#include <cstring>
#include <cmath>
#include <atomic>
#include <strings.h>
#include <sys/stat.h>

#include "ruffle_bridge.h"
#include "pad_read.h"

extern "C" bool gl_context_init(NWindow* win);
extern "C" void gl_context_shutdown(void);
extern "C" void gl_context_swap(void);

// ── Boot timing (issue: ~4 s of black screen before the library shows) ──────
// System-tick stamps so every boot phase reports its own cost instead of us
// guessing which one owns the wait. Cheap (one armGetSystemTick per phase).
static uint64_t g_boot_t0 = 0;
static double boot_ms_since(uint64_t t) {
    return (double)(armGetSystemTick() - t) * 1000.0 / (double)armGetSystemTickFreq();
}

// ── Whole-frame timing for the SLOW line ─────────────────────────────────────
// The SLOW line adds up tick + render, and the swap sits outside both. On this
// GL driver a texture freed while the pending command buffer still uses it is
// released at the next flush, after a blocking wait on that submission, which
// is normally inside eglSwapBuffers. These two let the Rust side print the
// previous frame's swap and its full loop period, so a cost that moves out of
// `render` into the swap is still seen. Game loop only; ticks.
static uint64_t g_prev_swap_ticks = 0;
static uint64_t g_prev_period_ticks = 0;
extern "C" uint64_t flashnx_prev_swap_ticks(void) { return g_prev_swap_ticks; }
extern "C" uint64_t flashnx_prev_period_ticks(void) { return g_prev_period_ticks; }

// ── `-` watched between frames ───────────────────────────────────────────────
// The game loop reads the buttons once per frame, and only as they are at that
// instant (padUpdate keeps the latest state alone): on a game slow enough, a
// `-` pressed and let go between two frames was never seen, and the pause did
// not open (Jonathan, 2026-10-15: "je veux juste que le modal s'ouvre meme si
// le jeu lag"). This thread reads `-` every 10 ms with its own PadState and
// keeps a press until the loop takes it. A game frozen in a script still opens
// the pause only once its frame ends: nothing is drawn before that.
static Thread            s_minus_thread;
static std::atomic<bool> s_minus_running{false};
static std::atomic<bool> s_minus_pressed{false};

static void minus_watch_entry(void*) {
    PadState wpad;
    padInitializeDefault(&wpad);
    while (s_minus_running.load(std::memory_order_relaxed)) {
        // Through the turn of a lone Joy-Con (issue #128): a right one's +
        // is its `-`.
        flashnx_pad_update(&wpad, PAD_FLAG_PLAYER1);
        if (padGetButtonsDown(&wpad) & HidNpadButton_Minus) {
            s_minus_pressed.store(true, std::memory_order_relaxed);
        }
        svcSleepThread(10'000'000LL);
    }
}

// While a game runs. Off the game's core when it can be (core 2, as audio).
static void minus_watch_start(void) {
    if (s_minus_running.load()) return;
    s_minus_pressed.store(false);
    s_minus_running.store(true);
    Result rc = threadCreate(&s_minus_thread, minus_watch_entry, nullptr, nullptr, 16 * 1024, 0x2C, 2);
    if (R_FAILED(rc)) {
        rc = threadCreate(&s_minus_thread, minus_watch_entry, nullptr, nullptr, 16 * 1024, 0x2C, -2);
    }
    if (R_FAILED(rc)) {
        s_minus_running.store(false);
        std::printf("minus watch: threadCreate failed 0x%x, - is read once per frame only\n", rc);
        std::fflush(stdout);
        return;
    }
    threadStart(&s_minus_thread);
}

static void minus_watch_stop(void) {
    if (!s_minus_running.exchange(false)) return;
    threadWaitForExit(&s_minus_thread);
    threadClose(&s_minus_thread);
}

// ── The pause opened inside a frame that computes ───────────────────────────
// A game frozen in a script (a loading, a calculation) holds the game thread
// for up to 60 s, and the loop below cannot draw the pause until it lets go.
// Ruffle asks the Rust side at its timeout checks, inside the script
// (`script_hook`), which draws the pause there while the script waits, as
// Flash Player did with its "a script is running slowly" dialog. These are
// its buttons and the applet's messages meanwhile.
extern "C" int flashnx_take_minus_pressed(void) {
    return s_minus_pressed.exchange(false, std::memory_order_relaxed) ? 1 : 0;
}

// Whether a `-` waits for the loop, left for it: the game's file loads stop
// between two (`run_until` in lib.rs) so the pause opens sooner.
extern "C" int flashnx_minus_pending(void) {
    return s_minus_pressed.load(std::memory_order_relaxed) ? 1 : 0;
}

static PadState s_frozen_pad;
static bool     s_frozen_pad_ready = false;

// Start from the buttons as they are: one still held is not a press.
extern "C" void flashnx_frozen_pad_reset(void) {
    if (!s_frozen_pad_ready) {
        padInitializeDefault(&s_frozen_pad);
        s_frozen_pad_ready = true;
    }
    flashnx_pad_update(&s_frozen_pad, PAD_FLAG_PLAYER1 | PAD_FLAG_MENU);
    flashnx_pad_update(&s_frozen_pad, PAD_FLAG_PLAYER1 | PAD_FLAG_MENU);
}

// Presses since the last call: 1 A, 2 B, 4 `-`, 8 up, 16 down (d-pad or
// either stick), 32 `-` or B still held.
extern "C" uint32_t flashnx_frozen_pad_down(void) {
    flashnx_pad_update(&s_frozen_pad, PAD_FLAG_PLAYER1 | PAD_FLAG_MENU);
    const u64 d = padGetButtonsDown(&s_frozen_pad);
    const u64 h = padGetButtons(&s_frozen_pad);
    uint32_t out = 0;
    if (d & HidNpadButton_A) out |= 1;
    if (d & HidNpadButton_B) out |= 2;
    if (d & HidNpadButton_Minus) out |= 4;
    if (d & (HidNpadButton_Up | HidNpadButton_StickLUp | HidNpadButton_StickRUp)) out |= 8;
    if (d & (HidNpadButton_Down | HidNpadButton_StickLDown | HidNpadButton_StickRDown)) out |= 16;
    if (h & (HidNpadButton_Minus | HidNpadButton_B)) out |= 32;
    return out;
}

// The applet's messages (HOME, sleep, closing): 0 once FlashNX must exit.
extern "C" int flashnx_applet_ok(void) {
    return appletMainLoop() ? 1 : 0;
}

// Sampling profiler (cpp/src/prof.cpp). Inert unless `prof.on` is on the card.
extern "C" void prof_boot(Thread* worker);
extern "C" void prof_game_active(int active);
extern "C" void prof_frame(void);
extern "C" void prof_shutdown(void);
extern "C" void prof_set_regime(int regime);
// The profiling build's A/B switch (rust/src/lib.rs).
extern "C" void ruffle_ab_regime(int regime);
// libnx (weak, 0 by default): how `appletExit` leaves, see the end of main().
extern "C" u32 __nx_applet_exit_mode;

extern "C" void swf_picker_run(void);
// The `.swf` embedded in this `.nro` (issue #106), or nullptr on a normal
// build. Only meaningful after romfsInit(); see cpp/src/swf_picker.cpp.
extern "C" const char* swf_picker_embedded_swf(void);
// 1 when we only have the album's small memory pool. Defined in
// cpp/src/ruffle_bridge.cpp; the library flow already refuses to launch a game
// on it, and the forwarder path has to do the same.
extern "C" int ruffle_is_applet_mode(void);
extern "C" void https_thumb_stop(void);
extern "C" void https_job_stop(void);
extern "C" void flashnx_update_check_stop(void); // updater.cpp
// swf_picker.cpp: a deleted game's companion folder is emptied in the background.
extern "C" void swf_picker_trash_start(void);
extern "C" void swf_picker_trash_pause(void);
extern "C" void swf_picker_trash_frame(int input);
extern "C" void swf_picker_trash_stop(void);

// Phase 3.4 — library boot screen FFI (rust/src/library.rs).
extern "C" void ruffle_loc_init(void);
extern "C" int  ruffle_library_init(void);
extern "C" int  ruffle_library_add_path(const char* path, unsigned long long mtime);
extern "C" void ruffle_library_open(void);
extern "C" int  ruffle_library_active(void);
extern "C" int  ruffle_library_picked(void);
extern "C" int  ruffle_library_input(const char* button_name);
extern "C" void ruffle_nav_repeat(int is_repeat);
extern "C" void ruffle_library_nav_held(unsigned char mask);
extern "C" void ruffle_library_touch(float x, float y, int pressed);
extern "C" void ruffle_library_render(void);
extern "C" int  ruffle_library_selected_path(char* out, int cap);
extern "C" void ruffle_library_shutdown(void);
extern "C" void ruffle_library_reset(void);

extern "C" void ruffle_handle_key(int code, bool down);
extern "C" void ruffle_handle_mouse_move(int x, int y);
extern "C" void ruffle_handle_mouse_button(bool down);
extern "C" void ruffle_handle_mouse_right(bool down);
extern "C" void ruffle_redraw_paused(void);
// The same for the pause pages that change nothing in the picture (the main
// panel, TOUCHES, the closing slide): the game is drawn once and kept. ECRAN and
// the framing mode keep `ruffle_redraw_paused`, which shows their changes live.
extern "C" void ruffle_redraw_paused_frozen(void);
// What the pause opened inside a computing frame was left with: 0 Resume (or
// nothing), 1 Quit, 2 Restart. The script was stopped for 1 and 2.
extern "C" int  ruffle_take_frozen_request(void);
// Hide an interval of wall clock from the movie (#87). Frames only advance on
// the dt we hand to ruffle_render_frame_dt, but getTimer() reads the real
// clock, so a pause the player never simulated comes back as a jump for any
// game that times itself from getTimer() deltas.
extern "C" void ruffle_skip_paused_time(uint64_t us);
// One SWF frame in microseconds (clamped), for the touch click delay (#87).
extern "C" uint64_t ruffle_frame_interval_us(void);
// What is on stage, two levels deep, one line each. Fired once a few seconds
// into a game (#100), so a black-screen report carries the answer.
extern "C" void ruffle_dump_stage_children(void);
extern "C" void ruffle_dump_root_vars(void);
extern "C" void ruffle_alloc_force_off(void);
extern "C" bool ruffle_dlmalloc_force_off(void);
// src/flashnx_heap.c: how the heap is split between newlib and the top region.
extern "C" void flashnx_heap_stats(uint64_t* out);
extern "C" int malloc_trim(size_t pad);

// CPU power mode — see ruffle_bridge.cpp for what it does and what the
// evidence for it is. Default is mode 0, which touches nothing. The value is
// per game, applied on the Rust side at load, and the pause menu's OVERCLOCK
// row toggles it. Setters report the mode ACTUALLY in force, which is not
// always the one asked for.
extern "C" int  flashnx_set_clock_mode(int mode);
extern "C" int  flashnx_clock_mode(void);
extern "C" int  flashnx_clocks_reassert(void);
extern "C" uint32_t ruffle_cpu_clock_hz(void);
extern "C" uint32_t ruffle_gpu_clock_hz(void);
extern "C" void flashnx_clocks_restore(void);

// True when `sdmc:/switch/FlashNX/dumpvars.on` exists: the pause button then
// also prints the movie's AVM1 variables (see the `-` handler). Same shape as
// the `trace.on` marker in ruffle_bridge.cpp, and for the same reason — a
// diagnostic nobody should pay for unless they asked for it. Checked once.
static bool dump_vars_on_pause(void) {
    static bool checked = false;
    static bool on = false;
    if (!checked) {
        checked = true;
        struct stat st;
        on = (::stat("sdmc:/switch/FlashNX/dumpvars.on", &st) == 0);
    }
    return on;
}

extern "C" void ruffle_draw_menu(int selected);
extern "C" void ruffle_draw_screen_menu(int selected);
extern "C" void ruffle_menu_close_begin(void);
extern "C" int  ruffle_draw_menu_closing(int selected);
extern "C" int  ruffle_restart(void);
// The value rows of the pause and of ECRAN (OVERCLOCK, AFFICHAGE, ROTATION,
// PIXELS, FILTRE), by panel (UI_KIND_PAUSE / UI_KIND_SCREEN) and row. Input
// rule 1: A ARMS such a row, and only an armed row takes Left / Right; A
// keeps the value, B puts back exactly what was there. The value is applied
// to the game LIVE as it moves, so the frozen frame behind the panel shows
// it, and written to the game's .prefs only when kept (lib.rs, the
// pause_values module). The drawing reads the same armed state.
extern "C" void ruffle_pause_opened(void);              // a fresh pause: nothing armed
extern "C" int  ruffle_value_arm(int kind, int row);    // 1 = armed (a value row)
extern "C" int  ruffle_value_armed(void);               // 1 = a row is armed
// Step the armed value: 1 = it moved, 0 = against an end (silent), -1 = the
// console refused it (a raise of the clock), which says so with a sound.
extern "C" int  ruffle_value_step(int dir);
extern "C" int  ruffle_value_confirm(void);             // A, Up / Down, closing
extern "C" int  ruffle_value_cancel(void);              // B
// A on a switch row (the FPS counter): flips it and keeps it.
extern "C" int  ruffle_value_toggle(int kind, int row);
// X: the game drops its own value and follows Reglages again (1 = it had one).
extern "C" int  ruffle_value_reset(int kind, int row);
// Close audren and join its worker. Idempotent; called on the way out, since
// the launcher's interface sounds open a session no game will close.
extern "C" void ruffle_audio_shutdown(void);
// Free zoom + framing (issue #101). Unlike the three rows above, ZOOM does not
// cycle a value on `A`: it opens a mode where the panel gives way to the picture
// and the buttons drive the framing until A or B.
extern "C" void ruffle_zoom_begin(void);
extern "C" void ruffle_zoom_adjust(int d_percent, int dx, int dy);
extern "C" int  ruffle_zoom_percent(void);
extern "C" void ruffle_zoom_reset(void);
extern "C" void ruffle_zoom_commit(void);
extern "C" void ruffle_zoom_cancel(void);
extern "C" void ruffle_draw_zoom_overlay(void);
extern "C" int  ruffle_keymap_lookup(const char* button_name);
// Players 2 to 4 (issues #40, #128): the same, through that player's keymap.
extern "C" int  ruffle_keymap_lookup_player(int player, const char* button_name);
// Per-modifier combo layers (#57): mod_code 1=ZL 2=ZR 3=L 4=R.
extern "C" int  ruffle_keymap_lookup_combo(int mod_code, const char* button_name);
extern "C" int  ruffle_keymap_lookup_combo_player(int player, int mod_code, const char* button_name);
extern "C" int  ruffle_keymap_combo_active(int mod_code);    // 1 = that modifier has a layer
extern "C" int  ruffle_keymap_combo_active_player(int player, int mod_code);
// The cursor a lone Joy-Con's stick drives for player 1 (issue #128): drawn
// in gold, even with the pointer hidden, so the stick's mode shows.
extern "C" void ruffle_set_stick_cursor(int on);
extern "C" int  ruffle_keymap_cursor_speed(void);          // per-game speed, -1 = unset
extern "C" int  ruffle_gyro_mode(void);                    // per-game gyroscope cursor mode (#119)
extern "C" void ruffle_keymap_set_cursor_speed(int idx);   // persist per-game speed
extern "C" void ruffle_touches_open(void);
extern "C" void ruffle_touches_close(void);
extern "C" int  ruffle_touches_active(void);
extern "C" int  ruffle_touches_input(const char* button_name);
extern "C" int  ruffle_touches_consume_dirty(void);
extern "C" void ruffle_touches_draw(void);
// The pause's deferred profile request, before the frame: its wait draws
// frames itself, so the loading card's spinner turns (lib.rs).
extern "C" void ruffle_touches_drive(void);
extern "C" void ruffle_menu_touch(float x, float y, int pressed);
extern "C" int  ruffle_pause_touch(float x, float y, int pressed, int live);
extern "C" void ruffle_set_direct_launch(int on);
// Touch tables of the pause panels (lib.rs UI_KIND_PAUSE / UI_KIND_SCREEN).
static const int UI_KIND_PAUSE  = 60;
static const int UI_KIND_SCREEN = 61;
// What `ruffle_pause_touch` answers besides a row index (lib.rs PAUSE_TAP_*):
// nothing, the way back (the rail or the page's caption), or a part of a
// row's control that Rust already acted on (an option set, the switch
// flipped, an armed arrow stepped), with the row in the low byte.
static const int PAUSE_TAP_NONE    = -1;
static const int PAUSE_TAP_BACK    = -2;
static const int PAUSE_TAP_SET     = 0x1000;  // a value changed: the click
static const int PAUSE_TAP_REFUSED = 0x2000;  // the console said no: the error
static const int PAUSE_TAP_SAME    = 0x3000;  // the option already in force

// In-game software keyboard (raised when a Flash TextField gains focus).
extern "C" int  ruffle_keyboard_take_request(void);
extern "C" int  ruffle_keyboard_field(char* out, int cap, int* out_flags, int* out_max);
extern "C" int  ruffle_keyboard_submit(const char* text);
// Raise the keyboard from a button bound to SK_KEYBOARD, and type its result
// into the movie when no editable field is focused to receive it.
extern "C" void ruffle_keyboard_request_manual(void);
extern "C" int  ruffle_keyboard_type_text(const char* text);
// The keyboard's title in the menu language (Rust string tables).
extern "C" int  ruffle_keyboard_header(char* out, int cap);
extern "C" int  swkbd_prompt_game_field(const char* header, const char* initial, int flags, int maxlen, char* out, int cap);

// Switch key codes (must match SK_* constants in rust/src/lib.rs).
enum SwitchKey {
    SK_NONE     = 0,
    SK_SPACE    = 1,
    SK_ENTER    = 2,
    SK_ESCAPE   = 3,
    SK_LEFT     = 4,
    SK_RIGHT    = 5,
    SK_UP       = 6,
    SK_DOWN     = 7,
    SK_Z        = 8,
    SK_X        = 9,
    SK_SHIFT    = 10,
    SK_P        = 11,
    // Pseudo-codes (NOT keys): a button resolving to one of these fires a mouse
    // click at the cursor instead of a key. Must match rust/src/lib.rs.
    SK_MOUSE_LEFT  = 49,
    SK_MOUSE_RIGHT = 50,
    // Opens the console keyboard, for the games whose text box Ruffle does not
    // track as focused and which therefore never raise it on their own.
    SK_KEYBOARD    = 91,
};

// Joycon → key mapping is now driven by the user-editable keymap on SD:
// `sdmc:/ruffle/<basename>.keymap.json` (per-game) → `sdmc:/ruffle/keymap_default.json`
// (global) → hardcoded fallback in rust/src/keymap.rs. We declare every
// joycon button we COULD bind here (name + mask); at boot we ask Rust which
// Flash SK_* each name resolves to, and populate the runtime BINDINGS array.
//
// Unbound buttons stay in the table with key=SK_NONE and are skipped in the
// input loop. `Minus` is intentionally absent — it's reserved by the
// runtime for the pause menu and cannot be remapped.
struct ButtonBinding {
    const char* name;  // matches keymap JSON key strings
    u64         mask;
    int         key;   // populated at boot via ruffle_keymap_lookup
};
static ButtonBinding BINDINGS[] = {
    { "A",            HidNpadButton_A,            SK_NONE },
    { "B",            HidNpadButton_B,            SK_NONE },
    { "X",            HidNpadButton_X,            SK_NONE },
    { "Y",            HidNpadButton_Y,            SK_NONE },
    { "L",            HidNpadButton_L,            SK_NONE },
    { "R",            HidNpadButton_R,            SK_NONE },
    { "ZL",           HidNpadButton_ZL,           SK_NONE },
    // ZR is keymap-driven now (defaults to "Left click"). The legacy hardcoded
    // ZR left-click below only kicks in when ZR is UNBOUND (old sidecars), so
    // those games don't lose their click. Touch is always left-click.
    { "ZR",           HidNpadButton_ZR,           SK_NONE },
    { "Plus",         HidNpadButton_Plus,         SK_NONE },
    { "Left",         HidNpadButton_Left,         SK_NONE },
    { "Right",        HidNpadButton_Right,        SK_NONE },
    { "Up",           HidNpadButton_Up,           SK_NONE },
    { "Down",         HidNpadButton_Down,         SK_NONE },
    { "StickLLeft",   HidNpadButton_StickLLeft,   SK_NONE },
    { "StickLRight",  HidNpadButton_StickLRight,  SK_NONE },
    { "StickLUp",     HidNpadButton_StickLUp,     SK_NONE },
    { "StickLDown",   HidNpadButton_StickLDown,   SK_NONE },
    // Joy-Con side buttons (handheld / detached). Left+right SL/SR share a
    // generic name so a single binding covers whichever Joy-Con is in use.
    { "SL",           HidNpadButton_LeftSL | HidNpadButton_RightSL, SK_NONE },
    { "SR",           HidNpadButton_LeftSR | HidNpadButton_RightSR, SK_NONE },
    // Right stick as a d-pad: the digital StickR* masks edge-detect for us, so
    // these forward as key events like any button. Binding ANY of them flips the
    // right stick out of cursor mode in-game (see g_right_stick_dpad below).
    { "StickRUp",     HidNpadButton_StickRUp,     SK_NONE },
    { "StickRDown",   HidNpadButton_StickRDown,   SK_NONE },
    { "StickRLeft",   HidNpadButton_StickRLeft,   SK_NONE },
    { "StickRRight",  HidNpadButton_StickRRight,  SK_NONE },
    // Stick CLICKS (press the analog sticks, L3/R3). Not directions — they don't
    // affect cursor/d-pad mode.
    { "StickLPress",  HidNpadButton_StickL,        SK_NONE },
    { "StickRPress",  HidNpadButton_StickR,        SK_NONE },
    // `-` is player 1's pause and never reaches this table for them (their
    // keymap cannot bind it); for players 2 to 4 it is a button like any other
    // (issue #128), the small button of a left Joy-Con they hold alone.
    { "Minus",        HidNpadButton_Minus,         SK_NONE },
};
static constexpr size_t BINDINGS_COUNT = sizeof(BINDINGS) / sizeof(BINDINGS[0]);

// Players 2 to 4 (issues #40, #128): resolved Flash key per BINDINGS entry for
// controllers 2 to 4, each through its own keymap. Same buttons/masks as
// BINDINGS, different keys. Index 0 is player 2.
static constexpr int EXTRA_PLAYERS = 3;
static int BINDINGS_PN_KEYS[EXTRA_PLAYERS][BINDINGS_COUNT];

// Per-modifier combo layers (issue #57): FOUR independent modifiers (ZL/ZR/L/R),
// each with its own button→key layer. Holding an active modifier makes every other
// button send that modifier's key (SK_NONE = fall through to the base key, so a
// held modifier never breaks unremapped buttons). All four work at once, so L+A
// and R+A can differ. Index order matches COMBO_MODS below.
enum { CM_ZL = 0, CM_ZR, CM_L, CM_R, CM_COUNT };
static const u64 COMBO_MOD_MASKS[CM_COUNT] = {
    HidNpadButton_ZL, HidNpadButton_ZR, HidNpadButton_L, HidNpadButton_R,
};
static int  BINDINGS_COMBO_KEYS[CM_COUNT][BINDINGS_COUNT];    // P1 layers
static int  BINDINGS_COMBO_PN_KEYS[EXTRA_PLAYERS][CM_COUNT][BINDINGS_COUNT]; // P2-P4 layers
static bool g_combo_active[CM_COUNT];    // P1: this modifier has a layer -> it's a modifier
static bool g_combo_active_pn[EXTRA_PLAYERS][CM_COUNT];
// The Flash key each button is CURRENTLY holding down (SK_NONE = up), per player.
// Captured at press time so the key-up releases the SAME key even if the modifier
// was let go in between (press ZL+A → F1 down; release ZL; release A → F1 up).
static int g_p1_down_key[BINDINGS_COUNT];
static int g_pn_down_key[EXTRA_PLAYERS][BINDINGS_COUNT];

// True when the user bound any StickR*/StickL* DIRECTION (not the press): that
// stick then acts as a d-pad (its key bindings fire) instead of moving the
// mouse cursor. Both default differently — the left stick is bound to the arrow
// keys by default (so it stays a d-pad), the right stick to nothing (so it's
// the cursor) — but the rule is symmetric: clear a stick's directions and it
// becomes a cursor. Recomputed whenever the keymap is (re)loaded.
static bool g_right_stick_dpad = false;
static bool g_left_stick_dpad = false;

// Buttons forwarded to the Rust TOUCHES sub-screen / library state.
// Names match what `keymap::EDITABLE_BUTTONS` exposes. `repeat = true`
// makes the button auto-repeat when held in menu loops (D-pad + L-stick
// directions only — face buttons stay one-shot so held A doesn't
// re-trigger downloads, options, etc.).
struct MenuNavButton {
    const char* name;
    u64         mask;
    bool        repeat;
};
static const MenuNavButton MENU_NAV_BUTTONS[] = {
    { "A",            HidNpadButton_A,            false },
    { "B",            HidNpadButton_B,            false },
    { "X",            HidNpadButton_X,            false },
    { "Y",            HidNpadButton_Y,            false },
    // L/R: hold-to-repeat ON because they PAGE the long lists (an archive's
    // games, which run to thousands, and the bug report's game list), where a
    // held shoulder is how you get far fast. Everywhere else they act once per
    // press and ignore the repeat ticks (`nav_may_wrap` on the Rust side): the
    // tabs, the next / previous game in a game's options, the layers of the
    // controls editor.
    { "L",            HidNpadButton_L,            true  },
    { "R",            HidNpadButton_R,            true  },
    { "ZL",           HidNpadButton_ZL,           false },
    // ZR is consumed by the in-game mouse-click path, but during the
    // library / menu loops we forward its down-edge to Rust so DISTANT
    // mode can use it as "fetch this URL without opening the keyboard".
    // No collision because in-game and library loops are exclusive.
    { "ZR",           HidNpadButton_ZR,           false },
    { "Plus",         HidNpadButton_Plus,         false },
    { "Minus",        HidNpadButton_Minus,        false },
    { "Up",           HidNpadButton_Up,           true  },
    { "Down",         HidNpadButton_Down,         true  },
    { "Left",         HidNpadButton_Left,         true  },
    { "Right",        HidNpadButton_Right,        true  },
    { "StickLUp",     HidNpadButton_StickLUp,     true  },
    { "StickLDown",   HidNpadButton_StickLDown,   true  },
    { "StickLLeft",   HidNpadButton_StickLLeft,   true  },
    { "StickLRight",  HidNpadButton_StickLRight,  true  },
    // Right stick also navigates the library / menus. It's the mouse cursor
    // ONLY in-game, and the library/menu loops are exclusive with the game
    // loop, so it's free here. Aliased to the left-stick nav names so every
    // screen's existing Up/Down/Left/Right handling picks it up with no Rust
    // change. (Separate repeat-state slots → no conflict with the left stick.)
    { "StickLUp",     HidNpadButton_StickRUp,     true  },
    { "StickLDown",   HidNpadButton_StickRDown,   true  },
    { "StickLLeft",   HidNpadButton_StickRLeft,   true  },
    { "StickLRight",  HidNpadButton_StickRRight,  true  },
};
static constexpr size_t MENU_NAV_COUNT =
    sizeof(MENU_NAV_BUTTONS) / sizeof(MENU_NAV_BUTTONS[0]);

// Per-button hold-to-repeat state for the menu / library loops.
// `held_since[i]` is the tick count when the button was first pressed (0
// = not held). `last_emit[i]` is the tick of the last forwarded event;
// once `held_since[i] != 0`, repeat events fire after INITIAL_DELAY,
// then every REPEAT_INTERVAL until release.
struct MenuRepeatState {
    u64  held_since[MENU_NAV_COUNT];
    u64  last_emit[MENU_NAV_COUNT];
    bool first_repeat_done[MENU_NAV_COUNT];
};

// Reset all per-button state. Call when entering a menu loop so previous
// long-held buttons don't bleed in.
static void menu_repeat_reset(MenuRepeatState& rs) {
    for (size_t i = 0; i < MENU_NAV_COUNT; ++i) {
        rs.held_since[i] = 0;
        rs.last_emit[i] = 0;
        rs.first_repeat_done[i] = false;
    }
}

// Forward one frame of pad input to a Rust input callback, with auto-
// repeat for buttons marked `repeat = true`. `forward` is called once
// per emitted event (down-edge or repeat tick). 400 ms initial delay,
// 80 ms repeat interval — matches standard hbmenu / Switch system feel.
static void menu_repeat_step(
    MenuRepeatState& rs,
    u64 kDown, u64 kUp, u64 kHeld,
    u64 now_tick, u64 tick_freq,
    int (*forward)(const char* name)
) {
    const u64 INITIAL_DELAY = (400ULL * tick_freq) / 1000ULL;
    const u64 REPEAT_INTERVAL = ( 80ULL * tick_freq) / 1000ULL;
    for (size_t i = 0; i < MENU_NAV_COUNT; ++i) {
        const auto& nb = MENU_NAV_BUTTONS[i];
        // Tell Rust whether each event is a fresh press or a repeat tick. Lists
        // wrap round on a press but STOP at the end under a held direction, so
        // holding Down reaches the end of the library instead of looping through
        // it for ever (see `nav_may_wrap` in [rust/src/library.rs]).
        if (kDown & nb.mask) {
            rs.held_since[i] = now_tick;
            rs.last_emit[i] = now_tick;
            rs.first_repeat_done[i] = false;
            ruffle_nav_repeat(0);
            forward(nb.name);
        } else if (kUp & nb.mask) {
            rs.held_since[i] = 0;
            rs.first_repeat_done[i] = false;
        } else if (nb.repeat && (kHeld & nb.mask) && rs.held_since[i] != 0) {
            const u64 since_press = now_tick - rs.held_since[i];
            if (!rs.first_repeat_done[i]) {
                if (since_press >= INITIAL_DELAY) {
                    rs.first_repeat_done[i] = true;
                    rs.last_emit[i] = now_tick;
                    ruffle_nav_repeat(1);
                    forward(nb.name);
                }
            } else if (now_tick - rs.last_emit[i] >= REPEAT_INTERVAL) {
                rs.last_emit[i] = now_tick;
                ruffle_nav_repeat(1);
                forward(nb.name);
            }
        }
    }
}

// Hold-to-repeat for the pause panels, which read raw down-edges: a held
// direction fires again after 400 ms, then every 80 ms, the timing the
// launcher's lists use (`menu_repeat_step`). Returns the direction bits to
// treat as pressed this frame; `*repeat_out` says they are all repeat ticks,
// so a list stops at its end under a held direction and only a fresh press
// wraps round, as in the launcher (no carousel).
struct PauseRepeat {
    u64  since[4];
    u64  last[4];
    bool first_done[4];
};
static const u64 PAUSE_DIRS[4] = {
    HidNpadButton_Up | HidNpadButton_StickLUp | HidNpadButton_StickRUp,
    HidNpadButton_Down | HidNpadButton_StickLDown | HidNpadButton_StickRDown,
    HidNpadButton_Left | HidNpadButton_StickLLeft | HidNpadButton_StickRLeft,
    HidNpadButton_Right | HidNpadButton_StickLRight | HidNpadButton_StickRRight,
};
static void pause_repeat_reset(PauseRepeat& r) {
    for (int i = 0; i < 4; ++i) {
        r.since[i] = 0;
        r.last[i] = 0;
        r.first_done[i] = false;
    }
}
static u64 pause_repeat_step(PauseRepeat& r, u64 kDown, u64 kHeld, u64 now, u64 freq, bool* repeat_out) {
    const u64 INITIAL = (400ULL * freq) / 1000ULL;
    const u64 INTERVAL = (80ULL * freq) / 1000ULL;
    u64 out = 0;
    bool fresh = false;
    for (int i = 0; i < 4; ++i) {
        const u64 m = PAUSE_DIRS[i];
        if (kDown & m) {
            r.since[i] = now;
            r.last[i] = now;
            r.first_done[i] = false;
            out |= kDown & m;
            fresh = true;
            continue;
        }
        if (!(kHeld & m)) {
            r.since[i] = 0;
            continue;
        }
        if (r.since[i] == 0) continue;
        if (!r.first_done[i]) {
            if (now - r.since[i] >= INITIAL) {
                r.first_done[i] = true;
                r.last[i] = now;
                out |= kHeld & m;
            }
        } else if (now - r.last[i] >= INTERVAL) {
            r.last[i] = now;
            out |= kHeld & m;
        }
    }
    *repeat_out = out != 0 && !fresh;
    return out;
}

// Menu navigation acts on ONE axis at a time (input rule 7). A stick pushed a
// little off straight sets a vertical AND a horizontal bit, and both used to
// go through: going down a settings list, a thumb slightly off-centre stepped
// the values it passed (Jonathan: "on peut descendre en diagonal
// involontairement"). Filtered here, for the menu loops only, never for the
// game's own buttons, the zoom framing or the cursor, which want both axes.
//
// When a stick leaves its centre, the axis pushed further is latched (a tie
// goes vertical) and the other axis is dropped for as long as the latched one
// is held. Letting go of the latched axis while the other is still held (the
// thumb rolling round the rim) switches the latch, and the new direction
// counts as a fresh press. The d-pad gets the same latch when both its axes
// are down: the first one pressed wins. This is the usual dominant-axis
// filtering of console menus.
struct NavLatch {
    int axis[3]; // left stick, right stick, d-pad: 0 free, 1 vertical, 2 horizontal
};
static void nav_latch_reset(NavLatch& n) {
    n.axis[0] = n.axis[1] = n.axis[2] = 0;
}
static void nav_latch_group(int& axis, u64 vmask, u64 hmask, u64* held, u64* down, float dx, float dy) {
    const u64 v = *held & vmask;
    const u64 h = *held & hmask;
    if (!v && !h) {
        axis = 0;
        return;
    }
    if (axis == 0) {
        if (v && h) axis = (std::fabs(dy) >= std::fabs(dx)) ? 1 : 2;
        else        axis = v ? 1 : 2;
    } else if (axis == 1 && !v) {
        axis = 2;
        *down |= h;
    } else if (axis == 2 && !h) {
        axis = 1;
        *down |= v;
    }
    const u64 drop = (axis == 1) ? hmask : vmask;
    *held &= ~drop;
    *down &= ~drop;
}
// Filter one frame of `held` / `down` in place. Only the direction bits are
// touched; every other button goes through as it came.
static void nav_filter(NavLatch& n, PadState* pad, u64* held, u64* down) {
    const HidAnalogStickState l = padGetStickPos(pad, 0);
    const HidAnalogStickState r = padGetStickPos(pad, 1);
    nav_latch_group(n.axis[0], HidNpadButton_StickLUp | HidNpadButton_StickLDown,
                    HidNpadButton_StickLLeft | HidNpadButton_StickLRight, held, down,
                    (float)l.x, (float)l.y);
    nav_latch_group(n.axis[1], HidNpadButton_StickRUp | HidNpadButton_StickRDown,
                    HidNpadButton_StickRLeft | HidNpadButton_StickRRight, held, down,
                    (float)r.x, (float)r.y);
    // The d-pad has no deflection to compare: both axes pressed on the same
    // frame is a tie, which goes vertical.
    nav_latch_group(n.axis[2], HidNpadButton_Up | HidNpadButton_Down,
                    HidNpadButton_Left | HidNpadButton_Right, held, down, 0.0f, 1.0f);
}

static void populate_bindings_from_keymap(void) {
    for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
        BINDINGS[i].key = ruffle_keymap_lookup(BINDINGS[i].name);
        for (int p = 0; p < EXTRA_PLAYERS; ++p) {
            BINDINGS_PN_KEYS[p][i] = ruffle_keymap_lookup_player(p + 2, BINDINGS[i].name); // #40, #128
        }
        // Per-modifier combo layers (#57): mod_code = m+1 (1=ZL..4=R).
        for (int m = 0; m < CM_COUNT; ++m) {
            BINDINGS_COMBO_KEYS[m][i] = ruffle_keymap_lookup_combo(m + 1, BINDINGS[i].name);
            for (int p = 0; p < EXTRA_PLAYERS; ++p) {
                BINDINGS_COMBO_PN_KEYS[p][m][i] = ruffle_keymap_lookup_combo_player(p + 2, m + 1, BINDINGS[i].name);
            }
        }
    }
    // Which modifiers have a layer (-> they act as modifiers, their own key muted).
    for (int m = 0; m < CM_COUNT; ++m) {
        g_combo_active[m] = ruffle_keymap_combo_active(m + 1) != 0;
        for (int p = 0; p < EXTRA_PLAYERS; ++p) {
            g_combo_active_pn[p][m] = ruffle_keymap_combo_active_player(p + 2, m + 1) != 0;
        }
    }
    // Drop any remembered held-key state so a re-remap / restart can't leave a
    // button "stuck down" against the new bindings.
    for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
        g_p1_down_key[i] = SK_NONE;
        for (int p = 0; p < EXTRA_PLAYERS; ++p) g_pn_down_key[p][i] = SK_NONE;
    }
    // A stick = d-pad as soon as any of its DIRECTION sub-buttons is bound;
    // otherwise it stays the mouse cursor in-game. "Stick?Press" (n[6]=='P') is
    // the analog CLICK and is excluded — it doesn't change cursor/d-pad mode.
    // n[5] is 'R'/'L' (StickR.../StickL...). All "St…" binding names are sticks.
    g_right_stick_dpad = false;
    g_left_stick_dpad = false;
    for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
        const char* n = BINDINGS[i].name;
        const bool is_stick_dir =
            n[0] == 'S' && n[1] == 't' && n[6] != 'P'; // "Stick{L,R}{U,D,L,R}..."
        if (!is_stick_dir || BINDINGS[i].key == SK_NONE) continue;
        if (n[5] == 'R') g_right_stick_dpad = true;
        else if (n[5] == 'L') g_left_stick_dpad = true;
    }
    // Quick visibility into what the loaded keymap resolved to. Helps the
    // user confirm their edits to keymap.json took effect.
    std::printf("keymap: resolved %zu bindings:", BINDINGS_COUNT);
    for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
        if (BINDINGS[i].key != SK_NONE) {
            std::printf(" %s=%d", BINDINGS[i].name, BINDINGS[i].key);
        }
    }
    std::printf("\n");
    std::fflush(stdout);
}

// Pause-menu items. The labels live on the Rust side (render::MENU_ITEMS);
// here we just need the count and per-index action enum. ORDER MUST MATCH
// the `MENU_ITEMS` slice in [rust/src/backend/render.rs].
enum MenuAction {
    MENU_RESUME       = 0,
    MENU_TOUCHES      = 1,  // opens the TOUCHES sub-menu (Rust-driven): edit /
                           // apply / share / revert / cursor speed (#20 Opt 1)
    MENU_SCREEN       = 2,  // opens the ECRAN sub-panel: display mode (#65/#69/
                            // #74), rotation (#78) and screen filter (#65). All
                            // three preview on the frozen frame behind the panel
                            // and all three answer the same question, so they sit
                            // together instead of taking three of seven rows.
    MENU_POWER        = 3,  // OVERCLOCK Off / Auto / On, a value row: A arms
                            // it, Left / Right change it, A keeps it.
                            // Top level and NOT inside ECRAN: every ECRAN row
                            // previews on the frozen frame behind the panel and
                            // this one cannot, so buried a level down it would
                            // be undiscoverable. It is also the largest lever
                            // the app has, roughly 1.5x on the games that are
                            // interpreter-bound.
    MENU_RESTART      = 4,
    MENU_QUIT         = 5,  // VITESSE moved INTO the TOUCHES sub-menu
    MENU_COUNT        = 6,
};

// Rows of the ECRAN sub-panel. ORDER MUST MATCH `SCREEN_ITEMS` in
// [rust/src/backend/render.rs].
enum ScreenAction {
    SCREEN_DISPLAY  = 0,
    SCREEN_ROTATION = 1,
    SCREEN_ZOOM     = 2,  // geometry, so next to the rotation; the filter is colour.
                          // A row that opens a mode (the framing), not a value
                          // row: its percentage is changed in there.
    SCREEN_PIXELS   = 3,  // resampling: the last step of the geometry, under ZOOM
    SCREEN_FILTER   = 4,
    SCREEN_FPS      = 5,  // adds something to the screen rather than changing
                          // what is on it, so it goes after the ones that do
    SCREEN_COUNT    = 6,
};

// The physical panel / touchscreen coordinate space. Touch samples always arrive
// in these units regardless of what we render at, so they need scaling into
// render space whenever the two differ.
static constexpr int PANEL_W = 1280;
static constexpr int PANEL_H = 720;

// TWO internal render resolutions — keep in sync with rust/src/lib.rs.
//
// The Switch display scaler upscales the window surface to the panel for free, so
// rendering smaller is a straight fill-rate saving rather than a smaller picture.
// Cost is softness, which is why the two differ:
//
//  - The UI (library, modals, keymap editor) renders at PANEL size. It is cheap
//    (a few hundred draws) and it is all text and thin lines, where upscaling is
//    immediately visible and unpleasant.
//  - GAMES render lower. They are the actual load, and the ones that hurt are
//    fill-bound: Dragon City spends ~76 ms of a 112 ms frame in render, with 83
//    full-stage complex blend groups per frame. Shrinking the surface shrinks the
//    main pass AND every full-stage offscreen temp (blend groups, alpha masks,
//    cacheAsBitmap), quadratically. Measured 5.7 -> 8.6 fps at 960x540.
//
//    960x540 = 0.56x the pixels (1.8x less fill); 640x360 would be 0.25x.
//    Games that are SCRIPT-bound (Mario 63, Agent P: 115 of 123 ms in the AVM)
//    gain nothing from this — their cost is Ruffle's interpreter, not our fill.
static constexpr int UI_VIEWPORT_W   = PANEL_W;
static constexpr int UI_VIEWPORT_H   = PANEL_H;
// Currently PANEL size: measured 2026-07-29, lowering this only helps FILL-BOUND
// titles (Dragon City 5.7 -> 8.6 fps) while every script-bound game gains nothing
// (Mario 63, Agent P spend ~90% of the frame in Ruffle's interpreter). Since the
// game's render space is also what the pause menu and TOUCHES editor draw in,
// a global reduction softened all that UI to speed up exactly one game — a bad
// trade, so it is off. The plumbing below is kept and stays correct at any value:
// set these lower (960x540 = 1.8x less fill, 640x360 = 4x) to re-enable, ideally
// per-game via the keymap sidecar rather than globally.
static constexpr int GAME_VIEWPORT_W = PANEL_W;
static constexpr int GAME_VIEWPORT_H = PANEL_H;

// Panel -> render space scale for touch, per context.
static constexpr float UI_TOUCH_SCALE_X   = (float)UI_VIEWPORT_W / (float)PANEL_W;
static constexpr float UI_TOUCH_SCALE_Y   = (float)UI_VIEWPORT_H / (float)PANEL_H;
static constexpr float GAME_TOUCH_SCALE_X = (float)GAME_VIEWPORT_W / (float)PANEL_W;
static constexpr float GAME_TOUCH_SCALE_Y = (float)GAME_VIEWPORT_H / (float)PANEL_H;

// Recreate the window surface at a different size, preserving all GL objects.
extern "C" bool gl_context_resize(unsigned int w, unsigned int h);

// Right-stick axis range (libnx reports -0x7FFF..0x7FFF).
static constexpr float STICK_DEADZONE = 4000.0f;
static constexpr float STICK_MAX      = 32767.0f;
// Cursor speed in pixels per frame at full stick deflection. `CURSOR_SPEED_BASE`
// is the x1.0 tuning; the effective `g_cursor_speed` = base * the chosen preset
// multiplier (adjustable in REGLAGES, persisted to sdmc:/flashnx/cursor_speed).
// Requested by DSwizzy (issue #17) for fast-mouse games like Spank the Monkey.
// Per-game now: the chosen preset is saved in the game's keymap (see
// ruffle_keymap_cursor_speed); the top presets go fast for twitchy cursors.
static constexpr float CURSOR_SPEED_BASE = 12.0f;
static const float CURSOR_SPEED_MULTS[] = { 0.5f, 1.0f, 1.5f, 2.0f, 2.5f, 3.0f, 4.0f, 5.0f };
static constexpr int CURSOR_SPEED_COUNT = 8;
static int   g_cursor_speed_idx = 1;                  // x1.0 by default
static float g_cursor_speed     = CURSOR_SPEED_BASE;  // = base * mult[idx]
// True while a game is actually running (vs the library UI). Decides whether the
// VITESSE cycle saves per-game (<basename>.cursor) or the global default.
static bool  g_in_game = false;

extern "C" int flashnx_commit_sd(void); // ruffle_bridge.cpp (fsdevCommitDevice)

static void cursor_speed_apply() {
    if (g_cursor_speed_idx < 0) g_cursor_speed_idx = 0;
    if (g_cursor_speed_idx >= CURSOR_SPEED_COUNT) g_cursor_speed_idx = CURSOR_SPEED_COUNT - 1;
    g_cursor_speed = CURSOR_SPEED_BASE * CURSOR_SPEED_MULTS[g_cursor_speed_idx];
}

// Path of one of the app's own files, beside the library wherever it lives.
//
// `games_dir` is the one file that never moves — it is the pointer that says
// where everything else went (#79). Everything else follows the games, so that
// a folder the user was told is empty really is.
static const char* app_file_path(const char* name) {
    static char out[600];
    char root[512];
    root[0] = '\0';
    FILE* gd = std::fopen("sdmc:/switch/FlashNX/games_dir", "rb");
    if (!gd) gd = std::fopen("sdmc:/flashnx/games_dir", "rb");
    if (gd) {
        const size_t n = std::fread(root, 1, sizeof(root) - 1, gd);
        std::fclose(gd);
        root[n] = '\0';
        size_t len = std::strlen(root);
        while (len > 0 && (root[len - 1] == '\n' || root[len - 1] == '\r' ||
                           root[len - 1] == ' ' || root[len - 1] == '/')) {
            root[--len] = '\0';
        }
    }
    if (!root[0]) {
        std::snprintf(root, sizeof(root), "%s", "sdmc:/flashnx");
    }
    std::snprintf(out, sizeof(out), "%s/%s", root, name);
    return out;
}

// ─── Gyroscope cursor (#119) ────────────────────────────────────────────────
// Moving the controller moves the cursor, on top of the sticks, in the mode the
// game's TOUCHES panel picks (`ruffle_gyro_mode`). A stick or a touch moves the
// cursor as usual and the gyroscope carries on from there, so no button is
// taken for recentring. Sensor: the handheld console, a Pro Controller, or the
// RIGHT Joy-Con of a pair; every new sample is used exactly once, with its own
// interval, so nothing depends on the frame rate. 30 degrees cross the game's
// whole width. The per-game cursor speed is the sticks' alone: scaled by it, a
// game set to x5 for its stick crossed the screen in 9 degrees (2026-10-08).
//
// POINTER, like a Wii remote: the cursor goes where the controller points,
// from the rotation. It keeps an aim point that may go half a screen past the
// edges, so pointing away and back lands on the same spot.
//
// MARBLE: the screen is a board, the cursor sits where a marble would have
// rolled to: toward the edge that goes down (raise the bottom edge and it goes
// up), as far as the board is tilted from how it was held when the mode came
// on or the cursor was last moved by hand. It reads the TILT, not the turn:
// turning on the spot does nothing, the same tilt is always the same place,
// and nothing drifts. "Position control" in Constantin and MacKenzie's tilt
// study (2014), the faster of the two mappings in their marble maze.
//
// Axes (nn::hid, as chiaki-ng converts them for a PlayStation pad): x points
// right (pitch), y forward along the controller (roll), z out of its face (the
// turn when it is held flat); angular velocity in revolutions per second,
// acceleration in G, gravity reading as pointing down. A console held upright
// turns about y instead, so the turn is measured about the real vertical, from
// gravity: GyroWiki's "player space", as in Splatoon. Pitch stays local.
enum { GYRO_OFF = 0, GYRO_MARBLE = 1, GYRO_POINTER = 2 };  // keymap::GYRO_*
static HidSixAxisSensorHandle g_gyro_handles[2];
static int g_gyro_handle_count = 0;
static u32 g_gyro_style = 0;
static u64 g_gyro_last_sample = 0;
static u64 g_gyro_last_tick = 0;
// Gravity in controller axes (G), smoothed over about 0.1 s.
static float g_grav[3] = {0.0f, 0.0f, 0.0f};
static bool g_grav_valid = false;
// POINTER's aim point, in game pixels; up to half a screen past each edge.
static float g_aim_x = 0.0f, g_aim_y = 0.0f;
static bool g_aim_valid = false;
// Once a second while it is on: samples, their intervals, gravity, the signed
// rotation about each axis and the cursor motion, so a test session (turn
// right, tilt the top up) shows whether the axes and signs are the ones above.
static u32 g_gyro_trace_n = 0;
static u64 g_gyro_trace_dt_min = ~0ull;
static u64 g_gyro_trace_dt_max = 0;
static float g_gyro_trace_rot[3] = {0.0f, 0.0f, 0.0f};
static float g_gyro_trace_px[2] = {0.0f, 0.0f};
static u64 g_gyro_trace_tick = 0;
// Revolutions per second below which a sample fades out (~2.5 degrees/s):
// sensor noise and hand tremor at rest would otherwise drift the cursor.
static constexpr float GYRO_DEADZONE_RPS = 0.007f;
// GyroWiki's yaw relax factor: the turn may take up to ~45 degrees' worth of
// the other axis, for a controller held anywhere between flat and upright.
static constexpr float GYRO_YAW_RELAX = 1.41f;
// MARBLE: which way is down, in controller axes (unit vector). Turned by the
// gyroscope between samples and pulled toward the accelerometer (a
// complementary filter): the gyroscope keeps it steady through hand jolts, the
// accelerometer keeps it from drifting.
static float g_down[3] = {0.0f, 0.0f, -1.0f};
static bool g_down_valid = false;
// Samples since `g_down` started. The first states after the sensor starts
// can be a default (lying flat): seen on hardware 2026-10-08, the first level
// was 35 degrees off. So the tilt is not used before 0.2 s, and is pulled
// hard toward the accelerometer until then.
static u32 g_down_n = 0;
static constexpr u32 TILT_SETTLE_SAMPLES = 40;
// The tilt when the cursor was last placed by hand (radians), and where.
static float g_tilt0[2] = {0.0f, 0.0f};
static float g_tilt_anchor_x = 0.0f, g_tilt_anchor_y = 0.0f;
static bool g_tilt_anchor_valid = false;
// Pull toward the accelerometer per 5 ms sample: about a quarter of a second.
static constexpr float TILT_ACCEL_PULL = 0.02f;

// The board's tilt (radians): [0] the right edge below the left, [1] the board
// pitched about its x axis, the bottom edge rising positive. Read from `g_down`
// as two angles, so it answers the same whether the console is held upright
// or flat. False before the first sample.
static bool gyro_tilt(float out[2]) {
    if (!g_down_valid || g_down_n < TILT_SETTLE_SAMPLES) return false;
    float nx = g_down[0];
    if (nx > 1.0f) nx = 1.0f;
    if (nx < -1.0f) nx = -1.0f;
    out[0] = std::asin(nx);
    out[1] = std::atan2(g_down[2], g_down[1]);
    return true;
}

static void gyro_stop() {
    for (int i = 0; i < g_gyro_handle_count; ++i) hidStopSixAxisSensor(g_gyro_handles[i]);
    g_gyro_handle_count = 0;
    g_gyro_style = 0;
    g_gyro_last_sample = 0;
    g_grav_valid = false;
    g_aim_valid = false;
    g_down_valid = false;
    g_tilt_anchor_valid = false;
}

static void gyro_acquire(u32 style) {
    gyro_stop();
    Result rc = MAKERESULT(Module_Libnx, LibnxError_NotFound);
    if (style & HidNpadStyleTag_NpadHandheld) {
        rc = hidGetSixAxisSensorHandles(&g_gyro_handles[0], 1, HidNpadIdType_Handheld, HidNpadStyleTag_NpadHandheld);
        if (R_SUCCEEDED(rc)) g_gyro_handle_count = 1;
    } else if (style & HidNpadStyleTag_NpadFullKey) {
        rc = hidGetSixAxisSensorHandles(&g_gyro_handles[0], 1, HidNpadIdType_No1, HidNpadStyleTag_NpadFullKey);
        if (R_SUCCEEDED(rc)) g_gyro_handle_count = 1;
    } else if (style & HidNpadStyleTag_NpadJoyDual) {
        // [0] = left, [1] = right: the right one aims, like a mouse hand.
        rc = hidGetSixAxisSensorHandles(&g_gyro_handles[0], 2, HidNpadIdType_No1, HidNpadStyleTag_NpadJoyDual);
        if (R_SUCCEEDED(rc)) g_gyro_handle_count = 2;
    }
    for (int i = 0; i < g_gyro_handle_count; ++i) hidStartSixAxisSensor(g_gyro_handles[i]);
    // Recorded even on failure, so an unsupported style is not retried (and
    // logged) every frame.
    g_gyro_style = style;
    std::printf("gyro: controller style 0x%x -> %d sensor(s), rc=0x%x\n",
                (unsigned)style, g_gyro_handle_count, (unsigned)rc);
    std::fflush(stdout);
}

// This frame's cursor motion from the gyroscope, in game pixels (0 when off).
// Returns the mode in force, `GYRO_OFF` when there is none.
static int gyro_cursor_delta(float* dx, float* dy) {
    *dx = 0.0f;
    *dy = 0.0f;
    const int mode = ruffle_gyro_mode();
    if (mode == GYRO_OFF) {
        if (g_gyro_handle_count) gyro_stop();
        return GYRO_OFF;
    }
    // The controller in the hand first: a detached one (player 1) before the
    // console, so a Joy-Con taken off to aim is the one read even while the
    // other stays on its rail as the handheld controller.
    u32 style = hidGetNpadStyleSet(HidNpadIdType_No1);
    if (!style) style = hidGetNpadStyleSet(HidNpadIdType_Handheld);
    if (style != g_gyro_style) gyro_acquire(style);
    if (!g_gyro_handle_count) return mode;
    // A Joy-Con pair: the right one, or the left one when it is alone.
    int which = 0;
    if (g_gyro_handle_count == 2)
        which = (hidGetNpadDeviceType(HidNpadIdType_No1) & HidDeviceTypeBits_JoyRight) ? 1 : 0;
    const HidSixAxisSensorHandle h = g_gyro_handles[which];
    HidSixAxisSensorState st[16];
    const size_t n = hidGetSixAxisSensorStates(h, st, 16);
    // After a gap (pause menu, HOME, a sensor just started) the samples are
    // only marked as seen: the time away must not land on the cursor at once.
    const u64 now = armGetSystemTick();
    const bool resync = g_gyro_last_sample == 0
                     || armTicksToNs(now - g_gyro_last_tick) > 100000000ull;
    g_gyro_last_tick = now;
    // Back from a gap the controller may be held another way: MARBLE starts
    // again from wherever the cursor is.
    if (resync) {
        g_down_valid = false;
        g_tilt_anchor_valid = false;
    }
    u64 newest = g_gyro_last_sample;
    float turn = 0.0f, tilt = 0.0f;
    for (size_t i = 0; i < n; ++i) {
        if (st[i].sampling_number <= g_gyro_last_sample) continue;
        if (st[i].sampling_number > newest) newest = st[i].sampling_number;
        const float a[3] = {st[i].acceleration.x, st[i].acceleration.y, st[i].acceleration.z};
        for (int k = 0; k < 3; ++k)
            g_grav[k] = g_grav_valid ? g_grav[k] + (a[k] - g_grav[k]) * 0.05f : a[k];
        g_grav_valid = true;
        const float alen = std::sqrt(a[0] * a[0] + a[1] * a[1] + a[2] * a[2]);
        if (!g_down_valid && alen > 0.5f) {
            for (int k = 0; k < 3; ++k) g_down[k] = a[k] / alen;
            g_down_valid = true;
            g_down_n = 0;
        }
        if (resync) continue;
        // `delta_time` in nanoseconds; outside 1-50 ms it is not trusted and
        // one sample at 200 Hz is assumed instead.
        const u64 dt_ns = st[i].delta_time;
        const float dt = (dt_ns >= 1000000ull && dt_ns <= 50000000ull) ? (float)dt_ns * 1e-9f : 0.005f;
        if (dt_ns < g_gyro_trace_dt_min) g_gyro_trace_dt_min = dt_ns;
        if (dt_ns > g_gyro_trace_dt_max) g_gyro_trace_dt_max = dt_ns;
        const float wx = st[i].angular_velocity.x;
        const float wy = st[i].angular_velocity.y;
        const float wz = st[i].angular_velocity.z;
        if (g_down_valid) {
            // A direction fixed in the world turns the other way in controller
            // axes: d -= (w x d) dt, w in radians.
            const float r = 6.2831853f * dt;
            const float ox = wx * r, oy = wy * r, oz = wz * r;
            const float d0 = g_down[0], d1 = g_down[1], d2 = g_down[2];
            g_down[0] = d0 - (oy * d2 - oz * d1);
            g_down[1] = d1 - (oz * d0 - ox * d2);
            g_down[2] = d2 - (ox * d1 - oy * d0);
            // Toward the accelerometer, unless the hand is jolting it.
            const float pull = g_down_n < TILT_SETTLE_SAMPLES ? 0.2f : TILT_ACCEL_PULL;
            if (alen > 0.7f && alen < 1.3f)
                for (int k = 0; k < 3; ++k) g_down[k] += (a[k] / alen - g_down[k]) * pull;
            g_down_n++;
            const float dl = std::sqrt(g_down[0] * g_down[0] + g_down[1] * g_down[1] + g_down[2] * g_down[2]);
            if (dl > 0.0f)
                for (int k = 0; k < 3; ++k) g_down[k] /= dl;
        }
        // The turn: rotation about the world's vertical (opposite of gravity),
        // from the roll and face axes only, capped as GyroWiki does.
        float yaw = wz;  // no usable gravity (free fall, no data): held flat
        const float glen = std::sqrt(g_grav[0] * g_grav[0] + g_grav[1] * g_grav[1] + g_grav[2] * g_grav[2]);
        if (glen > 0.5f) {
            const float world = -(wy * g_grav[1] + wz * g_grav[2]) / glen;
            const float both = std::sqrt(wy * wy + wz * wz);
            yaw = std::copysign(std::fmin(std::fabs(world) * GYRO_YAW_RELAX, both), world);
        }
        float k = std::sqrt(yaw * yaw + wx * wx) / GYRO_DEADZONE_RPS;
        if (k > 1.0f) k = 1.0f;
        k = k * k * (3.0f - 2.0f * k);
        turn += yaw * dt * k;
        tilt += wx * dt * k;
        g_gyro_trace_rot[0] += wx * dt;
        g_gyro_trace_rot[1] += wy * dt;
        g_gyro_trace_rot[2] += wz * dt;
        g_gyro_trace_n++;
    }
    g_gyro_last_sample = newest;
    // 30 degrees (1/12 revolution) across the width. Right-handed axes: a
    // positive turn about the vertical is to the LEFT, a positive pitch raises
    // the top edge; screen x grows rightwards and y downwards.
    // POINTER's motion; MARBLE places the cursor itself (`gyro_tilt`).
    const float scale = (float)GAME_VIEWPORT_W * 12.0f;
    if (mode == GYRO_POINTER) {
        *dx = -turn * scale;
        *dy = -tilt * scale;
    }
    g_gyro_trace_px[0] += *dx;
    g_gyro_trace_px[1] += *dy;
    if (armTicksToNs(now - g_gyro_trace_tick) >= 1000000000ull) {
        if (g_gyro_trace_n) {
            float t[2] = {0.0f, 0.0f};
            gyro_tilt(t);
            std::printf("gyro: 1 s: mode %d, %u samples, interval %llu..%llu ns, gravity %+.2f %+.2f %+.2f G, "
                        "rotation x %+.3f y %+.3f z %+.3f rev, cursor %+.0f %+.0f px, "
                        "tilt %+.1f %+.1f deg (from %+.1f %+.1f)\n",
                        mode, g_gyro_trace_n, (unsigned long long)g_gyro_trace_dt_min,
                        (unsigned long long)g_gyro_trace_dt_max, g_grav[0], g_grav[1], g_grav[2],
                        g_gyro_trace_rot[0], g_gyro_trace_rot[1], g_gyro_trace_rot[2],
                        g_gyro_trace_px[0], g_gyro_trace_px[1],
                        t[0] * 57.2958f, t[1] * 57.2958f, g_tilt0[0] * 57.2958f, g_tilt0[1] * 57.2958f);
            std::fflush(stdout);
        }
        g_gyro_trace_tick = now;
        g_gyro_trace_n = 0;
        g_gyro_trace_dt_min = ~0ull;
        g_gyro_trace_dt_max = 0;
        for (float& r : g_gyro_trace_rot) r = 0.0f;
        g_gyro_trace_px[0] = g_gyro_trace_px[1] = 0.0f;
    }
    return mode;
}

// The GLOBAL default (RÉGLAGES > VITESSE) back in force: its file, or x1.0
// when there is none. The file only exists once VITESSE has been changed, and
// keeping whatever was in force without it carried a game's own speed into
// the library, into the next game, and into the game that had just dropped
// it (X on the row in the pause, which then still read "par défaut").
static void cursor_speed_load() {
    int v = 1;
    FILE* f = std::fopen(app_file_path("cursor_speed"), "rb");
    if (f) {
        if (std::fscanf(f, "%d", &v) != 1) v = 1;
        std::fclose(f);
    }
    g_cursor_speed_idx = v;
    cursor_speed_apply();
}
// The preset in force, for the rows that show and step it.
extern "C" int ruffle_cursor_speed_index(void) {
    return g_cursor_speed_idx;
}
// Apply preset `idx` WITHOUT keeping it: an armed row moves the pointer's
// speed live, and only A writes it (input rule 2); B puts the old one back
// through here too.
extern "C" void ruffle_cursor_speed_preview(int idx) {
    g_cursor_speed_idx = idx;
    cursor_speed_apply();
}
// The global default back in force, for a game that just dropped its own
// speed (X on the row, in game).
extern "C" void ruffle_cursor_speed_follow_global(void) {
    cursor_speed_load();
}
// The cursor speed a game runs at: its own preset (<basename>.cursor) when it
// has one, else the GLOBAL default (RÉGLAGES). Read at launch, and again after
// the Touches pages commit something: a profile applied in game writes the
// game's speed, and the pointer used to keep the old one until a relaunch,
// the Touches row showing it and a step there overwriting the profile's.
static void cursor_speed_for_game() {
    const int cs = ruffle_keymap_cursor_speed();
    if (cs >= 0) {
        g_cursor_speed_idx = cs;
        cursor_speed_apply();
    } else {
        cursor_speed_load(); // global default
    }
}
// Apply preset `idx` and keep it. Returns the index in force.
extern "C" int ruffle_cursor_speed_set(int idx) {
    g_cursor_speed_idx = idx;
    cursor_speed_apply();
    if (g_in_game) {
        // In a game → save to THIS game's <basename>.cursor (via Rust). No bleed
        // onto other games or the global default.
        ruffle_keymap_set_cursor_speed(g_cursor_speed_idx);
    } else {
        // Library / RÉGLAGES → the GLOBAL default file.
        FILE* f = std::fopen(app_file_path("cursor_speed"), "wb");
        if (f) { std::fprintf(f, "%d", g_cursor_speed_idx); std::fclose(f); flashnx_commit_sd(); }
    }
    return g_cursor_speed_idx;
}
// Current multiplier x10 (5,10,15,20,25) for the UI label "x1.5".
extern "C" int ruffle_cursor_speed_mult_x10(void) {
    return (int)(CURSOR_SPEED_MULTS[g_cursor_speed_idx] * 10.0f + 0.5f);
}

// Register a Sphaira file association so a `.swf` in Sphaira's file browser
// offers FlashNX as a launcher. That's what makes Sphaira's "Create a Forwarder"
// entry able to build a per-game Home-menu shortcut for a Flash game: it
// launches FlashNX with the `.swf` as argv (see the forwarder support in
// worker_entry/main). Without this association only generic players (e.g. nxmp)
// show up for `.swf`. We only write it when Sphaira is installed (its config dir
// exists) so we never litter other setups, and only when the file is absent so a
// user edit is never clobbered. `self_nro` is argv[0] (e.g.
// "sdmc:/switch/FlashNX/FlashNX.nro"); Sphaira wants the path without the
// "sdmc:" mount prefix. Format mirrors Sphaira's own bundled assoc .ini files
// (plain key=value, no section header).
static void register_sphaira_assoc(const char* self_nro) {
    struct stat st;
    if (stat("sdmc:/config/sphaira", &st) != 0) {
        return; // Sphaira not installed — nothing to register with.
    }
    mkdir("sdmc:/config/sphaira/assoc", 0777); // no-op if it already exists
    const char* ini = "sdmc:/config/sphaira/assoc/FlashNX.ini";
    if (stat(ini, &st) == 0) {
        return; // already registered — leave any user edits alone.
    }
    const char* nro = "/switch/FlashNX/FlashNX.nro"; // sane default
    if (self_nro && self_nro[0]) {
        nro = (std::strncmp(self_nro, "sdmc:", 5) == 0) ? self_nro + 5 : self_nro;
    }
    FILE* f = std::fopen(ini, "w");
    if (!f) {
        std::printf("sphaira: could not write %s\n", ini); std::fflush(stdout);
        return;
    }
    std::fprintf(f, "path=%s\nsupported_extensions=swf\n", nro);
    std::fclose(f);
    std::printf("sphaira: registered .swf association -> %s (launcher %s)\n", ini, nro);
    std::fflush(stdout);
}

// Where the HOME-menu tiles find FlashNX when it is not at its usual place.
//
// A tile's forwarder loads `sdmc:/switch/FlashNX/FlashNX.nro`, the path the
// README and the Homebrew App Store use. A `.nro` taken from the releases is
// often dropped straight into `/switch/` instead, where FlashNX runs fine and
// every tile it installs fails to start (2346-0003). So FlashNX notes the path it
// was started from, and the forwarder reads the note when the usual path holds
// nothing (see `forwarder/source/main.c`).
//
// Written only when it says something: at the usual path the note is never
// read, and an unchanged note is not written again, so a normal boot costs one
// string compare. Must stay out of self-contained builds (#106), like the
// Sphaira association: tiles would then start that one game whatever they were
// made for.
static void remember_own_path(const char* self_nro) {
    static const char* const usual = "sdmc:/switch/FlashNX/FlashNX.nro";
    static const char* const note = "sdmc:/switch/FlashNX/tiles/nro.path";
    if (!self_nro || !self_nro[0]) return;
    char path[512];
    if (self_nro[0] == '/') {
        std::snprintf(path, sizeof(path), "sdmc:%s", self_nro);
    } else if (std::strncmp(self_nro, "sdmc:/", 6) == 0) {
        std::snprintf(path, sizeof(path), "%s", self_nro);
    } else {
        return; // not on the card, the only device the forwarder mounts
    }
    const size_t n = std::strlen(path);
    if (n < 4 || strcasecmp(path + n - 4, ".nro") != 0) return;
    if (strcasecmp(path, usual) == 0) return;
    struct stat st;
    if (::stat(path, &st) != 0) return; // argv[0] cut at a space, or a stale path

    char old[512];
    if (FILE* f = std::fopen(note, "rb")) {
        const size_t got = std::fread(old, 1, sizeof(old) - 1, f);
        std::fclose(f);
        old[got] = '\0';
        if (std::strcmp(old, path) == 0) return;
    }
    ::mkdir("sdmc:/switch/FlashNX", 0777);
    ::mkdir("sdmc:/switch/FlashNX/tiles", 0777);
    FILE* f = std::fopen(note, "wb");
    if (!f) {
        std::printf("tiles: could not write %s\n", note);
        std::fflush(stdout);
        return;
    }
    std::fputs(path, f);
    std::fclose(f);
    std::printf("tiles: FlashNX runs from %s, noted in %s for the HOME Menu tiles\n", path, note);
    std::fflush(stdout);
}

// FlashNX's own file and arguments, for an update to replace the file and start
// it again (rust/src/updater.rs). Empty when argv[0] is not a file on the card,
// and in a self-contained build (#106), which is never updated in place.
static char g_self_nro[512];
static char g_self_args[2048];
// An update asked to start FlashNX again on the way out (see the end of main).
static bool g_restart_requested = false;

static void remember_self(int argc, char** argv) {
    if (argc < 1 || !argv[0] || !argv[0][0]) return;
    const char* a0 = argv[0];
    char path[sizeof(g_self_nro)];
    if (a0[0] == '/') {
        std::snprintf(path, sizeof(path), "sdmc:%s", a0);
    } else if (std::strncmp(a0, "sdmc:/", 6) == 0) {
        std::snprintf(path, sizeof(path), "%s", a0);
    } else {
        return;
    }
    struct stat st;
    if (::stat(path, &st) != 0) return;
    std::snprintf(g_self_nro, sizeof(g_self_nro), "%s", path);
    // The same arguments again, each quoted (a path can hold spaces), so a tile
    // started again is still a tile and a game started again is that game.
    size_t n = (size_t)std::snprintf(g_self_args, sizeof(g_self_args), "\"%s\"", path);
    for (int i = 1; i < argc && n < sizeof(g_self_args); ++i) {
        if (!argv[i] || std::strchr(argv[i], '"')) continue;
        n += (size_t)std::snprintf(g_self_args + n, sizeof(g_self_args) - n, " \"%s\"", argv[i]);
    }
}

// FlashNX's own `.nro` (`sdmc:/...`) into `out`: its length, 0 when unknown.
extern "C" int flashnx_self_nro(char* out, int cap) {
    const int n = (int)std::strlen(g_self_nro);
    if (n == 0 || !out || cap <= n) return 0;
    std::memcpy(out, g_self_nro, (size_t)n + 1);
    return n;
}

// 1 when FlashNX can start itself again on the way out: the loader that started
// it (hbmenu's nx-hbloader, a tile's forwarder) reads a next path to load.
extern "C" int flashnx_restart_supported(void) {
    return (g_self_nro[0] != 0 && envHasNextLoad()) ? 1 : 0;
}

extern "C" char* fake_heap_start;
extern "C" char* fake_heap_end;
extern "C" void __appExit(void);
extern "C" void NX_NORETURN __nx_exit(Result rc, LoaderReturnFn retaddr);

// libnx's own `__libnx_exit` (runtime/init.c), with one step added for an
// update's restart: the heap is wiped before the loader gets it back.
//
// The FlashNX started next gets the very same heap from the loader, and
// newlib's calloc takes memory fresh from sbrk to be zero already (its
// MORECORE_CLEARS). From the kernel it is, and after hbmenu it nearly is,
// hbmenu being small. After a FlashNX it is gigabytes of the last run's data:
// the restarted FlashNX died inside malloc while Mesa compiled its first
// shaders, on structures calloc had handed over full of old pointers
// (2026-10-17, "the software was closed because an error occurred", no
// Atmosphère report, a DataAbort in ruffle-crash.log). dlmalloc assumes the
// same of the big blocks it is given.
//
// After __appExit, so every service is closed and nothing frees into the heap
// once it is wiped. Only what is mapped read-write is touched: memory still
// lent to a service would fault.
extern "C" void NX_NORETURN __libnx_exit(int rc) {
    (void)rc;
    __appExit();
    if (g_restart_requested) {
        uintptr_t a = (uintptr_t)fake_heap_start;
        const uintptr_t end = (uintptr_t)fake_heap_end;
        while (a < end) {
            MemoryInfo info;
            u32 page = 0;
            if (R_FAILED(svcQueryMemory(&info, &page, a)) || info.size == 0) break;
            const uintptr_t lo = a;
            uintptr_t hi = (uintptr_t)(info.addr + info.size);
            if (hi > end) hi = end;
            if ((info.perm & Perm_Rw) == Perm_Rw && hi > lo) {
                std::memset((void*)lo, 0, hi - lo);
            }
            a = hi;
        }
    }
    __nx_exit(0, envGetExitFuncPtr());
}

// Ask the loader to start FlashNX again once it exits. 1 when the loader took it.
extern "C" int flashnx_request_restart(void) {
    if (!g_self_nro[0] || !envHasNextLoad()) return 0;
    if (R_FAILED(envSetNextLoad(g_self_nro, g_self_args))) return 0;
    g_restart_requested = true;
    std::printf("update: FlashNX starts again from %s on exit\n", g_self_nro);
    std::fflush(stdout);
    return 1;
}

// True if `path` ends in ".swf" (case-insensitive). Recognises a forwarder
// launch argument — a HOME-menu tile, or a forwarder built by hand.
static bool path_is_swf(const char* path) {
    if (!path) return false;
    const size_t n = std::strlen(path);
    if (n < 4) return false;
    const char* e = path + (n - 4);
    return e[0] == '.'
        && (e[1] == 's' || e[1] == 'S')
        && (e[2] == 'w' || e[2] == 'W')
        && (e[3] == 'f' || e[3] == 'F');
}

// True if `path` ends in ".cfg", which is how a HOME-menu tile names its game.
static bool path_is_tile_cfg(const char* path) {
    if (!path) return false;
    const size_t n = std::strlen(path);
    if (n < 4) return false;
    const char* e = path + (n - 4);
    return e[0] == '.'
        && (e[1] == 'c' || e[1] == 'C')
        && (e[2] == 'f' || e[2] == 'F')
        && (e[3] == 'g' || e[3] == 'G');
}

// Read the `.swf` path a HOME-menu tile stored for itself, or nullptr.
//
// A tile passes THIS file's path and not the game's, because the homebrew ABI
// hands argv over as one string that gets re-split on spaces and plenty of games
// have spaces in their names. A path under `tiles/` is named after a program ID,
// so it never does. See forwarder/source/main.c.
//
// The buffer is static because the pointer outlives this call: it goes to the
// worker thread, exactly like an `argv[i]` would.
static const char* resolve_tile_cfg(const char* cfg_path) {
    static char swf[768];
    FILE* f = std::fopen(cfg_path, "rb");
    if (!f) {
        std::printf("tile: no config at %s\n", cfg_path);
        std::fflush(stdout);
        return nullptr;
    }
    const size_t n = std::fread(swf, 1, sizeof(swf) - 1, f);
    std::fclose(f);
    swf[n] = '\0';
    // One line: anything from the first newline on is not part of the path.
    for (size_t i = 0; i < n; i++) {
        if (swf[i] == '\r' || swf[i] == '\n') {
            swf[i] = '\0';
            break;
        }
    }
    if (!path_is_swf(swf)) {
        // A tile whose game was deleted, or a file somebody edited. Falling
        // through to the library is the useful failure: the player lands
        // somewhere they can pick a game rather than on an error.
        std::printf("tile: %s does not name a .swf\n", cfg_path);
        std::fflush(stdout);
        return nullptr;
    }
    std::printf("tile: %s -> %s\n", cfg_path, swf);
    std::fflush(stdout);
    return swf;
}

// All of the GL + Ruffle work runs in a dedicated worker thread with a
// 32 MB stack (vs the default ~1 MB main-thread stack from nx-hbloader).
// Mario 63's AS2 preload recursion + Ruffle's AVM1 interpreter + GC arena
// traversal blew through 1 MB at frame ~40, producing a silent SIGSEGV
// that left no Rust-panic trail. With 32 MB we should have headroom for
// any AS2 game we'll realistically run.
static void worker_entry(void* arg) {
    // A non-null arg means "boot this single `.swf` and skip the library". Two
    // things produce one: a HOME-menu shortcut (NSP forwarder) that named a game
    // on the command line, and a self-contained build carrying its game in the
    // .nro's RomFS (#106, path `romfs:/<name>.swf`). Both hand over a pointer
    // that outlives the thread -- an `argv[i]` from main, or the static buffer
    // in swf_picker.cpp -- and main blocks on threadWaitForExit anyway.
    // NULL = normal launch → show library.
    const char* forwarder_swf = static_cast<const char*>(arg);
    // ...unless we only have the ALBUM's memory pool, in which case launching
    // anything would run out of heap. The notice that explains this to the
    // player lives in the library flow (`Screen::AppletNotice`), and a forwarder
    // skips that flow entirely, so this path had no guard at all and simply
    // OOM'd. Dropping the target falls through to the library, which says why.
    //
    // Present since forwarder launches shipped in v1.3.0 and never hit, because
    // making a shortcut meant going through Sphaira on purpose. The in-app
    // "create a shortcut" row changes that: hbmenu is usually opened from the
    // album, and now it has game entries sitting in it.
    if (forwarder_swf && ruffle_is_applet_mode()) {
        std::printf("boot: applet memory pool — ignoring '%s', opening the library "
                    "so the takeover notice is shown\n", forwarder_swf);
        std::fflush(stdout);
        forwarder_swf = nullptr;
    }
    std::printf("worker: starting (32 MB stack), %.0f ms after main()\n",
                boot_ms_since(g_boot_t0)); std::fflush(stdout);

    // DIAG (worker-TLS fault): this worker is a raw libnx thread. The kernel
    // sets TPIDRRO_EL0 (IPC/syscall TLS) but leaves TPIDR_EL0 (ELF/compiler
    // TLS, used by Rust's #[thread_local]) = 0. Ruffle's hot path never touches
    // TLS so the game runs fine — but the moment a Rust panic fires,
    // std::panicking::panic_with_hook reads the `panic_count` thread-local via
    // TPIDR_EL0 and faults at 0x100, so our panic hook never runs and the panic
    // MESSAGE is lost (we only see the native data abort, see exception.cpp).
    // Point TPIDR_EL0 at a zeroed scratch block so panic_count reads 0 and the
    // hook can format + dump the real message. Safe: TPIDR_EL0 was unused (0)
    // and syscalls use TPIDRRO_EL0, not this register.
    static unsigned char s_worker_tls[0x1000] __attribute__((aligned(16))) = {0};
    __asm__ volatile("msr tpidr_el0, %0" :: "r"(&s_worker_tls[0]));

    NWindow* win = nwindowGetDefault();
    // Boot at PANEL size: the library UI is what shows first and it wants to be
    // sharp. Games drop to GAME_VIEWPORT_* via gl_context_resize at launch.
    const uint64_t t_gl = armGetSystemTick();
    if (!gl_context_init(win)) {
        std::printf("gl_context_init failed\n"); std::fflush(stdout);
        return;
    }
    std::printf("boot: gl_context_init %.0f ms\n", boot_ms_since(t_gl));
    std::fflush(stdout);

    // Pad + touch init moved BEFORE the library so both the library boot
    // screen and the in-game loop share one PadState instance. Pad config
    // doesn't change between the two phases.
    PadState pad;
    padConfigureInput(1 + EXTRA_PLAYERS, HidNpadStyleSet_NpadStandard); // up to 4 players (#40, #128)
    // A Joy-Con given to a player alone is held on its side (issue #128): it
    // is what the console's controller screen shows and asks SL+SR for. The
    // turn of its buttons and stick is ours (`flashnx_pad_update`).
    hidSetNpadJoyHoldType(HidNpadJoyHoldType_Horizontal);
    // One Joy-Con on the console is enough for handheld mode. By default the
    // console wants both: with the right one taken off to play at a distance,
    // the left one still on its rail gave nothing at all (`pads:` log,
    // 2026-10-08). Now the attached one stays the handheld controller, the
    // detached one is player 1, and `pad` reads both. Only for this process.
    hidSetNpadHandheldActivationMode(HidNpadHandheldActivationMode_Single);
    padInitializeDefault(&pad);
    // Players 2 to 4 (issues #40, #128). Idle / absent = no input, so
    // single-player is unaffected. Local multiplayer Flash games (e.g. DBZ
    // Devolution, Super Smash Flash 2) read several key-sets on one keyboard;
    // each controller feeds its player's keymap into the same ruffle_handle_key
    // pipeline. A full controller, a Joy-Con pair or one Joy-Con on its side.
    PadState pads_n[EXTRA_PLAYERS];
    for (int p = 0; p < EXTRA_PLAYERS; ++p) {
        padInitializeWithMask(&pads_n[p], 1UL << (HidNpadIdType_No2 + p));
    }

    hidInitializeTouchScreen();
    HidTouchScreenState touch_state = {0};
    bool touch_was_pressed = false;

    // Tick clock — used by hold-to-repeat in the menu loops and by the
    // game-loop dt pacing later. Hoisted here so both phases share it.
    const uint64_t tick_freq_global = ruffle_tick_freq();

    // Outer loop: library phase → game phase → (QUITTER) back to library.
    // `exit_nro = true` on any unrecoverable failure or when the user
    // chose to leave the launcher via QUITTER on the library screen.
    bool exit_nro = false;
    while (!exit_nro) {
    // ── Phase 3.4: library launcher (FlashNX picker) ───────────────────
    //
    // Show the library UI: scan SD, list all .swf files, let the user pick
    // one. A=JOUER → ruffle_set_swf_path + boot Ruffle. -=QUITTER from
    // library → break out of the outer loop and exit the .nro. Two
    // SwitchRenderBackend instances exist over the boot — one for the
    // library (dropped before Ruffle's own gets built; ~96 MB GPU arena
    // per instance, never alive at the same time). On each iteration of
    // the outer loop a fresh library renderer + banner upload happens.
    // Global defaults, on BOTH paths.
    //
    // `loc::init` used to run only inside `ruffle_library_init` below, i.e.
    // only when the library phase happens. A forwarder launch skips that whole
    // block, so a game started from a HOME-menu shortcut ran with none of the
    // player's REGLAGES defaults: not just the UI language, but the display
    // mode, the rotation, the zoom, the screen filter and the overclock, each
    // falling back to 0. Per-game values still worked, because those come from
    // the game's own `.prefs`; it was the fallback underneath them that was
    // missing, which is why it looked like nothing more than "the menus are in
    // English". Reported from a Sphaira shortcut on 2026-08-26.
    //
    // Costs one small file read, and the library path re-reads it a moment
    // later, which is harmless.
    ruffle_loc_init();

    if (!forwarder_swf) {
    g_in_game = false;
    // The library / RÉGLAGES cursor speed is the GLOBAL default — re-read it on
    // entering the library so it doesn't show the last game's per-game speed.
    cursor_speed_load();
    const uint64_t t_libinit = armGetSystemTick();
    if (ruffle_library_init() != 0) {
        std::printf("library_init failed — exiting .nro\n");
        std::fflush(stdout);
        break;
    }
    const double ms_libinit = boot_ms_since(t_libinit);
    // Enumerate every .swf on SD and push to the Rust library state.
    const uint64_t t_scan = armGetSystemTick();
    swf_picker_run();
    const double ms_scan = boot_ms_since(t_scan);
    const uint64_t t_open = armGetSystemTick();
    ruffle_library_open();
    // The trash of deleted games empties while the library is up (and only
    // then: a game has the card to itself).
    swf_picker_trash_start();
    std::printf("boot: library_init %.0f ms | swf scan %.0f ms | library_open %.0f ms"
                " | total since main %.0f ms\n",
                ms_libinit, ms_scan, boot_ms_since(t_open), boot_ms_since(g_boot_t0));
    std::fflush(stdout);

    MenuRepeatState lib_repeat;
    menu_repeat_reset(lib_repeat);
    NavLatch lib_latch;
    nav_latch_reset(lib_latch);
    bool first_lib_frame = true;
    // Keep BOTH loop conditions so the exit can be attributed. They mean very
    // different things — `library_active=0` is the app's own state machine
    // saying "done", `appletMainLoop=0` is the SYSTEM asking us to quit — and
    // the code after the loop treats them identically ("user quit"), which
    // made a system-initiated exit indistinguishable from a crash.
    bool lib_active = true;
    bool applet_ok = true;
    // Same 30-frame slot as the in-game loop, for the one case that can reach
    // here with the clock still raised: a restore that failed on the way out of
    // a game. Without this the library is the one place a raised clock could
    // sit forever with nothing coming back to finish the job, and the library
    // is exactly where a console is left sitting.
    int lib_clock_check = 0;
    while ((lib_active = (ruffle_library_active() != 0))
           && (applet_ok = appletMainLoop())) {
        if (++lib_clock_check >= 30) {
            lib_clock_check = 0;
            flashnx_clocks_reassert();
        }
        // Not as player 1 (issue #128): the launcher has no pause, so a right
        // Joy-Con's + stays +, the Options its hints name. Read as the pause's
        // `-`, it ran the search instead, and Options was out of reach on a
        // lone Joy-Con of either side. Its B still goes back, as `-` does.
        flashnx_pad_update(&pad, PAD_FLAG_MENU);
        u64 kDownLib = padGetButtonsDown(&pad);
        const u64 kUpLib   = padGetButtonsUp(&pad);
        u64 kHeldLib = padGetButtons(&pad);
        // One axis at a time for every list of the launcher (see nav_filter).
        nav_filter(lib_latch, &pad, &kHeldLib, &kDownLib);
        const u64 now_lib  = ruffle_tick_now();
        // Forward joycon edges + auto-repeat (D-pad/sticks) into the
        // library state machine. Pause / Plus / face buttons are one-shot
        // (no repeat) — see MENU_NAV_BUTTONS.repeat flags.
        // Which nav directions are held RIGHT NOW, before any event goes out.
        // The filter keeps each stick and the d-pad to one axis, but two of
        // them can still be held on different axes (the d-pad down, a stick
        // to the side), and each direction is forwarded on its own -- where
        // one of the two is a multi-game jump, the pair moved five games for
        // one flick. The Rust side suppresses the jump when the other axis is
        // engaged, which it can only know from the held state; the events
        // themselves carry no such context.
        {
            unsigned char nav = 0;
            if (kHeldLib & (HidNpadButton_Up | HidNpadButton_StickLUp | HidNpadButton_StickRUp))
                nav |= 1;
            if (kHeldLib & (HidNpadButton_Down | HidNpadButton_StickLDown | HidNpadButton_StickRDown))
                nav |= 2;
            if (kHeldLib & (HidNpadButton_Left | HidNpadButton_StickLLeft | HidNpadButton_StickRLeft))
                nav |= 4;
            if (kHeldLib & (HidNpadButton_Right | HidNpadButton_StickLRight | HidNpadButton_StickRRight))
                nav |= 8;
            ruffle_library_nav_held(nav);
        }
        menu_repeat_step(lib_repeat, kDownLib, kUpLib, kHeldLib,
                         now_lib, tick_freq_global, ruffle_library_input);
        // Hidden ZL+ZR chord: emit one synthetic "ZL+ZR" event on the frame the
        // pair completes (both held now, not both held before this frame's new
        // presses). The library only acts on it in the Flashpoint results grid
        // (toggle the content filter, issue #33; "Relancer" on a shelf's page);
        // ZL/ZR are no-ops there on their own, so the individual events
        // menu_repeat_step also forwarded are inert.
        {
            const u64 ZLZR = HidNpadButton_ZL | HidNpadButton_ZR;
            const bool both_now  = (kHeldLib & ZLZR) == ZLZR;
            const bool both_prev = ((kHeldLib & ~kDownLib) & ZLZR) == ZLZR;
            if (both_now && !both_prev) {
                ruffle_library_input("ZL+ZR");
            }
        }
        // Touchscreen: drag to scroll the JOUER gallery, tap a game to select,
        // tap the selected game again to launch. Rust owns the gesture logic.
        hidGetTouchScreenStates(&touch_state, 1);
        const bool lib_touch = touch_state.count > 0;
        // Touch arrives in PANEL units; the UI renders at panel size so this is 1:1.
        const float lib_tx = lib_touch ? (float)touch_state.touches[0].x * UI_TOUCH_SCALE_X : 0.0f;
        const float lib_ty = lib_touch ? (float)touch_state.touches[0].y * UI_TOUCH_SCALE_Y : 0.0f;
        ruffle_library_touch(lib_tx, lib_ty, lib_touch ? 1 : 0);
        // The trash of deleted games empties only while nothing is held or
        // touched (it competes with the interface for the card).
        swf_picker_trash_frame((padGetButtons(&pad) != 0 || lib_touch) ? 1 : 0);
        ruffle_library_render();
        gl_context_swap();
        if (first_lib_frame) {
            first_lib_frame = false;
            std::printf("boot: FIRST LIBRARY FRAME ON SCREEN at %.0f ms after main()\n",
                        boot_ms_since(g_boot_t0));
            std::fflush(stdout);
        }
    }

    {
        char m[160];
        std::snprintf(m, sizeof(m),
            "main: library loop EXIT (library_active=%d appletMainLoop=%d picked=%d)\n",
            (int)lib_active, (int)applet_ok, ruffle_library_picked());
        ruffle_log_cstr(m); // goes to the SD trace too, unlike printf
    }
    const bool picked = ruffle_library_picked() != 0;
    char selected_path[512] = {0};
    if (!picked || ruffle_library_selected_path(selected_path, sizeof(selected_path)) != 0) {
        std::printf("library: user quit (no selection) — exiting .nro\n");
        std::fflush(stdout);
        ruffle_library_shutdown();
        ruffle_log_cstr("teardown: library shut down\n");
        break;
    }
    std::printf("library: user picked %s\n", selected_path);
    std::fflush(stdout);
    // Forward the choice to Ruffle's loader (consumed by find_and_
    // load_swf_uncached → CACHED_SWF in lib.rs).
    ruffle_set_swf_path(selected_path);

    // A game is starting: the trash waits for the library to come back.
    swf_picker_trash_pause();

    // Drop the library's standalone SwitchRenderBackend so its GL
    // resources (~96 MB arenas + shader programs + banner texture)
    // free BEFORE ruffle_init allocates Ruffle's own renderer. Without
    // this we'd peak at ~200 MB GPU during the cross-over second.
    ruffle_library_shutdown();
    } else {
        // Forwarder launch: no library UI — point the loader straight at the
        // .swf the HOME-menu shortcut named, then boot Ruffle like a normal pick.
        std::printf("forwarder: launching %s directly (skipping library)\n",
                    forwarder_swf);
        std::fflush(stdout);
        ruffle_set_swf_path(forwarder_swf);
        // Quitting will close FlashNX, not go back to a library: the pause
        // says so.
        ruffle_set_direct_launch(1);
    }

    // Drop the internal resolution for gameplay (see the GAME_VIEWPORT_* comment).
    // Done HERE: after the library's renderer is gone, before Ruffle's is built —
    // the new renderer picks up the game viewport, and the UI never rendered at
    // this size. Covers the forwarder path too, which joins us just above. On
    // failure we simply keep the current surface, so a game runs at panel size.
    if (GAME_VIEWPORT_W != UI_VIEWPORT_W || GAME_VIEWPORT_H != UI_VIEWPORT_H) {
        gl_context_resize(GAME_VIEWPORT_W, GAME_VIEWPORT_H);
    }

    if (ruffle_init() != 0) {
        std::printf("ruffle_init failed — exiting .nro\n");
        std::fflush(stdout);
        break;
    }

    // Resolve "A", "B", ..., "StickLDown" to their SK_* codes per the
    // user's keymap (loaded inside ruffle_init via keymap::init_for_swf).
    // Re-runs on each game iteration so the new pick's per-game sidecar
    // is honoured if the user back-to-library'd and picked a different SWF.
    populate_bindings_from_keymap();

    // Cursor speed: honour this game's per-game preset (<basename>.cursor) if
    // set, else fall back to the GLOBAL default (RÉGLAGES). No bleed from the
    // previously launched game.
    cursor_speed_for_game();
    g_in_game = true;
    prof_game_active(1);

    // Mouse cursor — centred at start.
    // In-game cursor: lives in the GAME's render space.
    float cursor_x = GAME_VIEWPORT_W * 0.5f;
    float cursor_y = GAME_VIEWPORT_H * 0.5f;
    ruffle_handle_mouse_move((int)cursor_x, (int)cursor_y);
    // After a Restart: the new Player draws the cursor in the middle, so it
    // goes back there. It used to keep its old position here, and the first
    // move made it jump back to where it was before the restart. The
    // gyroscope's aim and tilt anchor start again from the middle too.
    auto recenter_cursor = [&]() {
        cursor_x = GAME_VIEWPORT_W * 0.5f;
        cursor_y = GAME_VIEWPORT_H * 0.5f;
        ruffle_handle_mouse_move((int)cursor_x, (int)cursor_y);
        g_aim_valid = false;
        g_tilt_anchor_valid = false;
    };
    touch_was_pressed = false;
    // Touch tap -> left click, held back until the movie has run a frame with
    // the new cursor position (#87). `touch_press_at` is the tick that press is
    // due on; `touch_press_held` tracks a button we still owe a release.
    bool     touch_press_armed = false;
    uint64_t touch_press_at    = 0;
    bool     touch_press_held  = false;
    // Frames since this game started, for the one-shot display-list dump (#100).
    uint32_t frames_since_start = 0;

    // Real-time pacing: instead of telling Ruffle "16.6 ms elapsed" every tick,
    // we measure actual wall-clock between iterations and let its frame
    // accumulator decide how many SWF frames to run. Matches the desktop
    // Ruffle pacing model (core/src/player.rs::tick).
    const uint64_t tick_freq = ruffle_tick_freq();
    uint64_t last_tick = ruffle_tick_now();

    // Leave a pause: hand the movie the interval it never simulated so
    // getTimer() resumes where it stopped (#87), then restart the clock. Every
    // resume path goes through here — pause menu, ECRAN, TOUCHES, in-game
    // keyboard — because `last_tick` is only advanced by a frame that actually
    // ticked, so `now - last_tick` IS the unplayed gap.
    auto resume_after_pause = [&last_tick, tick_freq]() {
        const uint64_t now = ruffle_tick_now();
        if (tick_freq > 0 && now > last_tick) {
            ruffle_skip_paused_time(((now - last_tick) * 1000000ULL) / tick_freq);
        }
        last_tick = now;
    };

    // Pause-modal state. While `menu_open` is true the Ruffle frame loop is
    // skipped (we just re-render the last Player state under the overlay)
    // and joycon input is rerouted to menu navigation only.
    bool menu_open = false;
    // While true, the pause menu is playing its scale-out close pop; input is
    // suspended and we drain `ruffle_draw_menu_closing` until it reports done,
    // then resume the game. Keeps the close animated instead of snapping shut.
    bool menu_closing = false;
    // Buttons pressed while the close animation plays, for both players. Those
    // frames read no input, so a press made then had lost its edge by the
    // first frame of the game: a jump pressed right after `-`, or a direction
    // held to run, did nothing until it was let go and pressed again. The
    // first frame back gives the game the ones still held.
    u64  close_drain = 0;
    u64  close_drain_n[EXTRA_PLAYERS] = {};
    int  menu_selection = MENU_RESUME;
    // ECRAN sub-panel: open flag + its own row cursor, kept out of
    // `menu_selection` so backing out lands you on ECRAN rather than on
    // whatever row index the sub-panel happened to stop on.
    bool screen_menu = false;
    int  screen_selection = SCREEN_DISPLAY;
    // ZOOM framing mode (issue #101): a state INSIDE the ECRAN branch, because
    // backing out of it must land on the ECRAN rows, not on the game.
    bool zoom_mode = false;
    // Frames the zoom keys have been held, so a long press accelerates. One
    // percent a frame is the right grain to land on a value and much too slow to
    // cross the whole range, which is what this fixes.
    int  zoom_hold = 0;
    // Drag state for framing with a finger: the previous sample, and whether
    // there IS one. Without the flag, lifting and putting the finger down
    // elsewhere would jump the framing by the gap between the two.
    bool zoom_touching = false;
    int  zoom_touch_x = 0, zoom_touch_y = 0;
    // Pinch state: whether two fingers were down last frame and how far apart.
    bool  zoom_pinching = false;
    float zoom_pinch_dist = 0.0f;
    // Frames the d-pad has been held, so a tap stays one pixel and a hold ramps.
    int  pan_hold = 0;
    // Same, for the fine zoom on X / Y: a tap is one percent.
    int  zoom_fine_hold = 0;
    // A finger already on the screen when a pause panel comes up: when the
    // framing mode ends (one finger moves the picture there) or when `-` opens
    // the pause over the game. Until it lifts, the panels get no touch: its
    // first sample read as a new press, and lifting it then tapped whatever
    // the panel had drawn under it, setting and saving an option the player
    // never touched.
    bool touch_wait_lift = false;
    // MENU_QUIT now means "back to library" — controlled by this flag.
    // appletMainLoop returning false (home button → Close) also exits the
    // inner loop but with back_to_library=false → full .nro exit.
    bool back_to_library = false;

    // Power mode is applied on the Rust side, from this game's own preference,
    // next to its screen filter (`ruffle_init`). Nothing to do here.
    //
    // There is deliberately no marker file and no button combination. ZL+ZR
    // used to do it and that was a real hazard: the neighbouring SCREEN panel
    // binds zoom to ZR|R and ZL|L, so a player who had learned "the triggers
    // zoom" could raise the console's clock without ever knowing it happened.
    // A `power.high` marker replaced it and was no better in the end, just
    // quieter: a file left on the card silently forces every game to a raised
    // clock, with nothing on screen ever saying so. A labelled row, reached
    // with UP/DOWN and confirmed with A, is the whole point.

    // Hold-to-repeat state for the in-game TOUCHES editor (16 entries
    // scrollable list — benefits a lot from D-pad auto-repeat).
    MenuRepeatState touches_repeat;
    menu_repeat_reset(touches_repeat);
    PauseRepeat pause_rep;
    pause_repeat_reset(pause_rep);
    // One axis at a time on every page of the pause (see nav_filter).
    NavLatch pause_latch;
    nav_latch_reset(pause_latch);

    // CpuBoostMode re-assert counter. A download raises FastLoad for its own
    // duration (net.cpp) and resets it on completion, but an aborted/errored
    // transfer could leave it raised — which now COSTS us roughly half our
    // framerate (see the measurements at the boot-time call). Re-asserting
    // Normal every 30 frames makes gameplay self-healing without IPC spam.
    int boost_reassert = 0;

    // Player 1's stick on a lone Joy-Con (issue #128; what it does is told
    // where the loop reads it, before the key bindings). Kept per game: as
    // statics in the loop, a game quit with the stick on the cursor had the
    // next one start there, the stick moving the gold cursor instead of the
    // player. Restart keeps it, being the same game.
    bool stick_cursor = false;
    bool stick_cursor_click = false;  // A holds the click it pressed
    u64  press_since = 0;             // tick the stick went in, 0 = out
    bool press_long = false;          // this press already switched
    int  press_tap_frames = 0;        // a short push's key, held this many frames more
    int  press_tap_key = SK_NONE;
    ruffle_set_stick_cursor(0);
    const u64 STICK_DIRS = HidNpadButton_StickLUp | HidNpadButton_StickLDown |
                           HidNpadButton_StickLLeft | HidNpadButton_StickLRight;
    auto p1_key = [](int key, bool down) {
        if (key == SK_MOUSE_LEFT)       ruffle_handle_mouse_button(down);
        else if (key == SK_MOUSE_RIGHT) ruffle_handle_mouse_right(down);
        else if (key == SK_KEYBOARD)  { if (down) ruffle_keyboard_request_manual(); }
        else if (key != SK_NONE)        ruffle_handle_key(key, down);
    };

    minus_watch_start();
    while (appletMainLoop()) {
        const bool p1_single = (flashnx_pad_update(&pad, PAD_FLAG_PLAYER1) & PAD_READ_SINGLE) != 0;
        // In the pause and its pages, a lone Joy-Con's SL and SR are its
        // shoulders, L and R, which the menus page with (the editor's layers,
        // its tabs): it has no others. Its trigger is ZL or ZR there, as in
        // the launcher (ZR opens the shared profiles from the editor). In the
        // game SL and SR are SL and SR and the trigger does nothing, as its
        // editor shows them (issue #128).
        const bool menu_lone = menu_open && p1_single;
        auto menu_keys = [menu_lone](u64 m) { return menu_lone ? pad_lone_menu(m) : m; };
        const u64 kDown = menu_keys(padGetButtonsDown(&pad));
        const u64 kUp   = menu_keys(padGetButtonsUp(&pad));
        const u64 kHeld = menu_keys(padGetButtons(&pad));
        // Players 2 to 4 (issues #40, #128). Read every frame; only acted on
        // during gameplay (not in the pause menu / sub-screens).
        u64 kDownP[EXTRA_PLAYERS], kUpP[EXTRA_PLAYERS], kHeldP[EXTRA_PLAYERS];
        for (int p = 0; p < EXTRA_PLAYERS; ++p) {
            flashnx_pad_update(&pads_n[p], 0);
            kDownP[p] = padGetButtonsDown(&pads_n[p]);
            kUpP[p]   = padGetButtonsUp(&pads_n[p]);
            kHeldP[p] = padGetButtons(&pads_n[p]);
        }
        // Which controller the console puts where, whenever that changes:
        // style (1 Pro, 2 handheld, 4 Joy-Con pair, 8 left alone, 16 right
        // alone), device type and Joy-Con assignment of players 1 to 4 and of
        // the handheld slot (2026-10-08: the left Joy-Con did nothing once the
        // right one was taken off).
        {
            static u32 s_pads_last[14] = {~0u, ~0u, ~0u, ~0u, ~0u, ~0u, ~0u, ~0u, ~0u, ~0u, ~0u, ~0u, ~0u, ~0u};
            u32 cur[14];
            for (int p = 0; p < 4; ++p) {
                const HidNpadIdType id = (HidNpadIdType)(HidNpadIdType_No1 + p);
                cur[p * 3] = hidGetNpadStyleSet(id);
                cur[p * 3 + 1] = hidGetNpadDeviceType(id);
                cur[p * 3 + 2] = (u32)hidGetNpadJoyAssignment(id);
            }
            cur[12] = hidGetNpadStyleSet(HidNpadIdType_Handheld);
            cur[13] = hidGetNpadDeviceType(HidNpadIdType_Handheld);
            if (std::memcmp(cur, s_pads_last, sizeof(cur)) != 0) {
                std::memcpy(s_pads_last, cur, sizeof(cur));
                std::printf("pads: No1 style 0x%x dev 0x%x assign %u | No2 0x%x 0x%x %u | No3 0x%x 0x%x %u | "
                            "No4 0x%x 0x%x %u | Handheld style 0x%x dev 0x%x\n",
                            cur[0], cur[1], cur[2], cur[3], cur[4], cur[5], cur[6], cur[7], cur[8], cur[9],
                            cur[10], cur[11], cur[12], cur[13]);
                std::fflush(stdout);
            }
        }

        if (++boost_reassert >= 30) {
            boost_reassert = 0;
            // The clock code gets its slot unconditionally and answers whether
            // it is holding anything: a raised rate to keep, a failed restore
            // to retry, or nothing at all. Asking it first is what closes the
            // hole where a restore that failed reported NORMAL, so nothing ever
            // came back to finish it.
            //
            // apm is only told "Normal" when clkrst is idle. Do NOT poke apm
            // while a clkrst rate is held: these two ran one line apart, thirty
            // times a second, for a whole session, and apm being told "Normal"
            // while clkrst held 1785 MHz made the revocation question
            // undecidable, because a revoke followed by our own repair leaves
            // no trace either way.
            //
            // A raised clock has to be re-applied on some cadence anyway:
            // waking from sleep or returning from HOME resets it, which is the
            // known failure mode of every homebrew that pins clocks. Note the
            // cadence is 30 FRAMES, not half a second: at the 25-27 fps these
            // games run at it is 1.1-1.2 s.
            if (flashnx_clocks_reassert() == 0) {
                // Nothing pinned, so the self-healing apm re-assert is back on:
                // an aborted download can leave FastLoad raised (net.cpp), and
                // FastLoad additionally throttles the GPU to its minimum.
                appletSetCpuBoostMode(ApmCpuBoostMode_Normal);
            }
        }

        if (menu_open) {
            // The pause reads `-` on its own (it closes it): a press the watch
            // thread kept now must not open the pause again once it is shut.
            s_minus_pressed.store(false, std::memory_order_relaxed);
            // ─── Closing drain: play the scale-out, then resume the game ──
            // On dismiss we don't snap shut; we keep re-rendering the frozen
            // frame + the shrinking menu until the close pop reports done.
            if (menu_closing) {
                // Presses made now reach the game once it resumes.
                close_drain = (close_drain | kDown) & ~kUp;
                for (int p = 0; p < EXTRA_PLAYERS; ++p) {
                    close_drain_n[p] = (close_drain_n[p] | kDownP[p]) & ~kUpP[p];
                }
                ruffle_redraw_paused_frozen();
                if (ruffle_draw_menu_closing(menu_selection)) {
                    menu_open = false;
                    menu_closing = false;
                    // Re-measure wall clock so the resumed frame doesn't catch
                    // up by replaying the paused + closing interval, and hide
                    // that interval from getTimer() as well (#87).
                    resume_after_pause();
                }
                gl_context_swap();
                continue;
            }

            // The pause's lists move on one axis at a time (input rule 7):
            // the filtered bits drive every page below except the framing
            // mode, which wants both axes of both sticks.
            u64 kDownN = kDown;
            u64 kHeldN = kHeld;
            nav_filter(pause_latch, &pad, &kHeldN, &kDownN);

            // ─── Sub-screen branch: TOUCHES (Rust-driven) ───────────────
            // If the user picked "TOUCHES" earlier, Rust now owns the
            // input + rendering for the keymap editor. We just forward
            // joycon down-edges and ask it to draw.
            if (ruffle_touches_active()) {
                // `-` opened the pause, so `-` resumes the game from any of its
                // pages (keeping a value armed there, which the close does);
                // B is the one that goes back a level.
                if (kDown & HidNpadButton_Minus) {
                    ruffle_touches_close();
                    if (ruffle_touches_consume_dirty()) {
                        populate_bindings_from_keymap();
                        cursor_speed_for_game();
                    }
                    ruffle_ui_sound(UI_SFX_CLOSE);
                    ruffle_menu_close_begin();
                    menu_closing = true;
                    continue;
                }
                menu_repeat_step(touches_repeat, kDownN, kUp, kHeldN,
                                 ruffle_tick_now(), tick_freq_global,
                                 ruffle_touches_input);
                // Touchscreen, same contract as the launcher: tap a row to move
                // the cursor, tap it again to take it. The panels here are drawn
                // by the same renderers as the launcher's and publish the same
                // tables; they simply had nothing feeding them, so the on-screen
                // keyboard could only be reached with a stick.
                hidGetTouchScreenStates(&touch_state, 1);
                // Not a finger that was already down (`touch_wait_lift`).
                if (touch_state.count == 0) touch_wait_lift = false;
                const bool menu_touch = touch_state.count > 0 && !touch_wait_lift;
                ruffle_menu_touch(
                    menu_touch ? (float)touch_state.touches[0].x * UI_TOUCH_SCALE_X : 0.0f,
                    menu_touch ? (float)touch_state.touches[0].y * UI_TOUCH_SCALE_Y : 0.0f,
                    menu_touch ? 1 : 0);
                // If a binding was committed, refresh our runtime BINDINGS
                // so the change applies immediately (no need to REDEMARRER),
                // and the cursor speed, which a profile can carry.
                if (ruffle_touches_consume_dirty()) {
                    populate_bindings_from_keymap();
                    cursor_speed_for_game();
                }
                // A profile request the page asked for runs here, before the
                // frame and outside every lock, drawing frames while it waits.
                if (ruffle_touches_active()) {
                    ruffle_touches_drive();
                    if (ruffle_touches_consume_dirty()) {
                        populate_bindings_from_keymap();
                        cursor_speed_for_game();
                    }
                }
                ruffle_redraw_paused_frozen();
                // The input above may have CLOSED the sub-menu (B). If so, drawing
                // ruffle_touches_draw() now would draw nothing over the game — the
                // dim vanishes for one frame and the bright game flashes through.
                // Draw the pause menu instead so a dim is always present.
                if (ruffle_touches_active()) {
                    ruffle_touches_draw();
                } else {
                    // Back on the main panel: a direction still held from the
                    // page must not start repeating there.
                    pause_repeat_reset(pause_rep);
                    ruffle_draw_menu(menu_selection);
                }
                gl_context_swap();
                continue;
            }

            // ─── Sub-screen branch: ECRAN, ZOOM framing mode ────────────
            // The panel is gone and the picture is untouched: what the player
            // is judging here IS the framing, so nothing is laid over it but
            // two letterbox bands carrying the legend. Everything is live and
            // nothing is written until `A` (or `-`, which resumes the game).
            if (screen_menu && zoom_mode) {
                // COARSE zoom on the shoulders, FINE zoom on X / Y -- the same
                // split as the sticks and the d-pad below, so each pair of
                // controls means the same thing on both axes of the mode.
                const u64 ZOOM_IN  = HidNpadButton_ZR | HidNpadButton_R;
                const u64 ZOOM_OUT = HidNpadButton_ZL | HidNpadButton_L;
                int dz = 0;
                if (kHeld & (ZOOM_IN | ZOOM_OUT)) {
                    // Both directions down at once is the reversal the `else`
                    // below would have caught had the player let go first.
                    // Restart the ramp, so the surviving direction eases out at
                    // one percent instead of inheriting five.
                    if ((kHeld & ZOOM_IN) && (kHeld & ZOOM_OUT)) zoom_hold = 0;
                    zoom_hold++;
                    const int step = zoom_hold > 90 ? 5 : (zoom_hold > 30 ? 3 : 1);
                    if (kHeld & ZOOM_IN)  dz += step;
                    if (kHeld & ZOOM_OUT) dz -= step;
                } else {
                    zoom_hold = 0;
                }
                // X / Y: one percent a tap, nothing for the first 0.4 s of a
                // hold, then a ramp. Landing on a chosen value is otherwise
                // impossible -- a shoulder tap already lasts several frames.
                const u64 FINE_MASK = HidNpadButton_X | HidNpadButton_Y;
                if (kDown & HidNpadButton_X) dz += 1;
                if (kDown & HidNpadButton_Y) dz -= 1;
                if (kHeld & FINE_MASK) {
                    if ((kHeld & HidNpadButton_X) && (kHeld & HidNpadButton_Y)) zoom_fine_hold = 0;
                    zoom_fine_hold++;
                    if (zoom_fine_hold > 25) {
                        const int step = zoom_fine_hold > 90 ? 3 : (zoom_fine_hold > 55 ? 2 : 1);
                        if (kHeld & HidNpadButton_X) dz += step;
                        if (kHeld & HidNpadButton_Y) dz -= step;
                    }
                } else {
                    zoom_fine_hold = 0;
                }
                // EITHER stick frames it, camera-style: pushing right shows what
                // is on the right, so the picture slides the other way. Both
                // sticks because there is nothing else for them to do in this
                // mode, and a player who reaches for the wrong one should not
                // find it dead.
                int dx = 0, dy = 0;
                static constexpr float PAN_SPEED = 14.0f;
                for (int si = 0; si < 2; si++) {
                    const HidAnalogStickState zs = padGetStickPos(&pad, si);
                    const float zx = (float)zs.x;
                    const float zy = (float)zs.y;
                    if (zx > STICK_DEADZONE || zx < -STICK_DEADZONE) {
                        dx += -(int)((zx / STICK_MAX) * PAN_SPEED);
                    }
                    // Stick Y is positive-up, screen Y positive-down.
                    if (zy > STICK_DEADZONE || zy < -STICK_DEADZONE) {
                        dy += (int)((zy / STICK_MAX) * PAN_SPEED);
                    }
                }
                // The d-pad is the FINE control, the one that makes "one pixel
                // to the left" possible at all: a tap is exactly one pixel, and
                // only a long hold ramps up so a long trip is still bearable. A
                // stick cannot do this -- its smallest useful deflection is
                // already several pixels a frame.
                const u64 PAD_MASK = HidNpadButton_Left | HidNpadButton_Right
                                   | HidNpadButton_Up   | HidNpadButton_Down;
                if (kDown & HidNpadButton_Left)  dx += 1;
                if (kDown & HidNpadButton_Right) dx -= 1;
                if (kDown & HidNpadButton_Up)    dy += 1;
                if (kDown & HidNpadButton_Down)  dy -= 1;
                if (kHeld & PAD_MASK) {
                    pan_hold++;
                    // Nothing for the first ~0.4 s, so a tap stays a tap.
                    if (pan_hold > 25) {
                        const int step = pan_hold > 90 ? 6 : (pan_hold > 55 ? 3 : 1);
                        if (kHeld & HidNpadButton_Left)  dx += step;
                        if (kHeld & HidNpadButton_Right) dx -= step;
                        if (kHeld & HidNpadButton_Up)    dy += step;
                        if (kHeld & HidNpadButton_Down)  dy -= step;
                    }
                } else {
                    pan_hold = 0;
                }
                // The touchscreen: one finger frames, two fingers zoom.
                //
                // Note the SIGN of the drag: a finger pulls the PICTURE, which
                // follows it, where a stick moves the CAMERA and the picture
                // goes the other way. Those are the conventions of the two
                // devices and matching each one is what makes both feel right.
                //
                // The pinch is PROPORTIONAL (see ruffle_zoom_percent): spreading
                // two fingers by a tenth adds a tenth, at 100 % as at 500 %.
                hidGetTouchScreenStates(&touch_state, 1);
                const int tcount = (int)touch_state.count;
                if (tcount >= 2) {
                    const float ax = (float)touch_state.touches[0].x * GAME_TOUCH_SCALE_X;
                    const float ay = (float)touch_state.touches[0].y * GAME_TOUCH_SCALE_Y;
                    const float bx = (float)touch_state.touches[1].x * GAME_TOUCH_SCALE_X;
                    const float by = (float)touch_state.touches[1].y * GAME_TOUCH_SCALE_Y;
                    const float d  = std::sqrt((ax - bx) * (ax - bx) + (ay - by) * (ay - by));
                    const int   mx = (int)((ax + bx) * 0.5f);
                    const int   my = (int)((ay + by) * 0.5f);
                    if (zoom_pinching && zoom_pinch_dist > 1.0f) {
                        const float ratio = d / zoom_pinch_dist - 1.0f;
                        dz += (int)std::lround((double)ruffle_zoom_percent() * (double)ratio);
                        // The midpoint frames at the same time, so a pinch that
                        // drifts across the screen takes the picture with it.
                        dx += mx - zoom_touch_x;
                        dy += my - zoom_touch_y;
                    }
                    zoom_pinch_dist = d;
                    zoom_touch_x = mx;
                    zoom_touch_y = my;
                    zoom_pinching = true;
                    zoom_touching = true;
                } else if (tcount == 1) {
                    const int tx = (int)((float)touch_state.touches[0].x * GAME_TOUCH_SCALE_X);
                    const int ty = (int)((float)touch_state.touches[0].y * GAME_TOUCH_SCALE_Y);
                    // Lifting one finger out of a pinch: re-anchor, or the
                    // framing would jump from the midpoint to the survivor.
                    if (zoom_pinching) {
                        zoom_pinching = false;
                        zoom_touching = false;
                    }
                    if (zoom_touching) {
                        dx += tx - zoom_touch_x;
                        dy += ty - zoom_touch_y;
                    }
                    zoom_touch_x = tx;
                    zoom_touch_y = ty;
                    zoom_touching = true;
                } else {
                    // Fingers up: the next touch starts a fresh gesture rather
                    // than jumping the framing by the gap between them.
                    zoom_touching = false;
                    zoom_pinching = false;
                }
                if (dz || dx || dy) ruffle_zoom_adjust(dz, dx, dy);
                // Landing exactly back on an untouched picture is otherwise
                // impossible once you have nudged by one percent at a time.
                if (kDown & HidNpadButton_StickL) ruffle_zoom_reset();
                // Leaving the mode changes what sits under the crosshair, and
                // the cursor block that would recompute it is far below, past
                // the `continue` this branch takes every frame. So re-send the
                // pointer here: without it the FIRST click after a zoom lands
                // where the old magnification put it -- a third of a screen away
                // at 300% -- and only heals when the stick is next nudged.
                //
                // Back on the ÉCRAN rows, a direction still held from the
                // framing (the sticks and the d-pad move the picture here) must
                // not start repeating: their timers did not run in this mode
                // and still hold a press from before it, so the cursor ran
                // down the list on its own.
                if (kDown & HidNpadButton_A) {
                    ruffle_ui_sound(UI_SFX_DECIDE);
                    ruffle_zoom_commit();
                    ruffle_handle_mouse_move((int)cursor_x, (int)cursor_y);
                    zoom_mode = false;
                    zoom_hold = 0;
                    pause_repeat_reset(pause_rep);
                    touch_wait_lift = tcount > 0;
                } else if (kDown & HidNpadButton_B) {
                    ruffle_ui_sound(UI_SFX_BACK);
                    ruffle_zoom_cancel();
                    ruffle_handle_mouse_move((int)cursor_x, (int)cursor_y);
                    zoom_mode = false;
                    zoom_hold = 0;
                    pause_repeat_reset(pause_rep);
                    touch_wait_lift = tcount > 0;
                } else if (kDown & HidNpadButton_Minus) {
                    // `-` resumes the game from here as from every page of the
                    // pause, and keeps what is being set on the way, as the
                    // close button does everywhere (input rule 2): the framing,
                    // as A would. It used to throw the framing away and stay on
                    // ÉCRAN, a second `-` away from the game.
                    ruffle_zoom_commit();
                    ruffle_handle_mouse_move((int)cursor_x, (int)cursor_y);
                    zoom_mode = false;
                    zoom_hold = 0;
                    screen_menu = false;
                    pause_repeat_reset(pause_rep);
                    ruffle_ui_sound(UI_SFX_CLOSE);
                    ruffle_menu_close_begin();
                    menu_closing = true;
                    continue;
                }
                ruffle_redraw_paused();
                if (zoom_mode) ruffle_draw_zoom_overlay();
                else           ruffle_draw_screen_menu(screen_selection);
                gl_context_swap();
                continue;
            }

            // ─── ECRAN rows ─────────────────────────────────────────────
            // Each value row is ARMED by A, changed by Left / Right while armed
            // and kept by A, or put back as it was by B (input rules 1 and 2).
            // The value is applied live, and every pass of this branch
            // re-renders the frozen frame behind the panel, so you see what it
            // does to THIS game before keeping it. ZOOM opens the framing mode
            // above; the counter is a switch A flips. B, unarmed, returns to
            // the pause, which is why nothing here calls the close pop: the
            // panel is backed out of, not dismissed.
            if (screen_menu) {
                bool nav_rep = false;
                const u64 kNav = pause_repeat_step(pause_rep, kDownN, kHeldN, ruffle_tick_now(), tick_freq_global, &nav_rep);
                // A turn of the picture or a new zoom moves what sits under the
                // crosshair: after any change here the pointer is sent again,
                // or the next click lands where the old picture put it.
                bool changed = false;
                bool back = false;
                // Up / Down keep an armed value and move on: an armed row is
                // no trap (input rule 2).
                if (kNav & PAUSE_DIRS[0]) {
                    if (screen_selection > 0 || !nav_rep) {
                        if (ruffle_value_armed()) ruffle_value_confirm();
                        screen_selection = (screen_selection + SCREEN_COUNT - 1) % SCREEN_COUNT;
                        ruffle_ui_sound(UI_SFX_MOVE);
                    }
                }
                if (kNav & PAUSE_DIRS[1]) {
                    if (screen_selection + 1 < SCREEN_COUNT || !nav_rep) {
                        if (ruffle_value_armed()) ruffle_value_confirm();
                        screen_selection = (screen_selection + 1) % SCREEN_COUNT;
                        ruffle_ui_sound(UI_SFX_MOVE);
                    }
                }
                if (ruffle_value_armed()) {
                    // Left / Right change the armed value, a held direction
                    // repeating; against an end nothing moves and nothing
                    // sounds.
                    int dir = 0;
                    if (kNav & PAUSE_DIRS[2]) dir = -1;
                    if (kNav & PAUSE_DIRS[3]) dir = 1;
                    if (dir != 0) {
                        const int r = ruffle_value_step(dir);
                        if (r > 0) {
                            ruffle_ui_sound(UI_SFX_TOGGLE);
                            changed = true;
                        } else if (r < 0) {
                            ruffle_ui_sound(UI_SFX_ERROR);
                        }
                    }
                } else if (kDownN & PAUSE_DIRS[2]) {
                    // Unarmed, Left points at the pause folded into its rail:
                    // a fresh press goes back to it, exactly like B (input
                    // rule 3). Right points nowhere and does nothing.
                    back = true;
                }
                if (kDownN & HidNpadButton_Minus) {
                    // `-` resumes the game from here too, keeping an armed
                    // value; B goes back to the main panel.
                    ruffle_value_confirm();
                    screen_menu = false;
                    ruffle_ui_sound(UI_SFX_CLOSE);
                    ruffle_menu_close_begin();
                    menu_closing = true;
                    continue;
                }
                if (kDownN & HidNpadButton_B) {
                    if (ruffle_value_armed()) {
                        // B puts back what was there before A, and stays.
                        ruffle_value_cancel();
                        ruffle_ui_sound(UI_SFX_BACK);
                        changed = true;
                    } else {
                        back = true;
                    }
                }
                // X: the game drops its own value for this row and follows
                // Reglages again, as X does in its options in the launcher.
                if ((kDownN & HidNpadButton_X) && !ruffle_value_armed()
                    && ruffle_value_reset(UI_KIND_SCREEN, screen_selection) > 0) {
                    ruffle_ui_sound(UI_SFX_TOGGLE);
                    changed = true;
                }
                // Touch (input rule 6): an option or the switch acts at once,
                // the rail or the "< Pause" caption goes back, a row moves the
                // cursor, and the row it is already on is A. A tap anywhere
                // but on the armed row keeps its value first (done by Rust).
                bool tap_a = false;
                {
                    hidGetTouchScreenStates(&touch_state, 1);
                    // Not a finger that was already down (`touch_wait_lift`).
                    if (touch_state.count == 0) touch_wait_lift = false;
                    const bool t = touch_state.count > 0 && !touch_wait_lift;
                    const int hit = ruffle_pause_touch(
                        t ? (float)touch_state.touches[0].x * UI_TOUCH_SCALE_X : 0.0f,
                        t ? (float)touch_state.touches[0].y * UI_TOUCH_SCALE_Y : 0.0f,
                        t ? 1 : 0, UI_KIND_SCREEN);
                    if (hit == PAUSE_TAP_BACK) {
                        back = true;
                    } else if (hit >= PAUSE_TAP_SET) {
                        const int row = hit & 0xFF;
                        const int what = hit & 0xF000;
                        const bool moved = row != screen_selection && row < SCREEN_COUNT;
                        if (row < SCREEN_COUNT) screen_selection = row;
                        if (what == PAUSE_TAP_SET) {
                            ruffle_ui_sound(UI_SFX_TOGGLE);
                            changed = true;
                        } else if (what == PAUSE_TAP_REFUSED) {
                            ruffle_ui_sound(UI_SFX_ERROR);
                        } else if (moved) {
                            ruffle_ui_sound(UI_SFX_MOVE);
                        }
                    } else if (hit >= 0 && hit < SCREEN_COUNT) {
                        if (hit == screen_selection) {
                            tap_a = true;
                        } else {
                            screen_selection = hit;
                            ruffle_ui_sound(UI_SFX_MOVE);
                        }
                    }
                }
                if (back) {
                    screen_menu = false;
                    pause_repeat_reset(pause_rep);
                    ruffle_ui_sound(UI_SFX_BACK);
                    if (changed) ruffle_handle_mouse_move((int)cursor_x, (int)cursor_y);
                    ruffle_redraw_paused();
                    ruffle_draw_menu(menu_selection);
                    gl_context_swap();
                    continue;
                }
                if ((kDownN & HidNpadButton_A) || tap_a) {
                    if (ruffle_value_armed()) {
                        // A keeps the value (written to the game's .prefs now).
                        ruffle_value_confirm();
                        ruffle_ui_sound(UI_SFX_DECIDE);
                    } else if (screen_selection == SCREEN_ZOOM) {
                        // Opens a mode instead of arming a value: a free
                        // percentage is framed, not stepped.
                        ruffle_ui_sound(UI_SFX_DECIDE);
                        ruffle_zoom_begin();
                        zoom_mode = true;
                        zoom_hold = 0;
                        pause_repeat_reset(pause_rep);
                        // The finger that tapped this row must not read as the
                        // start of a drag on the first frame of the mode.
                        zoom_touching = false;
                        zoom_pinching = false;
                        pan_hold = 0;
                        zoom_fine_hold = 0;
                    } else if (screen_selection == SCREEN_FPS) {
                        // A switch: A flips it, nothing to arm.
                        if (ruffle_value_toggle(UI_KIND_SCREEN, screen_selection) > 0) {
                            ruffle_ui_sound(UI_SFX_TOGGLE);
                        }
                    } else if (ruffle_value_arm(UI_KIND_SCREEN, screen_selection)) {
                        ruffle_ui_sound(UI_SFX_DECIDE);
                    }
                }
                if (changed) ruffle_handle_mouse_move((int)cursor_x, (int)cursor_y);
                ruffle_redraw_paused();
                ruffle_draw_screen_menu(screen_selection);
                gl_context_swap();
                continue;
            }

            // ─── Pause main menu ────────────────────────────────────────
            // Up / Down repeat when held, like every list. `-` or B dismisses
            // (= Resume). Right stick navigates too (it's the mouse cursor only
            // when the menu is closed: this branch `continue`s before the
            // cursor code).
            //
            // A fresh Up on Resume wraps to Quit, the quickest way out, as
            // Jonathan asked; under a HELD direction the list stops at its
            // ends instead, so holding Down settles on Quit rather than
            // spinning round to Resume.
            {
                bool nav_rep = false;
                const u64 kNav = pause_repeat_step(pause_rep, kDownN, kHeldN, ruffle_tick_now(), tick_freq_global, &nav_rep);
                if (kNav & PAUSE_DIRS[0]) {
                    if (menu_selection > 0 || !nav_rep) {
                        if (ruffle_value_armed()) ruffle_value_confirm();
                        menu_selection = (menu_selection + MENU_COUNT - 1) % MENU_COUNT;
                        ruffle_ui_sound(UI_SFX_MOVE);
                    }
                }
                if (kNav & PAUSE_DIRS[1]) {
                    if (menu_selection + 1 < MENU_COUNT || !nav_rep) {
                        if (ruffle_value_armed()) ruffle_value_confirm();
                        menu_selection = (menu_selection + 1) % MENU_COUNT;
                        ruffle_ui_sound(UI_SFX_MOVE);
                    }
                }
                // OVERCLOCK, armed: Left / Right ask for the setting beside it,
                // applied at once. A raise the console refuses keeps what was
                // in force and says so.
                if (ruffle_value_armed()) {
                    int dir = 0;
                    if (kNav & PAUSE_DIRS[2]) dir = -1;
                    if (kNav & PAUSE_DIRS[3]) dir = 1;
                    if (dir != 0) {
                        const int r = ruffle_value_step(dir);
                        if (r > 0) ruffle_ui_sound(UI_SFX_TOGGLE);
                        else if (r < 0) ruffle_ui_sound(UI_SFX_ERROR);
                    }
                }
            }
            // Touch, as on ECRAN: the overclock's options act at once, a row
            // moves the cursor, the row it is already on is A.
            bool tap_a = false;
            {
                hidGetTouchScreenStates(&touch_state, 1);
                // Not a finger that was already down (`touch_wait_lift`).
                if (touch_state.count == 0) touch_wait_lift = false;
                const bool t = touch_state.count > 0 && !touch_wait_lift;
                const int hit = ruffle_pause_touch(
                    t ? (float)touch_state.touches[0].x * UI_TOUCH_SCALE_X : 0.0f,
                    t ? (float)touch_state.touches[0].y * UI_TOUCH_SCALE_Y : 0.0f,
                    t ? 1 : 0, UI_KIND_PAUSE);
                if (hit >= PAUSE_TAP_SET) {
                    const int row = hit & 0xFF;
                    const int what = hit & 0xF000;
                    const bool moved = row != menu_selection && row < MENU_COUNT;
                    if (row < MENU_COUNT) menu_selection = row;
                    if (what == PAUSE_TAP_SET) ruffle_ui_sound(UI_SFX_TOGGLE);
                    else if (what == PAUSE_TAP_REFUSED) ruffle_ui_sound(UI_SFX_ERROR);
                    else if (moved) ruffle_ui_sound(UI_SFX_MOVE);
                } else if (hit >= 0 && hit < MENU_COUNT) {
                    if (hit == menu_selection) {
                        tap_a = true;
                    } else {
                        menu_selection = hit;
                        ruffle_ui_sound(UI_SFX_MOVE);
                    }
                }
            }
            if ((kDownN & HidNpadButton_B) && ruffle_value_armed()) {
                // B on the armed OVERCLOCK puts back the setting it had, and
                // the pause stays.
                ruffle_value_cancel();
                ruffle_ui_sound(UI_SFX_BACK);
            } else if (kDownN & (HidNpadButton_Minus | HidNpadButton_B)) {
                // Scale the menu out (drained at the top of this branch over the
                // next frames), then resume. `-` keeps an armed value.
                // "dismissed", not "cancelled": B and `-` resume the game, they
                // do not undo whatever the player kept on the rows.
                ruffle_value_confirm();
                std::printf("menu: pause CLOSE (row=%d, dismissed)\n", menu_selection);
                std::fflush(stdout);
                ruffle_ui_sound(UI_SFX_CLOSE);
                ruffle_menu_close_begin();
                menu_closing = true;
                continue;
            }
            // X: the overclock follows Reglages again, as in the game's options.
            if ((kDownN & HidNpadButton_X) && !ruffle_value_armed()
                && ruffle_value_reset(UI_KIND_PAUSE, menu_selection) > 0) {
                ruffle_ui_sound(UI_SFX_TOGGLE);
            }
            if ((kDownN & HidNpadButton_A) || tap_a) {
                if (ruffle_value_armed()) {
                    // A keeps the overclock setting (written to the game's
                    // .prefs now).
                    ruffle_value_confirm();
                    ruffle_ui_sound(UI_SFX_DECIDE);
                } else {
                    // Resume closes the sheet, OVERCLOCK arms, the rest open a
                    // page or act.
                    ruffle_ui_sound(menu_selection == MENU_RESUME ? UI_SFX_CLOSE : UI_SFX_DECIDE);
                    switch (menu_selection) {
                    case MENU_RESUME:
                        std::printf("menu: pause CLOSE (REPRENDRE)\n");
                        std::fflush(stdout);
                        ruffle_menu_close_begin();
                        menu_closing = true;
                        continue;
                    case MENU_TOUCHES:
                        // Hand control to the Rust TOUCHES sub-screen.
                        // menu_open stays true so we re-enter the branch above
                        // next frame; Rust closes itself on B.
                        menu_repeat_reset(touches_repeat);
                        ruffle_touches_open();
                        continue;
                    case MENU_SCREEN:
                        // Open the sub-panel on its first row and let the branch
                        // above own the next frame.
                        screen_menu = true;
                        screen_selection = SCREEN_DISPLAY;
                        zoom_mode = false;
                        pause_repeat_reset(pause_rep);
                        continue;
                    case MENU_POWER:
                        // Armed in place: the panel stays open and the row
                        // redraws outlined, taking Left / Right.
                        ruffle_value_arm(UI_KIND_PAUSE, MENU_POWER);
                        break;
                    case MENU_RESTART: {
                        std::printf("menu: REDEMARRER → ruffle_restart()\n");
                        std::fflush(stdout);
                        if (ruffle_restart() != 0) {
                            std::printf("menu: ruffle_restart() failed\n");
                            std::fflush(stdout);
                            back_to_library = true;
                            break;
                        }
                        menu_open = false;
                        recenter_cursor();
                        // NOT resume_after_pause() here, unlike every other resume:
                        // ruffle_restart() destroyed the Player and built a new one,
                        // whose clock starts NOW. There is no unplayed gap to hide,
                        // and handing it one pushes its start_time into the FUTURE
                        // by however long the menu was open -- getTimer() then reads
                        // 0 for exactly that long (Instant::duration_since saturates
                        // rather than panicking), which is the frozen-animation
                        // failure mode of #100 reachable from any game's menu.
                        last_tick = ruffle_tick_now();
                        continue;
                    }
                    case MENU_QUIT:
                        back_to_library = true;
                        break;
                    }
                    if (back_to_library) break;
                }
            }

            // Put the frozen game frame back + cursor, then layer the menu
            // on top, then swap. Skipping ruffle_render_frame_dt freezes AVM
            // so the game doesn't advance.
            ruffle_redraw_paused_frozen();
            ruffle_draw_menu(menu_selection);
            gl_context_swap();
            continue;
        }

        // The presses made while the pause closed, still held on this first
        // frame back (`close_drain`), go to the game with this frame's own;
        // the `-` that may have closed it is never one of them.
        const u64 kDownG = kDown | (close_drain & kHeld & ~HidNpadButton_Minus);
        u64 kDownGP[EXTRA_PLAYERS];
        for (int p = 0; p < EXTRA_PLAYERS; ++p) {
            kDownGP[p] = kDownP[p] | (close_drain_n[p] & kHeldP[p] & ~HidNpadButton_Minus);
            close_drain_n[p] = 0;
        }
        close_drain = 0;

        // The pause opened inside the last frame, while it computed, was left
        // with Quit or Restart (`script_hook`): its script is already stopped.
        const int frozen_req = ruffle_take_frozen_request();
        if (frozen_req == 1) {
            std::printf("menu: QUITTER from the pause opened inside a frame\n");
            std::fflush(stdout);
            back_to_library = true;
            break;
        }
        if (frozen_req == 2) {
            std::printf("menu: REDEMARRER from the pause opened inside a frame → ruffle_restart()\n");
            std::fflush(stdout);
            if (ruffle_restart() != 0) {
                std::printf("menu: ruffle_restart() failed\n");
                std::fflush(stdout);
                back_to_library = true;
                break;
            }
            // As the pause's own Restart: a new Player, whose clock starts now.
            recenter_cursor();
            last_tick = ruffle_tick_now();
            continue;
        }

        // `-` opens the pause modal. Captured before BINDINGS so it can't
        // also fire a key event into Ruffle. So does a `-` the watch thread
        // saw between two frames (`minus_watch_entry`), which this frame's
        // read missed. Taken every frame, so one press opens one pause.
        const bool minus_between = s_minus_pressed.exchange(false, std::memory_order_relaxed);
        if ((kDown & HidNpadButton_Minus) || minus_between) {
            // Diagnostic: print the movie's own AVM1 variables at the exact
            // moment the player says "this is stuck". A pause is the one
            // instant the game state is worth reading, and `-` is the only
            // input we know reaches us rather than the game.
            //
            // Gated on a marker file because the dump runs to hundreds of
            // lines on a large movie, which nobody who is merely pausing
            // wants — but on issue #84 it was the whole answer: three
            // enemies the clear condition waits on, two of which no longer
            // existed and one of which was alive with full HP and invisible.
            if (dump_vars_on_pause()) {
                ruffle_dump_root_vars();
            }
            menu_open = true;
            menu_selection = MENU_RESUME;
            pause_repeat_reset(pause_rep);
            // The stick that was steering the game must not carry an axis
            // latched from before into the menu.
            nav_latch_reset(pause_latch);
            // Nor a finger left on the game: lifted once the panel is in, it
            // tapped the row under it (`touch_wait_lift`).
            hidGetTouchScreenStates(&touch_state, 1);
            touch_wait_lift = touch_state.count > 0;
            ruffle_pause_opened();
            ruffle_ui_sound(UI_SFX_OPEN);
            // A fresh pause always opens on the main panel, never back inside
            // ECRAN where a previous pause happened to leave it.
            screen_menu = false;
            // Release any held in-game keys so the player doesn't keep
            // running / jumping while paused.
            for (const auto& b : BINDINGS) {
                if (kHeld & b.mask) ruffle_handle_key(b.key, false);
            }
            // Same for the other players' held keys (issues #40, #128).
            for (int p = 0; p < EXTRA_PLAYERS; ++p) {
                for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
                    if ((kHeldP[p] & BINDINGS[i].mask) && BINDINGS_PN_KEYS[p][i] != SK_NONE) {
                        ruffle_handle_key(BINDINGS_PN_KEYS[p][i], false);
                    }
                }
            }
            // And player 1's stick on a lone Joy-Con (issue #128): the click A
            // holds in cursor mode, a short push's key still held for the
            // game, a push under way. Their releases came after the pause,
            // the click's only with the next A, which then did nothing but let
            // go of it.
            if (stick_cursor_click) {
                ruffle_handle_mouse_button(false);
                stick_cursor_click = false;
            }
            if (press_tap_frames > 0) {
                p1_key(press_tap_key, false);
                press_tap_frames = 0;
                press_tap_key = SK_NONE;
            }
            press_since = 0;
            // Leave a trace. Until this line the pause modal was INVISIBLE in a
            // log: unless the player happened to change the power mode, nothing
            // said the game had even been paused. That turned "did the modal
            // break my game?" into an argument from absence, which is the same
            // shape of blind spot `drift=` was added to close.
            std::printf("menu: pause OPEN (frame paused, by %s)\n",
                        (kDown & HidNpadButton_Minus) ? "this frame's -" : "the - watch");
            std::fflush(stdout);
            continue;
        }

        // Player 1 on a lone Joy-Con (issue #128) has one stick. Pushing it
        // in switches between the game and the cursor, where A clicks, at
        // once while its row ("appui" of the left stick in the editor) has no
        // key. With a key there (Jonathan: the hold only when the player set
        // one), a short push is that key, sent as it is let go and held two
        // frames so a game reading its keys once a frame sees it, and a push
        // held half a second switches. In cursor mode the stick's directions
        // and A are not the game's. Going to the cursor lets go of the keys
        // they held; leaving it (or the lone Joy-Con) lets go of a click A
        // held. The state is the game's, set before the loop.
        if (press_tap_frames > 0 && --press_tap_frames == 0) {
            p1_key(press_tap_key, false);
            press_tap_key = SK_NONE;
        }
        {
            bool now = stick_cursor;
            const u64 tick = armGetSystemTick();
            if (!p1_single) {
                now = false;
                press_since = 0;
            } else {
                int press_key = SK_NONE;
                for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
                    if (BINDINGS[i].mask == HidNpadButton_StickL) press_key = BINDINGS[i].key;
                }
                if (kDownG & HidNpadButton_StickL) {
                    press_since = tick;
                    // Nothing on the push: it switches now, and its release
                    // has nothing to send.
                    press_long = press_key == SK_NONE;
                    if (press_long) now = !now;
                }
                if (press_since != 0 && (kHeld & HidNpadButton_StickL) && !press_long &&
                    tick - press_since >= armGetSystemTickFreq() / 2) {
                    press_long = true;
                    now = !now;
                }
                if (press_since != 0 && (kUp & HidNpadButton_StickL)) {
                    if (!press_long && press_tap_frames == 0 && press_key != SK_NONE) {
                        p1_key(press_key, true);
                        std::printf("stick press: its key sent to the game (key %d)\n", press_key);
                        std::fflush(stdout);
                        press_tap_key = press_key;
                        press_tap_frames = 2;
                    }
                    press_since = 0;
                }
            }
            if (now != stick_cursor) {
                if (now) {
                    for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
                        if ((BINDINGS[i].mask & (STICK_DIRS | HidNpadButton_A)) && g_p1_down_key[i] != SK_NONE) {
                            const int key = g_p1_down_key[i];
                            g_p1_down_key[i] = SK_NONE;
                            if (key == SK_MOUSE_LEFT)       ruffle_handle_mouse_button(false);
                            else if (key == SK_MOUSE_RIGHT) ruffle_handle_mouse_right(false);
                            else if (key != SK_KEYBOARD && key != SK_NONE) ruffle_handle_key(key, false);
                        }
                    }
                } else if (stick_cursor_click) {
                    ruffle_handle_mouse_button(false);
                    stick_cursor_click = false;
                }
                stick_cursor = now;
                ruffle_set_stick_cursor(now ? 1 : 0);
                std::printf("stick cursor: %s\n", now ? "on" : "off");
                std::fflush(stdout);
            }
        }
        const u64 p1_withheld = p1_single
            ? (HidNpadButton_StickL | (stick_cursor ? (STICK_DIRS | HidNpadButton_A) : 0))
            : 0;
        if (stick_cursor) {
            if ((kDownG & HidNpadButton_A) && !stick_cursor_click) {
                ruffle_handle_mouse_button(true);
                stick_cursor_click = true;
            } else if ((kUp & HidNpadButton_A) && stick_cursor_click) {
                ruffle_handle_mouse_button(false);
                stick_cursor_click = false;
            }
        }
        const u64 kDownG1 = kDownG & ~p1_withheld;
        const u64 kUp1 = kUp & ~p1_withheld;

        // Keyboard-style buttons via edge detection. A button bound to a mouse
        // pseudo-code clicks at the cursor instead of sending a key event.
        //
        // Combo layers (issue #57, per-modifier): each ACTIVE modifier (ZL/ZR/L/R
        // with a layer) is a dedicated modifier -> its own key is muted. The
        // currently-HELD active modifier (priority ZL>ZR>L>R) makes every other
        // button send THAT layer's key, falling through to the base key when the
        // layer doesn't map it. The key is chosen at PRESS time (g_p1_down_key) so
        // the key-up matches even if the modifier is released first.
        u64 p1_mod_buttons = 0; // all active-modifier buttons (always muted)
        int p1_active_mod = -1; // held active modifier whose layer applies
        for (int m = 0; m < CM_COUNT; ++m) {
            if (!g_combo_active[m]) continue;
            p1_mod_buttons |= COMBO_MOD_MASKS[m];
            if (p1_active_mod < 0 && (kHeld & COMBO_MOD_MASKS[m])) p1_active_mod = m;
        }
        auto emit_p1 = [](int key, bool down) {
            if (key == SK_MOUSE_LEFT)       ruffle_handle_mouse_button(down);
            else if (key == SK_MOUSE_RIGHT) ruffle_handle_mouse_right(down);
            // On the press only: the keyboard is a modal, and a release arriving
            // after it closed would queue a second one.
            else if (key == SK_KEYBOARD)  { if (down) ruffle_keyboard_request_manual(); }
            else if (key != SK_NONE)        ruffle_handle_key(key, down);
        };
        for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
            const ButtonBinding& b = BINDINGS[i];
            if (b.mask & p1_mod_buttons) continue; // this button is a modifier, muted
            const bool dn = (kDownG1 & b.mask) != 0;
            const bool up = (kUp1    & b.mask) != 0;
            if (!dn && !up) continue;
            if (dn) {
                int key = b.key;
                if (p1_active_mod >= 0 && BINDINGS_COMBO_KEYS[p1_active_mod][i] != SK_NONE) {
                    key = BINDINGS_COMBO_KEYS[p1_active_mod][i];
                }
                g_p1_down_key[i] = key;
                emit_p1(key, true);
            }
            if (up) {
                int key = g_p1_down_key[i];
                if (key == SK_NONE) key = b.key; // safety net (never-tracked press)
                g_p1_down_key[i] = SK_NONE;
                emit_p1(key, false);
            }
        }

        // Players 2 to 4 (issues #40, #128): controllers 2 to 4 drive the same
        // key pipeline through their own keymaps. No cursor / mouse / pause for
        // them. Per-modifier combo layers (#57) mirror P1, with each player's own
        // layers. Mouse keys are ignored for them (one shared cursor).
        auto emit_pn = [](int key, bool down) {
            // Mouse and keyboard pseudo-keys stay with player 1: one cursor, and
            // one modal that would suspend the game for every player.
            if (key != SK_NONE && key != SK_MOUSE_LEFT && key != SK_MOUSE_RIGHT
                && key != SK_KEYBOARD) {
                ruffle_handle_key(key, down);
            }
        };
        for (int p = 0; p < EXTRA_PLAYERS; ++p) {
            u64 pn_mod_buttons = 0;
            int pn_active_mod = -1;
            for (int m = 0; m < CM_COUNT; ++m) {
                if (!g_combo_active_pn[p][m]) continue;
                pn_mod_buttons |= COMBO_MOD_MASKS[m];
                if (pn_active_mod < 0 && (kHeldP[p] & COMBO_MOD_MASKS[m])) pn_active_mod = m;
            }
            for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
                const u64 mask = BINDINGS[i].mask;
                if (mask & pn_mod_buttons) continue; // modifier button, muted
                const bool dn = (kDownGP[p] & mask) != 0;
                const bool up = (kUpP[p]    & mask) != 0;
                if (!dn && !up) continue;
                if (dn) {
                    int key = BINDINGS_PN_KEYS[p][i];
                    if (pn_active_mod >= 0 && BINDINGS_COMBO_PN_KEYS[p][pn_active_mod][i] != SK_NONE) {
                        key = BINDINGS_COMBO_PN_KEYS[p][pn_active_mod][i];
                    }
                    g_pn_down_key[p][i] = key;
                    emit_pn(key, true);
                }
                if (up) {
                    int key = g_pn_down_key[p][i];
                    if (key == SK_NONE) key = BINDINGS_PN_KEYS[p][i];
                    g_pn_down_key[p][i] = SK_NONE;
                    emit_pn(key, false);
                }
            }
        }

        // Right analog stick → cursor movement — UNLESS the user remapped the
        // right stick to a d-pad (g_right_stick_dpad), in which case its StickR*
        // key bindings already fired via the BINDINGS loop above and we leave the
        // cursor where it is.
        bool moved = false;
        if (!g_right_stick_dpad) {
            const HidAnalogStickState rs = padGetStickPos(&pad, 1);
            const float rsx = (float)rs.x;
            const float rsy = (float)rs.y;
            if (rsx >  STICK_DEADZONE || rsx < -STICK_DEADZONE) {
                cursor_x += (rsx / STICK_MAX) * g_cursor_speed;
                moved = true;
            }
            if (rsy >  STICK_DEADZONE || rsy < -STICK_DEADZONE) {
                // Switch right stick Y is positive-up; screen Y is positive-down.
                cursor_y -= (rsy / STICK_MAX) * g_cursor_speed;
                moved = true;
            }
        }

        // Left analog stick → cursor too, when it isn't bound as a d-pad (same
        // rule as the right stick). By default the left stick is the arrow keys
        // (so this is skipped); clear its direction bindings in TOUCHES and it
        // drives the cursor. Both sticks feed the one shared cursor.
        if (!g_left_stick_dpad || stick_cursor) {
            const HidAnalogStickState ls = padGetStickPos(&pad, 0);
            const float lsx = (float)ls.x;
            const float lsy = (float)ls.y;
            if (lsx >  STICK_DEADZONE || lsx < -STICK_DEADZONE) {
                cursor_x += (lsx / STICK_MAX) * g_cursor_speed;
                moved = true;
            }
            if (lsy >  STICK_DEADZONE || lsy < -STICK_DEADZONE) {
                // Switch left stick Y is positive-up; screen Y is positive-down.
                cursor_y -= (lsy / STICK_MAX) * g_cursor_speed;
                moved = true;
            }
        }

        // Gyroscope (#119), in the game's mode, on top of the sticks.
        {
            const bool by_stick = moved;
            float gdx, gdy;
            const int gmode = gyro_cursor_delta(&gdx, &gdy);
            float tilt_now[2];
            if (gmode == GYRO_MARBLE && gyro_tilt(tilt_now)) {
                // Placed by hand (or just switched on): this tilt is level.
                if (!g_tilt_anchor_valid || by_stick) {
                    g_tilt_anchor_x = cursor_x;
                    g_tilt_anchor_y = cursor_y;
                    g_tilt0[0] = tilt_now[0];
                    g_tilt0[1] = tilt_now[1];
                    g_tilt_anchor_valid = true;
                } else {
                    float dv = tilt_now[1] - g_tilt0[1];
                    if (dv > 3.14159265f) dv -= 6.28318531f;
                    if (dv < -3.14159265f) dv += 6.28318531f;
                    // 30 degrees across the width, as POINTER.
                    const float px_per_rad = (float)GAME_VIEWPORT_W * 12.0f / 6.28318531f;
                    const float nx = g_tilt_anchor_x + (tilt_now[0] - g_tilt0[0]) * px_per_rad;
                    const float ny = g_tilt_anchor_y - dv * px_per_rad;
                    if (std::fabs(nx - cursor_x) > 0.25f || std::fabs(ny - cursor_y) > 0.25f) {
                        cursor_x = nx;
                        cursor_y = ny;
                        moved = true;
                    }
                }
            } else if (gmode == GYRO_POINTER) {
                // The aim starts where the cursor is, and again whenever a stick
                // (or, below, a touch) moved it: the gyroscope carries on from
                // wherever the player put the cursor.
                if (!g_aim_valid || by_stick) {
                    g_aim_x = cursor_x;
                    g_aim_y = cursor_y;
                    g_aim_valid = true;
                }
                if (gdx != 0.0f || gdy != 0.0f) {
                    g_aim_x += gdx;
                    g_aim_y += gdy;
                    const float mx = GAME_VIEWPORT_W * 0.5f, my = GAME_VIEWPORT_H * 0.5f;
                    if (g_aim_x < -mx) g_aim_x = -mx;
                    if (g_aim_y < -my) g_aim_y = -my;
                    if (g_aim_x > GAME_VIEWPORT_W - 1 + mx) g_aim_x = GAME_VIEWPORT_W - 1 + mx;
                    if (g_aim_y > GAME_VIEWPORT_H - 1 + my) g_aim_y = GAME_VIEWPORT_H - 1 + my;
                    cursor_x = g_aim_x;
                    cursor_y = g_aim_y;
                    moved = true;
                }
            }
        }

        // Touch input — overrides stick position when active. We translate
        // touch X/Y (in Switch screen pixels, 1280x720 docked or 1280x720
        // handheld) directly to our viewport.
        hidGetTouchScreenStates(&touch_state, 1);
        const bool touch_pressed = touch_state.count > 0;
        if (touch_pressed) {
            // Panel units -> the game's (lower) render space.
            cursor_x = (float)touch_state.touches[0].x * GAME_TOUCH_SCALE_X;
            cursor_y = (float)touch_state.touches[0].y * GAME_TOUCH_SCALE_Y;
            moved = true;
            g_aim_valid = false;  // the gyroscope carries on from the touch
            g_tilt_anchor_valid = false;
        }

        // Clamp cursor to the viewport.
        if (cursor_x < 0)              cursor_x = 0;
        if (cursor_y < 0)              cursor_y = 0;
        if (cursor_x > GAME_VIEWPORT_W - 1) cursor_x = GAME_VIEWPORT_W - 1;
        if (cursor_y > GAME_VIEWPORT_H - 1) cursor_y = GAME_VIEWPORT_H - 1;

        if (moved) {
            ruffle_handle_mouse_move((int)cursor_x, (int)cursor_y);
        }

        // Click: touch tap → left mouse button, delivered one SWF frame AFTER
        // the cursor lands (#87). ZR (and any other button) is fully
        // keymap-driven (ZR defaults to "Left click"), handled in the BINDINGS
        // loop above.
        //
        // A tap teleports the cursor and clicks in the same instant, which no
        // mouse ever does: on a desktop the pointer is already sitting there
        // frames before the button goes down. Games that aim from the position
        // they sampled during their own last frame — Zuma's frog, Mario 63's
        // cursor — therefore fired at where the cursor USED to be. Waiting for
        // the movie to run one frame with the new position costs ~40 ms on a
        // 24 fps game and hands it the same situation a mouse would.
        bool pressed_now = false;
        if (touch_press_armed && ruffle_tick_now() >= touch_press_at) {
            ruffle_handle_mouse_button(true);
            touch_press_armed = false;
            touch_press_held  = true;
            pressed_now = true;
        }
        if (touch_pressed && !touch_was_pressed) {
            // Arm for one SWF frame from now — "next host frame" would not do,
            // since at 24 fps most host frames run no SWF frame at all.
            const uint64_t wait = (ruffle_frame_interval_us() * tick_freq) / 1000000ULL;
            touch_press_at    = ruffle_tick_now() + wait;
            touch_press_armed = true;
        }
        // Release once the press has actually gone out, and never in the same
        // frame as the press — a tap can be shorter than the delay itself.
        if (!touch_pressed && touch_press_held && !pressed_now) {
            ruffle_handle_mouse_button(false);
            touch_press_held = false;
        }
        touch_was_pressed = touch_pressed;

        // In-game software keyboard. Ruffle's focus tracker raises a request
        // when an editable TextField gains focus (e.g. the user clicked it with
        // the cursor / touch). We suspend the game, run swkbd configured to
        // match the field (prefill, password/numeric/multiline, max length),
        // and feed the result back through normal text events. swkbdShow blocks,
        // so no frames advance while it's open.
        if (ruffle_keyboard_take_request()) {
            char kbd_prefill[4096];
            char kbd_out[4096];
            char kbd_header[256];
            int  kbd_flags = 0, kbd_max = 0;
            if (!ruffle_keyboard_header(kbd_header, sizeof(kbd_header))) kbd_header[0] = '\0';
            if (ruffle_keyboard_field(kbd_prefill, sizeof(kbd_prefill), &kbd_flags, &kbd_max)) {
                if (swkbd_prompt_game_field(kbd_header, kbd_prefill, kbd_flags, kbd_max,
                                            kbd_out, sizeof(kbd_out)) == 0) {
                    ruffle_keyboard_submit(kbd_out);
                }
            } else {
                // No field Ruffle calls focused. The automatic path stops here,
                // because there is nothing to write into — but a button bound to
                // the keyboard action was pressed on purpose, so open an empty
                // one and TYPE the result into the game instead of doing nothing.
                kbd_prefill[0] = '\0';
                if (swkbd_prompt_game_field(kbd_header, kbd_prefill, 0, 0,
                                            kbd_out, sizeof(kbd_out)) == 0) {
                    ruffle_keyboard_type_text(kbd_out);
                }
            }
            // The modal held the loop for a while; re-measure the clock so the
            // next frame doesn't replay the elapsed interval, and take that
            // interval out of getTimer() too (#87).
            resume_after_pause();
        }

        // Compute real elapsed since last iteration → microseconds.
        // tick_freq is ~19.2 MHz on Switch, so this is precise to ~50ns.
        const uint64_t now_tick = ruffle_tick_now();
        const uint64_t dt_ticks = now_tick - last_tick;
        last_tick = now_tick;
        g_prev_period_ticks = dt_ticks;
        // dt_us = dt_ticks * 1e6 / tick_freq, but avoid overflow on big stalls.
        uint64_t dt_us = (tick_freq > 0)
            ? ((dt_ticks * 1000000ULL) / tick_freq)
            : 16667ULL;
        // Cap to 100 ms so a one-off stall (texture upload, JIT warmup) doesn't
        // make Ruffle replay ~6 SWF frames at once. A tighter cap (tried 35 ms)
        // does NOT help heavy scenes — their cost is a single run_frame (~55 ms
        // of AVM1 + display-list work), not catch-up replay — and only adds
        // visible slow-motion, so keep the loose cap and let the game run at
        // real-time speed (dropping rendered frames) when the sim can't keep up.
        // Three seconds between two iterations is not a slow frame, it is the
        // app not running at all: HOME menu, sleep, an applet on top. Nobody
        // resumes us explicitly there, so hide the gap from getTimer() here,
        // exactly like a pause does (#87).
        //
        // The bar was half a second, and half a second is something a game does
        // on its own: Peggle spends 619 ms in one frame while it unpacks its
        // levels (#100). Taking that time off getTimer() rewinds the movie's
        // clock under an animation engine that has already recorded when it
        // started, and its fades never advance — every screen stayed hidden,
        // which looks exactly like a game that never starts. Nobody comes back
        // from the HOME menu in under three seconds, so the intent survives.
        if (dt_us > 3000000ULL) ruffle_skip_paused_time(dt_us - 100000ULL);
        if (dt_us > 100000ULL) dt_us = 100000ULL;

        prof_frame();
#ifdef FLASHNX_PROF_ALWAYS
        // The profiling build's in-session A/B: 240-frame windows alternate
        // what ruffle_ab_regime switches, and every sample carries its window,
        // so one session gives both the gain and where the time went.
        {
            const int regime = (int)((frames_since_start / 240) & 1);
            ruffle_ab_regime(regime);
            prof_set_regime(regime);
        }
#endif
        ruffle_render_frame_dt(dt_us);
        // One snapshot of the stage, ~8 s in: by then a game that is going to
        // show something has, and one that shows a black screen has settled into
        // whatever it is doing. Six lines, and they answer the first question a
        // blank screen raises — is anything on stage, and is it visible?
        if (++frames_since_start == 480) {
            ruffle_dump_stage_children();
        }
        const uint64_t swap_t0 = ruffle_tick_now();
        gl_context_swap();
        g_prev_swap_ticks = ruffle_tick_now() - swap_t0;

        if (back_to_library) break;
    }
    minus_watch_stop();

    // End of game loop. Either:
    //   - back_to_library == true: user picked QUITTER → tear down Ruffle,
    //     reset library/keymap/SWF cache, loop back to library phase.
    //   - back_to_library == false: appletMainLoop() returned false (home
    //     button → Close, applet focus loss without resume, etc.) → full
    //     .nro exit.
    prof_game_active(0);
    ruffle_shutdown();
    gyro_stop(); // the sensor is the game's, like the clocks below
    // Leaving a game always puts the clocks back where they were found, so the
    // experiment can never follow the player into the library or into the next
    // game. No-op unless it was raised. (The OS resets these on its own too,
    // but not until the process dies.)
    flashnx_clocks_restore();
    if (back_to_library && !forwarder_swf) {
        std::printf("game: QUITTER → reset + back to library\n");
        std::fflush(stdout);
        // Back to panel resolution so the library UI is sharp again. After
        // ruffle_shutdown (game renderer gone) and before library_init rebuilds
        // the UI renderer at UI_VIEWPORT_*.
        if (GAME_VIEWPORT_W != UI_VIEWPORT_W || GAME_VIEWPORT_H != UI_VIEWPORT_H) {
            gl_context_resize(UI_VIEWPORT_W, UI_VIEWPORT_H);
        }
        ruffle_library_reset();
    } else {
        // Forwarder launch (or home/close button): there's no library to return
        // to — exit the .nro, which drops back to the HOME menu (i.e. the
        // forwarder's tile), so the shortcut behaves like a native game.
        if (forwarder_swf) {
            std::printf("forwarder: game exited → back to HOME\n");
            std::fflush(stdout);
        }
        exit_nro = true;
    }
    } // end of outer "while (!exit_nro)"

    ruffle_log_cstr("teardown: worker out of its loop, GL going\n");
    gl_context_shutdown();
    flashnx_clocks_restore();
    ruffle_log_cstr("teardown: worker exiting\n");
}

int main(int argc, char** argv) {
    // Forwarder support: a HOME-menu shortcut (an NSP forwarder made on-device
    // with switch-nsp-forwarder / Sphaira) launches FlashNX.nro with a target
    // `.swf` as an argument — exactly like a RetroArch forwarder passes a ROM
    // path. If we were handed one, boot straight into that game and skip the
    // library. Scan all args (argv[0] is our own NRO path under hbloader) for
    // the first that looks like a .swf; pass it to the worker via its thread arg
    // (argv memory outlives the thread — main blocks on threadWaitForExit).
    const char* forwarder_swf = nullptr;
    // A HOME-menu tile started us, whether or not its config names a game:
    // FlashNX's own tile (installed from the applet-mode page) has no config at
    // all and opens the library. It still runs us as the application, so it
    // needs the same exit request as a game's tile (see the end of main).
    bool launched_by_tile = false;
    for (int i = 1; i < argc; ++i) {
        // A HOME-menu tile passes its config file; a hand-built forwarder, or
        // `nxlink --args`, passes the game directly. Both still work.
        if (path_is_tile_cfg(argv[i])) {
            launched_by_tile = true;
            forwarder_swf = resolve_tile_cfg(argv[i]);
            if (forwarder_swf) break;
        } else if (path_is_swf(argv[i])) {
            forwarder_swf = argv[i];
            break;
        }
    }

    g_boot_t0 = armGetSystemTick();
    // Keep the Result: if the socket stack never came up, every later HTTPS
    // call is doomed and nxlinkStdio() has nothing to connect to. It was
    // discarded before, so a failure here surfaced much later as an opaque
    // curl error. Not fatal — the launcher and every game still work offline,
    // only the import/report features are unavailable.
    static bool g_noalloc_marker = false;
    static int g_dlmalloc_marker = 0;   // 1 = honoured, -1 = came too late
    // A/B switch for the small-object allocator cache (2026-08-25). Read here,
    // before the worker thread starts, because the flag has to be set before
    // the first Rust allocation reserves the region.
    {
        struct stat st;
        if (::stat("sdmc:/switch/FlashNX/noalloc.on", &st) == 0) {
            ruffle_alloc_force_off();
            g_noalloc_marker = true;   // announced below, once nxlink is up
            std::fflush(stdout);
        }
        // Same for the medium-block allocator (dlmalloc, 2026-10-01).
        if (::stat("sdmc:/switch/FlashNX/dlmalloc.off", &st) == 0) {
            g_dlmalloc_marker = ruffle_dlmalloc_force_off() ? 1 : -1;
        }
    }
    const Result sock_rc = socketInitializeDefault();
    if (R_FAILED(sock_rc)) {
        std::printf("socketInitializeDefault failed: 0x%x (networking disabled)\n", sock_rc);
        std::fflush(stdout);
    } else {
        nxlinkStdio();
        if (g_noalloc_marker) {
            std::printf("alloc: noalloc.on present -> small-object cache DISABLED\n");
            std::fflush(stdout);
        }
        if (g_dlmalloc_marker != 0) {
            std::printf(g_dlmalloc_marker > 0
                ? "alloc: dlmalloc.off present -> medium blocks go to newlib\n"
                : "alloc: dlmalloc.off present but too late, IGNORED\n");
            std::fflush(stdout);
        }
    }
    // The CPU rate we INHERITED, printed before anything here touches it.
    //
    // This is the instrument for the one claim in the clock code still written
    // as a fact without being one: that Horizon hands the rate back when a
    // process dies. Crash the app while the overclock is on, relaunch, and read
    // this line. 1785 means the rate outlived the process and a crash leaves the
    // console raised; 1020 means something put it back, though not necessarily
    // process death, since apm reprograms pcv on its own transitions too.
    //
    // Useful beyond that test: it also catches a stuck apm boost left behind by
    // an aborted download, which is otherwise invisible until a game starts.
    // Through ruffle_log_cstr, NOT printf: only lines going through that funnel
    // are mirrored into the SD trace, and a probe that reaches nxlink but not
    // the card is useless in exactly the case it exists for, a relaunch after a
    // crash with no PC attached. Cost one run of the test to find out.
    {
        char line[64];
        // The GPU is what makes this line self-diagnosing rather than just a
        // number. 1785 + 76 names apm, whose FastLoad throttles the GPU to its
        // minimum; 1785 + 307 names clkrst, which never touches the GPU. Read
        // alone, the CPU rate cannot tell those two apart.
        std::snprintf(line, sizeof(line), "boot: cpu clock at entry = %u Hz, gpu %u Hz\n",
                      ruffle_cpu_clock_hz(), ruffle_gpu_clock_hz());
        ruffle_log_cstr(line);
    }

    romfsInit();
    // Self-contained build (issue #106): a `.swf` baked into this `.nro` boots
    // like a forwarder target -- straight into the game, no library, no card
    // scan. Read AFTER romfsInit, which is what mounts `romfs:/`.
    //
    // Routed through `forwarder_swf` on purpose rather than as a second
    // mechanism: that variable already carries "boot this one file and skip the
    // library" through the whole worker, including the REGLAGES defaults the
    // library phase would otherwise be the one to apply, and QUITTER exiting to
    // HOME instead of returning to a library that was never built.
    //
    // argv wins when both exist: a shortcut naming a game is an explicit
    // request, and it leaves a way to try another `.swf` against an embedded
    // build without rebuilding it.
    const char* embedded_swf = swf_picker_embedded_swf();
    if (embedded_swf && !forwarder_swf) {
        forwarder_swf = embedded_swf;
    }
    cursor_speed_load(); // restore the saved cursor-speed preset (REGLAGES)

    std::printf("FlashNX: starting (%.0f ms in socket/romfs init)\n",
                boot_ms_since(g_boot_t0));
    std::printf("FlashNX: argc=%d", argc);
    for (int i = 0; i < argc; ++i) {
        std::printf(" argv[%d]=%s", i, argv[i] ? argv[i] : "(null)");
    }
    std::printf("\n");
    if (forwarder_swf) {
        // Which of the two sources won matters when reading a log from a
        // self-contained build: the same variable now has two origins.
        std::printf("FlashNX: boot target = %s (%s)\n", forwarder_swf,
                    (forwarder_swf == embedded_swf) ? "embedded" : "forwarder argv");
    }
    std::fflush(stdout);

    // CpuBoostMode Normal — do NOT use FastLoad.
    //
    // We ran FastLoad from 2026-05-25 to 2026-07-29 on the reasoning that Mario
    // 63 is AVM1-bound (tick 50 ms vs render 5 ms), so trading the GPU for CPU
    // clocks looked free. `FastLoad` boosts the CPU but, per libnx apm.h,
    // "additionally throttles GPU to minimum" — and that half of the trade was
    // never measured.
    //
    // Measured on hardware 2026-07-29, same session, same scenes, alternating
    // between the two modes:
    //     Mario 63        24.8 fps FastLoad  ->  55.3 fps Normal   (+124%)
    //     Papa Louie 3    25.2 fps FastLoad  ->  52.2 fps Normal   (+107%)
    //     Agent P         22.8 fps FastLoad  ->  32.0 fps Normal   (+40%)
    // Every mode change flipped the framerate the same way, across several
    // separate segments per game, so this is not a scene artefact. The GPU
    // throttle costs far more than the CPU boost returns, even for the title
    // the original choice was tuned on.
    //
    // Normal is the OS default; nothing here is out of spec.
    {
        Result rc = appletSetCpuBoostMode(ApmCpuBoostMode_Normal);
        if (R_FAILED(rc)) {
            std::printf("appletSetCpuBoostMode(Normal) failed: 0x%x (continuing)\n", rc);
        } else {
            std::printf("appletSetCpuBoostMode(Normal) OK — GPU not throttled\n");
        }
        std::fflush(stdout);
    }

    // What malloc will really give us, asked before anything else has taken any
    // of it. Every memory investigation so far has reasoned from the 3185 MB the
    // reserved-heap counter reports, and allocations were seen failing at a
    // third of that. Freed immediately; this only runs in the dev build's boot.
    {
        uint64_t biggest = 0;
        const uint64_t total = ruffle_probe_heap_ceiling(32ull * 1024 * 1024, &biggest);
        std::printf("boot: malloc ceiling %llu MB total, biggest single block %llu MB\n",
                    (unsigned long long)(total / (1024 * 1024)),
                    (unsigned long long)(biggest / (1024 * 1024)));
        // The probe walked newlib up to the end of the heap. Hand that back,
        // or the top region for big blocks (src/flashnx_heap.c) would find no
        // room above newlib's break and every big block would fall back to
        // newlib, as before it existed.
        malloc_trim(0);
        uint64_t h[8];
        flashnx_heap_stats(h);
        std::printf("boot: top region %llu MB, room between newlib and it %llu MB\n",
                    (unsigned long long)(h[0] / (1024 * 1024)),
                    (unsigned long long)(h[5] / (1024 * 1024)));
        std::fflush(stdout);
    }

    // Boot-replay: if the previous launch crashed (Rust panic OR native
    // exception), its dump was appended to sdmc:/switch/ruffle-crash.log
    // by either the panic hook or the libnx exception handler. We print
    // and clear it now so the user sees the previous-run diagnostics in
    // this nxlink session — even though the crashing process itself died
    // before its TCP buffer could flush.
    {
        FILE* f = std::fopen("sdmc:/switch/ruffle-crash.log", "r");
        if (f) {
            std::printf("=== previous-run crash log ===\n");
            char buf[512];
            size_t n;
            while ((n = std::fread(buf, 1, sizeof(buf), f)) > 0) {
                std::fwrite(buf, 1, n, stdout);
            }
            std::fclose(f);
            std::printf("=== end previous-run crash log ===\n");
            std::fflush(stdout);
            // Clear it so we only replay each crash once — but KEEP a copy.
            //
            // Deleting outright made the log unrecoverable in the one case it
            // matters most: a crash with the WiFi off. There is no nxlink then,
            // so the only way to read the dump is to pull the file off the SD
            // afterwards (FTP/card reader) — and the app destroyed it the moment
            // it was relaunched, which is exactly what you do to get the WiFi
            // back. Rotating to `-prev` costs nothing and buys one more launch.
            std::remove("sdmc:/switch/ruffle-crash-prev.log");
            if (std::rename("sdmc:/switch/ruffle-crash.log",
                            "sdmc:/switch/ruffle-crash-prev.log") != 0) {
                // Rename unavailable/failed: fall back to deleting, otherwise
                // this same dump replays on every future boot.
                std::remove("sdmc:/switch/ruffle-crash.log");
            }
        }
    }

    // Register our `.swf` file association with Sphaira (if installed) so a Flash
    // game can be turned into a Home-menu shortcut from Sphaira's file browser.
    //
    // Never from a self-contained build (#106): that .nro plays the one game it
    // carries and ignores any other path, so claiming the association would
    // point every Flash file on the console at an app that cannot open them --
    // and it would stick, because the registration only writes when no assoc
    // file exists yet and then leaves it alone forever.
    if (!embedded_swf) {
        register_sphaira_assoc(argc > 0 ? argv[0] : nullptr);
        remember_own_path(argc > 0 ? argv[0] : nullptr);
        remember_self(argc, argv);
    }
    std::printf("boot: main() pre-worker done at %.0f ms\n", boot_ms_since(g_boot_t0));
    std::fflush(stdout);

    // Spawn the Ruffle worker with a 32 MB stack. NULL stack_mem → libnx
    // allocates from heap, so we don't bloat .nro BSS. Priority 0x2C is the
    // libnx default; bumping it to 0x20 was tested 2026-05-25 soir and
    // produced no measurable FPS improvement (the Switch isn't loaded with
    // competing threads, so priority doesn't help). cpuid=-2 lets the
    // kernel pick the least-loaded core. The CpuBoostMode_FastLoad set
    // above is the perf lever that actually moved the needle.
    Thread t;
    Result rc = threadCreate(&t, worker_entry, (void*)forwarder_swf,
                              nullptr, 32 * 1024 * 1024,
                              0x2C, -2);
    if (R_FAILED(rc)) {
        std::printf("threadCreate failed: 0x%x\n", rc); std::fflush(stdout);
        romfsExit();
        socketExit();
        return EXIT_FAILURE;
    }
    prof_boot(&t);
    threadStart(&t);
    threadWaitForExit(&t);
    ruffle_log_cstr("teardown: worker joined\n");
    // The launcher's interface sounds bring audren up on their own (UI mode),
    // and only a game's audio backend ever shut it down. Left open, its worker
    // would keep waking every audren frame inside an NRO the loader is about
    // to unmap.
    ruffle_audio_shutdown();
    ruffle_log_cstr("teardown: audio down\n");
    prof_shutdown();
    threadClose(&t);

    // The network's two threads (net.cpp), stopped while the sockets they may
    // be using still exist.
    https_thumb_stop();
    https_job_stop();
    flashnx_update_check_stop();
    ruffle_log_cstr("teardown: network threads stopped\n");
    // And the trash's, after the file it is on; the rest waits for next time.
    swf_picker_trash_stop();

    // Logged before: nxlink's output goes through the sockets.
    ruffle_log_cstr("teardown: closing romfs and sockets\n");
    romfsExit();
    socketExit();

    // A HOME-menu tile runs us as the APPLICATION itself, and an application
    // has to tell the OS it is leaving (ISelfController::Exit) before its
    // process ends. libnx only does that for an NSO (`envIsNso()`), and we are
    // an NRO loaded by the tile's nx-hbloader, so quitting left the OS to find
    // a dead application and say "the software was closed because an error
    // occurred". Exit mode 1 makes `appletExit` register `_appletExitProcess`
    // as the exit routine, which reopens the applet session and sends that
    // request, the way nx.js's forwarder (bootstrap/launcher-nsp) and nxmp do.
    // Not when the OS runs us as an applet (hbmenu, Sphaira's file browser):
    // there the library applet's version would end the process instead of
    // handing back to the menu.
    // FlashNX's own tile counts too: quitting from Settings after it had opened
    // the library put up the same error, because only a tile that named a game
    // set this.
    // Except when an update starts FlashNX again: the process does not end
    // then, the tile's forwarder loads the new `.nro` into it the way hbmenu
    // loads a game, and exit mode 1 would end it before that.
    if ((forwarder_swf || launched_by_tile) && !g_restart_requested) {
        const AppletType type = appletGetAppletType();
        if (type == AppletType_Application || type == AppletType_SystemApplication) {
            __nx_applet_exit_mode = 1;
        }
    }

    return EXIT_SUCCESS;
}
