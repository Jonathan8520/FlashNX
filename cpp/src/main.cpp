#include <switch.h>
#include <cstdio>
#include <cstdlib>
#include <cstdint>

#include "ruffle_bridge.h"

extern "C" bool gl_context_init(NWindow* win);
extern "C" void gl_context_shutdown(void);
extern "C" void gl_context_swap(void);

extern "C" void swf_picker_run(void);

extern "C" void ruffle_handle_key(int code, bool down);
extern "C" void ruffle_handle_mouse_move(int x, int y);
extern "C" void ruffle_handle_mouse_button(bool down);

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
};

// Joycon → key mapping table. + is reserved for "exit the app".
struct ButtonBinding {
    u64 mask;
    int key;
};
static const ButtonBinding BINDINGS[] = {
    { HidNpadButton_A,     SK_SPACE }, // jump in most Flash games
    { HidNpadButton_B,     SK_Z     }, // alt jump (Mario 63 uses Z)
    { HidNpadButton_X,     SK_X     }, // run / item / dive
    { HidNpadButton_Y,     SK_SHIFT }, // alt run
    { HidNpadButton_Minus, SK_ENTER }, // "Press Start" prompts
    { HidNpadButton_L,     SK_ESCAPE},
    { HidNpadButton_Left,  SK_LEFT  },
    { HidNpadButton_Right, SK_RIGHT },
    { HidNpadButton_Up,    SK_UP    },
    { HidNpadButton_Down,  SK_DOWN  },
    { HidNpadButton_StickLLeft,  SK_LEFT  },
    { HidNpadButton_StickLRight, SK_RIGHT },
    { HidNpadButton_StickLUp,    SK_UP    },
    { HidNpadButton_StickLDown,  SK_DOWN  },
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

    // Scan the SD card for a `.swf` and forward the path to Rust. Bypasses
    // the Rust read_dir bug on Horizon. If nothing is found, Rust's
    // hardcoded candidates / embedded fallback still apply.
    swf_picker_run();

    if (ruffle_init() != 0) {
        std::printf("ruffle_init failed\n"); std::fflush(stdout);
        gl_context_shutdown();
        return;
    }

    PadState pad;
    padConfigureInput(1, HidNpadStyleSet_NpadStandard);
    padInitializeDefault(&pad);

    // Touch screen state.
    hidInitializeTouchScreen();
    HidTouchScreenState touch_state = {0};
    bool touch_was_pressed = false;

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

    while (appletMainLoop()) {
        padUpdate(&pad);
        const u64 kDown = padGetButtonsDown(&pad);
        const u64 kUp   = padGetButtonsUp(&pad);
        const u64 kHeld = padGetButtons(&pad);

        if (kDown & HidNpadButton_Plus) break;

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
