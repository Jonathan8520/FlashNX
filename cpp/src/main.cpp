#include <switch.h>
#include <cstdio>
#include <cstdlib>
#include <cstdint>

#include "ruffle_bridge.h"

extern "C" bool gl_context_init(NWindow* win);
extern "C" void gl_context_shutdown(void);
extern "C" void gl_context_swap(void);

extern "C" void swf_picker_run(void);

// Phase 3.4 — library boot screen FFI (rust/src/library.rs).
extern "C" int  ruffle_library_init(void);
extern "C" int  ruffle_library_add_path(const char* path);
extern "C" void ruffle_library_open(void);
extern "C" int  ruffle_library_active(void);
extern "C" int  ruffle_library_picked(void);
extern "C" int  ruffle_library_input(const char* button_name);
extern "C" void ruffle_library_render(void);
extern "C" int  ruffle_library_selected_path(char* out, int cap);
extern "C" void ruffle_library_shutdown(void);

extern "C" void ruffle_handle_key(int code, bool down);
extern "C" void ruffle_handle_mouse_move(int x, int y);
extern "C" void ruffle_handle_mouse_button(bool down);
extern "C" void ruffle_redraw_paused(void);
extern "C" void ruffle_draw_menu(int selected);
extern "C" int  ruffle_restart(void);
extern "C" int  ruffle_keymap_lookup(const char* button_name);
extern "C" void ruffle_touches_open(void);
extern "C" void ruffle_touches_close(void);
extern "C" int  ruffle_touches_active(void);
extern "C" int  ruffle_touches_input(const char* button_name);
extern "C" int  ruffle_touches_consume_dirty(void);
extern "C" void ruffle_touches_draw(void);

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
    // ZR is hardcoded as left-mouse-click below (touch fallback shares it);
    // we still expose it here so a future keymap could repurpose it if the
    // user disables mouse — for now ZR in the JSON is no-op (handled
    // alongside touch in the click path).
    { "Plus",         HidNpadButton_Plus,         SK_NONE },
    { "Left",         HidNpadButton_Left,         SK_NONE },
    { "Right",        HidNpadButton_Right,        SK_NONE },
    { "Up",           HidNpadButton_Up,           SK_NONE },
    { "Down",         HidNpadButton_Down,         SK_NONE },
    { "StickLLeft",   HidNpadButton_StickLLeft,   SK_NONE },
    { "StickLRight",  HidNpadButton_StickLRight,  SK_NONE },
    { "StickLUp",     HidNpadButton_StickLUp,     SK_NONE },
    { "StickLDown",   HidNpadButton_StickLDown,   SK_NONE },
};
static constexpr size_t BINDINGS_COUNT = sizeof(BINDINGS) / sizeof(BINDINGS[0]);

// Buttons forwarded to the Rust TOUCHES sub-screen as down-edges. Names
// match what `keymap::EDITABLE_BUTTONS` exposes (plus the nav-only ones
// like Up/Down/A/B/Minus). Keep in sync with the EDITABLE_BUTTONS slice.
struct MenuNavButton {
    const char* name;
    u64         mask;
};
static const MenuNavButton MENU_NAV_BUTTONS[] = {
    { "A",            HidNpadButton_A            },
    { "B",            HidNpadButton_B            },
    { "X",            HidNpadButton_X            },
    { "Y",            HidNpadButton_Y            },
    { "L",            HidNpadButton_L            },
    { "R",            HidNpadButton_R            },
    { "ZL",           HidNpadButton_ZL           },
    { "Plus",         HidNpadButton_Plus         },
    { "Minus",        HidNpadButton_Minus        },
    { "Up",           HidNpadButton_Up           },
    { "Down",         HidNpadButton_Down         },
    { "Left",         HidNpadButton_Left         },
    { "Right",        HidNpadButton_Right        },
    { "StickLUp",     HidNpadButton_StickLUp     },
    { "StickLDown",   HidNpadButton_StickLDown   },
    { "StickLLeft",   HidNpadButton_StickLLeft   },
    { "StickLRight",  HidNpadButton_StickLRight  },
};

