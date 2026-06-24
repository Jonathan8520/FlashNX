#include <switch.h>
#include <cstdio>
#include <cstdlib>
#include <cstdint>
#include <cstring>
#include <sys/stat.h>

#include "ruffle_bridge.h"

extern "C" bool gl_context_init(NWindow* win);
extern "C" void gl_context_shutdown(void);
extern "C" void gl_context_swap(void);

extern "C" void swf_picker_run(void);

// Phase 3.4 — library boot screen FFI (rust/src/library.rs).
extern "C" int  ruffle_library_init(void);
extern "C" int  ruffle_library_add_path(const char* path, unsigned long long mtime);
extern "C" void ruffle_library_open(void);
extern "C" int  ruffle_library_active(void);
extern "C" int  ruffle_library_picked(void);
extern "C" int  ruffle_library_input(const char* button_name);
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
extern "C" void ruffle_draw_menu(int selected);
extern "C" void ruffle_menu_close_begin(void);
extern "C" int  ruffle_draw_menu_closing(int selected);
extern "C" int  ruffle_restart(void);
extern "C" int  ruffle_keymap_lookup(const char* button_name);
extern "C" int  ruffle_keymap_lookup_p2(const char* button_name);
extern "C" int  ruffle_keymap_cursor_speed(void);          // per-game speed, -1 = unset
extern "C" void ruffle_keymap_set_cursor_speed(int idx);   // persist per-game speed
extern "C" void ruffle_touches_open(void);
extern "C" void ruffle_touches_close(void);
extern "C" int  ruffle_touches_active(void);
extern "C" int  ruffle_touches_input(const char* button_name);
extern "C" int  ruffle_touches_consume_dirty(void);
extern "C" void ruffle_touches_draw(void);

// In-game software keyboard (raised when a Flash TextField gains focus).
extern "C" int  ruffle_keyboard_take_request(void);
extern "C" int  ruffle_keyboard_field(char* out, int cap, int* out_flags, int* out_max);
extern "C" int  ruffle_keyboard_submit(const char* text);
extern "C" int  swkbd_prompt_game_field(const char* initial, int flags, int maxlen, char* out, int cap);

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
};
static constexpr size_t BINDINGS_COUNT = sizeof(BINDINGS) / sizeof(BINDINGS[0]);

// Player 2 (issue #40): resolved Flash key per BINDINGS entry for controller 2,
// via the P2 keymap. Same buttons/masks as BINDINGS, different keys.
static int BINDINGS_P2_KEYS[BINDINGS_COUNT];

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
    // L/R: hold-to-repeat ON so DistantFiles page-up/down can be held to
    // scroll quickly through thousand-entry archive.org dumps. DistantIdle
    // history cycle is fine with repeat (just bumps the visible URL index).
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
        if (kDown & nb.mask) {
            rs.held_since[i] = now_tick;
            rs.last_emit[i] = now_tick;
            rs.first_repeat_done[i] = false;
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
                    forward(nb.name);
                }
            } else if (now_tick - rs.last_emit[i] >= REPEAT_INTERVAL) {
                rs.last_emit[i] = now_tick;
                forward(nb.name);
            }
        }
    }
}

static void populate_bindings_from_keymap(void) {
    for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
        BINDINGS[i].key = ruffle_keymap_lookup(BINDINGS[i].name);
        BINDINGS_P2_KEYS[i] = ruffle_keymap_lookup_p2(BINDINGS[i].name); // #40
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
    MENU_RESTART      = 2,
    MENU_QUIT         = 3,  // VITESSE moved INTO the TOUCHES sub-menu
    MENU_COUNT        = 4,
};

