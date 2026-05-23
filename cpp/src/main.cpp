#include <switch.h>
#include <cstdio>
#include <cstdlib>
#include <cstdint>

#include "ruffle_bridge.h"

extern "C" bool gl_context_init(NWindow* win);
extern "C" void gl_context_shutdown(void);
extern "C" void gl_context_swap(void);

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

int main(int argc, char** argv) {
    (void)argc; (void)argv;

    socketInitializeDefault();
    nxlinkStdio();
    romfsInit();

    std::printf("flash-for-switch: starting\n");

    NWindow* win = nwindowGetDefault();
    if (!gl_context_init(win)) {
        std::printf("gl_context_init failed\n");
        socketExit();
        return EXIT_FAILURE;
    }

    if (ruffle_init() != 0) {
        std::printf("ruffle_init failed\n");
        gl_context_shutdown();
        socketExit();
        return EXIT_FAILURE;
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

        ruffle_render_frame();
        gl_context_swap();
    }

    ruffle_shutdown();
    gl_context_shutdown();
    romfsExit();
    socketExit();
    return EXIT_SUCCESS;
}