static void populate_bindings_from_keymap(void) {
    for (size_t i = 0; i < BINDINGS_COUNT; ++i) {
        BINDINGS[i].key = ruffle_keymap_lookup(BINDINGS[i].name);
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
    MENU_RESUME  = 0,
    MENU_TOUCHES = 1,  // opens the keymap editor sub-screen (Rust-driven)
    MENU_RESTART = 2,
    MENU_QUIT    = 3,
    MENU_COUNT   = 4,
};

// Viewport size — keep in sync with rust/src/lib.rs VIEWPORT_W/H.
static constexpr int VIEWPORT_W = 1280;
static constexpr int VIEWPORT_H = 720;

// Right-stick axis range (libnx reports -0x7FFF..0x7FFF).
static constexpr float STICK_DEADZONE = 4000.0f;
static constexpr float STICK_MAX      = 32767.0f;
// Cursor speed in pixels per frame at full stick deflection.
static constexpr float CURSOR_SPEED   = 12.0f;

// All of the GL + Ruffle work runs in a dedicated worker thread with a
// 32 MB stack (vs the default ~1 MB main-thread stack from nx-hbloader).
// Mario 63's AS2 preload recursion + Ruffle's AVM1 interpreter + GC arena
// traversal blew through 1 MB at frame ~40, producing a silent SIGSEGV
// that left no Rust-panic trail. With 32 MB we should have headroom for
// any AS2 game we'll realistically run.
static void worker_entry(void* /*arg*/) {
    std::printf("worker: starting (32 MB stack)\n"); std::fflush(stdout);

    NWindow* win = nwindowGetDefault();
    if (!gl_context_init(win)) {
        std::printf("gl_context_init failed\n"); std::fflush(stdout);
        return;
    }

    // Pad + touch init moved BEFORE the library so both the library boot
    // screen and the in-game loop share one PadState instance. Pad config
    // doesn't change between the two phases.
    PadState pad;
    padConfigureInput(1, HidNpadStyleSet_NpadStandard);
    padInitializeDefault(&pad);

    hidInitializeTouchScreen();
    HidTouchScreenState touch_state = {0};
    bool touch_was_pressed = false;

    // ── Phase 3.4: library launcher (FlashNX picker) ───────────────────
    //
    // Before booting Ruffle, show the library UI: scan SD, list all .swf
    // files, let the user pick one. A=JOUER → ruffle_set_swf_path + boot
    // Ruffle on the chosen file. -=QUITTER → exit the .nro cleanly without
    // ever initialising Ruffle. Two SwitchRenderBackend instances are
    // created over the boot — one for the library, dropped before Ruffle's
    // own gets built; ~96 MB GPU arena per instance but never alive at
    // the same time.
    if (ruffle_library_init() == 0) {
        // Enumerate every .swf on SD and push to the Rust library state.
        swf_picker_run();
        ruffle_library_open();

        while (ruffle_library_active() && appletMainLoop()) {
            padUpdate(&pad);
            const u64 kDownLib = padGetButtonsDown(&pad);
            // Forward joycon down-edges into the library state machine.
            // Reuses the same MENU_NAV_BUTTONS table the pause menu uses —
            // covers A/B/X/Y/Up/Down/Left/Right/sticks/Minus/Plus.
            for (const auto& nb : MENU_NAV_BUTTONS) {
                if (kDownLib & nb.mask) ruffle_library_input(nb.name);
            }
            ruffle_library_render();
            gl_context_swap();
        }

        const bool picked = ruffle_library_picked() != 0;
        char selected_path[512] = {0};
        if (picked && ruffle_library_selected_path(selected_path, sizeof(selected_path)) == 0) {
            std::printf("library: user picked %s\n", selected_path);
            std::fflush(stdout);
            // Forward the choice to Ruffle's loader (consumed by find_and_
            // load_swf_uncached → CACHED_SWF in lib.rs).
            ruffle_set_swf_path(selected_path);
        } else {
            std::printf("library: user quit (no selection) — exiting .nro\n");
            std::fflush(stdout);
            ruffle_library_shutdown();
            gl_context_shutdown();
            return;
        }

        // Drop the library's standalone SwitchRenderBackend so its GL
        // resources (~96 MB arenas + shader programs + banner texture)
        // free BEFORE ruffle_init allocates Ruffle's own renderer. Without
        // this we'd peak at ~200 MB GPU during the cross-over second.
        ruffle_library_shutdown();
    } else {
        // Library init failed (renderer alloc failed). Fall back to the
        // original swf_picker first-hit logic + embedded SWF — bare bones,
        // but at least the .nro doesn't refuse to boot.
        std::printf("library_init failed — falling back to first-hit picker\n");
        std::fflush(stdout);
        swf_picker_run();
    }

    if (ruffle_init() != 0) {
        std::printf("ruffle_init failed\n"); std::fflush(stdout);
        gl_context_shutdown();
        return;
    }

    // Resolve "A", "B", ..., "StickLDown" to their SK_* codes per the
    // user's keymap (loaded inside ruffle_init via keymap::init_for_swf).
    // Done once at boot — keymap is locked in a OnceLock so it doesn't
    // change across REDEMARRER cycles.
    populate_bindings_from_keymap();

    // Mouse cursor — centred at start.
    float cursor_x = VIEWPORT_W * 0.5f;
    float cursor_y = VIEWPORT_H * 0.5f;
    ruffle_handle_mouse_move((int)cursor_x, (int)cursor_y);
    bool zr_was_pressed = false;

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
    int  menu_selection = MENU_RESUME;
    bool quit_requested = false;

    while (appletMainLoop()) {
        padUpdate(&pad);
        const u64 kDown = padGetButtonsDown(&pad);
        const u64 kUp   = padGetButtonsUp(&pad);
        const u64 kHeld = padGetButtons(&pad);

        if (menu_open) {
            // ─── Sub-screen branch: TOUCHES (Rust-driven) ───────────────
            // If the user picked "TOUCHES" earlier, Rust now owns the
            // input + rendering for the keymap editor. We just forward
            // joycon down-edges and ask it to draw.
            if (ruffle_touches_active()) {
                for (const auto& nb : MENU_NAV_BUTTONS) {
                    if (kDown & nb.mask) ruffle_touches_input(nb.name);
                }
                // If a binding was committed, refresh our runtime BINDINGS
                // so the change applies immediately (no need to REDEMARRER).
                if (ruffle_touches_consume_dirty()) {
                    populate_bindings_from_keymap();
                }
                ruffle_redraw_paused();
                ruffle_touches_draw();
                gl_context_swap();
                continue;
            }

            // ─── Pause main menu ────────────────────────────────────────
            // Edge-detected nav so a held D-pad doesn't scroll past every
            // entry. Press `-` again or `B` to dismiss (= Resume).
            if (kDown & (HidNpadButton_Up | HidNpadButton_StickLUp)) {
                menu_selection = (menu_selection + MENU_COUNT - 1) % MENU_COUNT;
            }
            if (kDown & (HidNpadButton_Down | HidNpadButton_StickLDown)) {
                menu_selection = (menu_selection + 1) % MENU_COUNT;
            }
            if (kDown & (HidNpadButton_Minus | HidNpadButton_B)) {
                menu_open = false;
                // Re-measure wall clock so the resumed frame doesn't
                // catch up by replaying the paused interval.
                last_tick = ruffle_tick_now();
                continue;
            }
            if (kDown & HidNpadButton_A) {
                switch (menu_selection) {
                case MENU_RESUME:
                    menu_open = false;
                    last_tick = ruffle_tick_now();
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
                        quit_requested = true;
                        break;
                    }
                    menu_open = false;
                    last_tick = ruffle_tick_now();
                    continue;
                }
                case MENU_QUIT:
                    quit_requested = true;
                    break;
                }
                if (quit_requested) break;
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
            continue;
        }

        // Keyboard-style buttons via edge detection.
        for (const auto& b : BINDINGS) {
            if (kDown & b.mask) ruffle_handle_key(b.key, true);
            if (kUp   & b.mask) ruffle_handle_key(b.key, false);
        }

        // Right analog stick → cursor movement.
        const HidAnalogStickState rs = padGetStickPos(&pad, 1);
        const float rsx = (float)rs.x;
        const float rsy = (float)rs.y;
        bool moved = false;
        if (rsx >  STICK_DEADZONE || rsx < -STICK_DEADZONE) {
            cursor_x += (rsx / STICK_MAX) * CURSOR_SPEED;
            moved = true;
        }
        if (rsy >  STICK_DEADZONE || rsy < -STICK_DEADZONE) {
            // Switch right stick Y is positive-up; screen Y is positive-down.
            cursor_y -= (rsy / STICK_MAX) * CURSOR_SPEED;
            moved = true;
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

        // Click: ZR button or touch tap. Track edges manually since these
        // two sources are heterogeneous.
        const bool zr_pressed = (kHeld & HidNpadButton_ZR) != 0;
        const bool click_pressed = zr_pressed || touch_pressed;
        const bool click_was_pressed = zr_was_pressed || touch_was_pressed;
        if (click_pressed && !click_was_pressed) {
            ruffle_handle_mouse_button(true);
        } else if (!click_pressed && click_was_pressed) {
            ruffle_handle_mouse_button(false);
        }
        zr_was_pressed = zr_pressed;
        touch_was_pressed = touch_pressed;

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
        // make Ruffle catch up by replaying 6 SWF frames in a row.
        if (dt_us > 100000ULL) dt_us = 100000ULL;

        ruffle_render_frame_dt(dt_us);
        gl_context_swap();

        if (quit_requested) break;
    }

    ruffle_shutdown();
    gl_context_shutdown();
    std::printf("worker: exiting\n"); std::fflush(stdout);
}

int main(int argc, char** argv) {
    (void)argc; (void)argv;

    socketInitializeDefault();
    nxlinkStdio();
    romfsInit();

    std::printf("flash-for-switch: starting\n"); std::fflush(stdout);

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

    // Spawn the Ruffle worker with a 32 MB stack. NULL stack_mem → libnx
    // allocates from heap, so we don't bloat .nro BSS. Priority 0x2C is the
    // libnx default; bumping it to 0x20 was tested 2026-05-25 soir and
    // produced no measurable FPS improvement (the Switch isn't loaded with
    // competing threads, so priority doesn't help). cpuid=-2 lets the
    // kernel pick the least-loaded core. The CpuBoostMode_FastLoad set
    // above is the perf lever that actually moved the needle.
    Thread t;
    Result rc = threadCreate(&t, worker_entry, nullptr,
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