// Viewport size — keep in sync with rust/src/lib.rs VIEWPORT_W/H.
static constexpr int VIEWPORT_W = 1280;
static constexpr int VIEWPORT_H = 720;

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
static void cursor_speed_load() {
    FILE* f = std::fopen("sdmc:/flashnx/cursor_speed", "rb");
    if (f) {
        int v = 1;
        if (std::fscanf(f, "%d", &v) == 1) g_cursor_speed_idx = v;
        std::fclose(f);
    }
    cursor_speed_apply();
}
// Cycle to the next preset, apply, persist. Returns the new index.
extern "C" int ruffle_cursor_speed_cycle(void) {
    g_cursor_speed_idx = (g_cursor_speed_idx + 1) % CURSOR_SPEED_COUNT;
    cursor_speed_apply();
    if (g_in_game) {
        // In a game → save to THIS game's <basename>.cursor (via Rust). No bleed
        // onto other games or the global default.
        ruffle_keymap_set_cursor_speed(g_cursor_speed_idx);
    } else {
        // Library / RÉGLAGES → the GLOBAL default file.
        FILE* f = std::fopen("sdmc:/flashnx/cursor_speed", "wb");
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

// True if `path` ends in ".swf" (case-insensitive). Recognises a forwarder
// launch argument — a HOME-menu shortcut (NSP forwarder) to a single game.
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

// All of the GL + Ruffle work runs in a dedicated worker thread with a
// 32 MB stack (vs the default ~1 MB main-thread stack from nx-hbloader).
// Mario 63's AS2 preload recursion + Ruffle's AVM1 interpreter + GC arena
// traversal blew through 1 MB at frame ~40, producing a silent SIGSEGV
// that left no Rust-panic trail. With 32 MB we should have headroom for
// any AS2 game we'll realistically run.
static void worker_entry(void* arg) {
    // A non-null arg is a forwarder launch: the absolute path of a single `.swf`
    // to boot straight into (a HOME-menu shortcut to one game), skipping the
    // library. The pointer is an `argv[i]` from main(), valid for the whole run
    // (main blocks on threadWaitForExit). NULL = normal launch → show library.
    const char* forwarder_swf = static_cast<const char*>(arg);
    std::printf("worker: starting (32 MB stack)\n"); std::fflush(stdout);

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
    if (!gl_context_init(win)) {
        std::printf("gl_context_init failed\n"); std::fflush(stdout);
        return;
    }

    // Pad + touch init moved BEFORE the library so both the library boot
    // screen and the in-game loop share one PadState instance. Pad config
    // doesn't change between the two phases.
    PadState pad;
    padConfigureInput(2, HidNpadStyleSet_NpadStandard); // up to 2 players (issue #40)
    padInitializeDefault(&pad);
    // Player 2 controller (issue #40). Idle / absent = no input, so single-player
    // is unaffected. Local 2-player Flash games (e.g. DBZ Devolution) read two
    // key-sets on one keyboard; controller 2 feeds the P2 keymap into the same
    // ruffle_handle_key pipeline. Needs two full controllers (Pro / dual Joy-Con);
    // one Joy-Con each would need extra style flags (follow-up).
    PadState pad2;
    padInitializeWithMask(&pad2, 1UL << HidNpadIdType_No2);

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
    if (!forwarder_swf) {
    g_in_game = false;
    // The library / RÉGLAGES cursor speed is the GLOBAL default — re-read it on
    // entering the library so it doesn't show the last game's per-game speed.
    cursor_speed_load();
    if (ruffle_library_init() != 0) {
        std::printf("library_init failed — exiting .nro\n");
        std::fflush(stdout);
        break;
    }
    // Enumerate every .swf on SD and push to the Rust library state.
    swf_picker_run();
    ruffle_library_open();

    MenuRepeatState lib_repeat;
    menu_repeat_reset(lib_repeat);
    while (ruffle_library_active() && appletMainLoop()) {
        padUpdate(&pad);
        const u64 kDownLib = padGetButtonsDown(&pad);
        const u64 kUpLib   = padGetButtonsUp(&pad);
        const u64 kHeldLib = padGetButtons(&pad);
        const u64 now_lib  = ruffle_tick_now();
        // Forward joycon edges + auto-repeat (D-pad/sticks) into the
        // library state machine. Pause / Plus / face buttons are one-shot
        // (no repeat) — see MENU_NAV_BUTTONS.repeat flags.
        menu_repeat_step(lib_repeat, kDownLib, kUpLib, kHeldLib,
                         now_lib, tick_freq_global, ruffle_library_input);
        // Hidden ZL+ZR chord: emit one synthetic "ZL+ZR" event on the frame the
        // pair completes (both held now, not both held before this frame's new
        // presses). The library only acts on it in the Flashpoint results grid
        // (toggle the content filter, issue #33); ZL/ZR are no-ops there on their
        // own, so the individual events menu_repeat_step also forwarded are inert.
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
        const float lib_tx = lib_touch ? (float)touch_state.touches[0].x : 0.0f;
        const float lib_ty = lib_touch ? (float)touch_state.touches[0].y : 0.0f;
        ruffle_library_touch(lib_tx, lib_ty, lib_touch ? 1 : 0);
        ruffle_library_render();
        gl_context_swap();
    }

    const bool picked = ruffle_library_picked() != 0;
    char selected_path[512] = {0};
    if (!picked || ruffle_library_selected_path(selected_path, sizeof(selected_path)) != 0) {
        std::printf("library: user quit (no selection) — exiting .nro\n");
        std::fflush(stdout);
        ruffle_library_shutdown();
        break;
    }
    std::printf("library: user picked %s\n", selected_path);
    std::fflush(stdout);
    // Forward the choice to Ruffle's loader (consumed by find_and_
    // load_swf_uncached → CACHED_SWF in lib.rs).
    ruffle_set_swf_path(selected_path);

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
    {
        int cs = ruffle_keymap_cursor_speed();
        if (cs >= 0) {
            g_cursor_speed_idx = cs;
            cursor_speed_apply();
        } else {
            cursor_speed_load(); // global default
        }
    }
    g_in_game = true;

    // Mouse cursor — centred at start.
    float cursor_x = VIEWPORT_W * 0.5f;
    float cursor_y = VIEWPORT_H * 0.5f;
    ruffle_handle_mouse_move((int)cursor_x, (int)cursor_y);
    touch_was_pressed = false;

    // Real-time pacing: instead of telling Ruffle "16.6 ms elapsed" every tick,
    // we measure actual wall-clock between iterations and let its frame
    // accumulator decide how many SWF frames to run. Matches the desktop
    // Ruffle pacing model (core/src/player.rs::tick).
    const uint64_t tick_freq = ruffle_tick_freq();
    uint64_t last_tick = ruffle_tick_now();

    // Pause-modal state. While `menu_open` is true the Ruffle frame loop is
    // skipped (we just re-render the last Player state under the overlay)
    // and joycon input is rerouted to menu navigation only.
    bool menu_open = false;
    // While true, the pause menu is playing its scale-out close pop; input is
    // suspended and we drain `ruffle_draw_menu_closing` until it reports done,
    // then resume the game. Keeps the close animated instead of snapping shut.
    bool menu_closing = false;
    int  menu_selection = MENU_RESUME;
    // MENU_QUIT now means "back to library" — controlled by this flag.
    // appletMainLoop returning false (home button → Close) also exits the
    // inner loop but with back_to_library=false → full .nro exit.
    bool back_to_library = false;

    // Hold-to-repeat state for the in-game TOUCHES editor (16 entries
    // scrollable list — benefits a lot from D-pad auto-repeat).
    MenuRepeatState touches_repeat;
    menu_repeat_reset(touches_repeat);

    // CpuBoostMode re-assert counter. Hardware profiling (2026-05-30) showed
    // the OS drops the boot-time FastLoad boost back to the 1020 MHz handheld
    // base clock mid-gameplay (seen in the heavy water lake), and the worst fps
    // dips (9-12 fps) lined up with those 1020 MHz windows while the same scene
    // held ~15 fps at 1785 MHz. So we re-assert periodically to recover any
    // transient drop. IMPORTANT FINDING: re-asserting every single frame did
    // NOT eliminate the drops — they recur at the SAME elapsed time each run
    // (~25-30 s into sustained load), i.e. the apm sysmodule *force-revokes*
    // FastLoad on sustained use (it's officially a short loading-screen boost)
    // and we cannot override that, whatever the cadence. So per-frame re-assert
    // only spammed the IPC for no gain; every 30 frames recovers non-forced
    // drops without the spam. NB: even pinned at 1785 MHz the lake is AVM1-bound
    // (~15 fps) — this only recovers dropped windows, it doesn't lift Ruffle's
    // interpreter ceiling. Still stock spec, no hardware risk.
    int boost_reassert = 0;

    while (appletMainLoop()) {
        padUpdate(&pad);
        const u64 kDown = padGetButtonsDown(&pad);
        const u64 kUp   = padGetButtonsUp(&pad);
        const u64 kHeld = padGetButtons(&pad);
        // Player 2 controller (issue #40). Read every frame; only acted on
        // during gameplay (not in the pause menu / sub-screens).
        padUpdate(&pad2);
        const u64 kDown2 = padGetButtonsDown(&pad2);
        const u64 kUp2   = padGetButtonsUp(&pad2);
        const u64 kHeld2 = padGetButtons(&pad2);

        if (++boost_reassert >= 30) {
            boost_reassert = 0;
            appletSetCpuBoostMode(ApmCpuBoostMode_FastLoad);
        }

        if (menu_open) {
            // ─── Closing drain: play the scale-out, then resume the game ──
            // On dismiss we don't snap shut; we keep re-rendering the frozen
            // frame + the shrinking menu until the close pop reports done.
            if (menu_closing) {
                ruffle_redraw_paused();
                if (ruffle_draw_menu_closing(menu_selection)) {
                    menu_open = false;
                    menu_closing = false;
                    // Re-measure wall clock so the resumed frame doesn't catch
                    // up by replaying the paused + closing interval.
                    last_tick = ruffle_tick_now();
                }
                gl_context_swap();
                continue;
            }

            // ─── Sub-screen branch: TOUCHES (Rust-driven) ───────────────
            // If the user picked "TOUCHES" earlier, Rust now owns the
            // input + rendering for the keymap editor. We just forward
            // joycon down-edges and ask it to draw.
            if (ruffle_touches_active()) {
                menu_repeat_step(touches_repeat, kDown, kUp, kHeld,
                                 ruffle_tick_now(), tick_freq_global,
                                 ruffle_touches_input);
                // If a binding was committed, refresh our runtime BINDINGS
                // so the change applies immediately (no need to REDEMARRER).
                if (ruffle_touches_consume_dirty()) {
                    populate_bindings_from_keymap();
                }
                ruffle_redraw_paused();
                // The input above may have CLOSED the sub-menu (B). If so, drawing
                // ruffle_touches_draw() now would draw nothing over the game — the
                // dim vanishes for one frame and the bright game flashes through.
                // Draw the pause menu instead so a dim is always present.
                if (ruffle_touches_active()) {
                    ruffle_touches_draw();
                } else {
                    ruffle_draw_menu(menu_selection);
                }
                gl_context_swap();
                continue;
            }

            // ─── Pause main menu ────────────────────────────────────────
            // Edge-detected nav so a held D-pad doesn't scroll past every
            // entry. Press `-` again or `B` to dismiss (= Resume).
            // Right stick navigates too (it's the mouse cursor only when the
            // menu is closed — this branch `continue`s before the cursor code).
            if (kDown & (HidNpadButton_Up | HidNpadButton_StickLUp | HidNpadButton_StickRUp)) {
                menu_selection = (menu_selection + MENU_COUNT - 1) % MENU_COUNT;
            }
            if (kDown & (HidNpadButton_Down | HidNpadButton_StickLDown | HidNpadButton_StickRDown)) {
                menu_selection = (menu_selection + 1) % MENU_COUNT;
            }
            if (kDown & (HidNpadButton_Minus | HidNpadButton_B)) {
                // Scale the menu out (drained at the top of this branch over the
                // next frames), then resume.
                ruffle_menu_close_begin();
                menu_closing = true;
                continue;
            }
            if (kDown & HidNpadButton_A) {
                switch (menu_selection) {
                case MENU_RESUME:
                    ruffle_menu_close_begin();
                    menu_closing = true;
                    continue;
                case MENU_TOUCHES:
                    // Hand control to the Rust TOUCHES sub-screen.
                    // menu_open stays true so we re-enter the branch above
                    // next frame; Rust closes itself on B/Minus.
                    ruffle_touches_open();
                    continue;
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
                    last_tick = ruffle_tick_now();
                    continue;
                }
                case MENU_QUIT:
                    back_to_library = true;
                    break;
                }
                if (back_to_library) break;
            }

            // Re-render the frozen game frame + cursor, then layer the menu
            // on top, then swap. Skipping ruffle_render_frame_dt freezes AVM
            // so the game doesn't advance.
            ruffle_redraw_paused();
            ruffle_draw_menu(menu_selection);
            gl_context_swap();
            continue;
        }

        // `-` opens the pause modal. Captured before BINDINGS so it can't
        // also fire a key event into Ruffle.
        if (kDown & HidNpadButton_Minus) {
            menu_open = true;
            menu_selection = MENU_RESUME;
            // Release any held in-game keys so the player doesn't keep
            // running / jumping while paused.
            for (const auto& b : BINDINGS) {
                if (kHeld & b.mask) ruffle_handle_key(b.key, false);
            }
            // Same for player 2's held keys (issue #40).
            for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
                if ((kHeld2 & BINDINGS[i].mask) && BINDINGS_P2_KEYS[i] != SK_NONE) {
                    ruffle_handle_key(BINDINGS_P2_KEYS[i], false);
                }
            }
            continue;
        }

        // Keyboard-style buttons via edge detection. A button bound to a mouse
        // pseudo-code clicks at the cursor instead of sending a key event.
        for (const auto& b : BINDINGS) {
            const bool dn = (kDown & b.mask) != 0;
            const bool up = (kUp   & b.mask) != 0;
            if (!dn && !up) continue;
            if (b.key == SK_MOUSE_LEFT) {
                if (dn) ruffle_handle_mouse_button(true);
                if (up) ruffle_handle_mouse_button(false);
            } else if (b.key == SK_MOUSE_RIGHT) {
                if (dn) ruffle_handle_mouse_right(true);
                if (up) ruffle_handle_mouse_right(false);
            } else {
                if (dn) ruffle_handle_key(b.key, true);
                if (up) ruffle_handle_key(b.key, false);
            }
        }

        // Player 2 (issue #40): a 2nd controller drives the same key pipeline
        // through the P2 keymap. No cursor / mouse / pause for P2 (one cursor,
        // and P1 owns the menu), so we only emit key down/up here.
        for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
            const int k = BINDINGS_P2_KEYS[i];
            if (k == SK_NONE || k == SK_MOUSE_LEFT || k == SK_MOUSE_RIGHT) continue;
            const u64 mask = BINDINGS[i].mask;
            if (kDown2 & mask) ruffle_handle_key(k, true);
            if (kUp2   & mask) ruffle_handle_key(k, false);
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
        if (!g_left_stick_dpad) {
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

        // Touch input — overrides stick position when active. We translate
        // touch X/Y (in Switch screen pixels, 1280x720 docked or 1280x720
        // handheld) directly to our viewport.
        hidGetTouchScreenStates(&touch_state, 1);
        const bool touch_pressed = touch_state.count > 0;
        if (touch_pressed) {
            cursor_x = (float)touch_state.touches[0].x;
            cursor_y = (float)touch_state.touches[0].y;
            moved = true;
        }

        // Clamp cursor to the viewport.
        if (cursor_x < 0)              cursor_x = 0;
        if (cursor_y < 0)              cursor_y = 0;
        if (cursor_x > VIEWPORT_W - 1) cursor_x = VIEWPORT_W - 1;
        if (cursor_y > VIEWPORT_H - 1) cursor_y = VIEWPORT_H - 1;

        if (moved) {
            ruffle_handle_mouse_move((int)cursor_x, (int)cursor_y);
        }

        // Click: touch tap → left mouse button. ZR (and any other button) is now
        // fully keymap-driven (ZR defaults to "Left click"), handled in the
        // BINDINGS loop above — there's no hardcoded ZR path anymore.
        if (touch_pressed && !touch_was_pressed) {
            ruffle_handle_mouse_button(true);
        } else if (!touch_pressed && touch_was_pressed) {
            ruffle_handle_mouse_button(false);
        }
        touch_was_pressed = touch_pressed;

        // In-game software keyboard. Ruffle's focus tracker raises a request
        // when an editable TextField gains focus (e.g. the user clicked it with
        // the cursor / touch). We suspend the game, run swkbd configured to
        // match the field (prefill, password/numeric/multiline, max length),
        // and feed the result back through normal text events. swkbdShow blocks,
        // so no frames advance while it's open.
        if (ruffle_keyboard_take_request()) {
            char kbd_prefill[1024];
            char kbd_out[1024];
            int  kbd_flags = 0, kbd_max = 0;
            if (ruffle_keyboard_field(kbd_prefill, sizeof(kbd_prefill), &kbd_flags, &kbd_max)) {
                if (swkbd_prompt_game_field(kbd_prefill, kbd_flags, kbd_max,
                                            kbd_out, sizeof(kbd_out)) == 0) {
                    ruffle_keyboard_submit(kbd_out);
                }
            }
            // The modal held the loop for a while; re-measure the clock so the
            // next frame doesn't replay the elapsed interval.
            last_tick = ruffle_tick_now();
        }

        // Compute real elapsed since last iteration → microseconds.
        // tick_freq is ~19.2 MHz on Switch, so this is precise to ~50ns.
        const uint64_t now_tick = ruffle_tick_now();
        const uint64_t dt_ticks = now_tick - last_tick;
        last_tick = now_tick;
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
        if (dt_us > 100000ULL) dt_us = 100000ULL;

        ruffle_render_frame_dt(dt_us);
        gl_context_swap();

        if (back_to_library) break;
    }

    // End of game loop. Either:
    //   - back_to_library == true: user picked QUITTER → tear down Ruffle,
    //     reset library/keymap/SWF cache, loop back to library phase.
    //   - back_to_library == false: appletMainLoop() returned false (home
    //     button → Close, applet focus loss without resume, etc.) → full
    //     .nro exit.
    ruffle_shutdown();
    if (back_to_library && !forwarder_swf) {
        std::printf("game: QUITTER → reset + back to library\n");
        std::fflush(stdout);
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

    gl_context_shutdown();
    std::printf("worker: exiting\n"); std::fflush(stdout);
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
    for (int i = 1; i < argc; ++i) {
        if (path_is_swf(argv[i])) {
            forwarder_swf = argv[i];
            break;
        }
    }

    socketInitializeDefault();
    nxlinkStdio();
    romfsInit();
    cursor_speed_load(); // restore the saved cursor-speed preset (REGLAGES)

    std::printf("FlashNX: starting\n");
    std::printf("FlashNX: argc=%d", argc);
    for (int i = 0; i < argc; ++i) {
        std::printf(" argv[%d]=%s", i, argv[i] ? argv[i] : "(null)");
    }
    std::printf("\n");
    if (forwarder_swf) {
        std::printf("FlashNX: forwarder target = %s\n", forwarder_swf);
    }
    std::fflush(stdout);

    // CpuBoostMode FastLoad — Mario 63 is bottlenecked by Ruffle's AVM1
    // bytecode interpreter on the Cortex-A57 (Tegra X1, 1.02 GHz handheld /
    // 1.78 GHz docked). Profiling on hardware (2026-05-25 soir) showed
    // tick=50ms/frame vs render=5ms/frame in heavy scenes → CPU-bound, not
    // GPU. `FastLoad` (libnx name for "Type1") boosts CPU clocks and
    // throttles the GPU to minimum, which we can afford — our render is
    // < 5ms even worst case. Within stock clock specs; same API Nintendo's
    // own titles use for loading screens. No hardware risk.
    {
        Result rc = appletSetCpuBoostMode(ApmCpuBoostMode_FastLoad);
        if (R_FAILED(rc)) {
            std::printf("appletSetCpuBoostMode(FastLoad) failed: 0x%x (continuing)\n", rc);
        } else {
            std::printf("appletSetCpuBoostMode(FastLoad) OK — CPU prioritized over GPU\n");
        }
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
            // Truncate so we only replay each crash once.
            std::remove("sdmc:/switch/ruffle-crash.log");
        }
    }

    // Register our `.swf` file association with Sphaira (if installed) so a Flash
    // game can be turned into a Home-menu shortcut from Sphaira's file browser.
    register_sphaira_assoc(argc > 0 ? argv[0] : nullptr);

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
    threadStart(&t);
    threadWaitForExit(&t);
    threadClose(&t);

    romfsExit();
    socketExit();
    return EXIT_SUCCESS;
}
