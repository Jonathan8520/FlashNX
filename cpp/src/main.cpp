#include <switch.h>
#include <cstdio>
#include <cstdlib>

#include "ruffle_bridge.h"

extern "C" bool gl_context_init(NWindow* win);
extern "C" void gl_context_shutdown(void);
extern "C" void gl_context_swap(void);

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

    while (appletMainLoop()) {
        padUpdate(&pad);
        const u64 kDown = padGetButtonsDown(&pad);
        if (kDown & HidNpadButton_Plus) break;

        ruffle_render_frame();
        gl_context_swap();
    }

    ruffle_shutdown();
    gl_context_shutdown();
    romfsExit();
    socketExit();
    return EXIT_SUCCESS;
}
