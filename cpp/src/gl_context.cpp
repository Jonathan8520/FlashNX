#include <switch.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <cstdio>

static EGLDisplay s_display = EGL_NO_DISPLAY;
static EGLContext s_context = EGL_NO_CONTEXT;
static EGLSurface s_surface = EGL_NO_SURFACE;
// Kept so the surface can be recreated at a different size without rebuilding
// the context — see gl_context_resize.
static EGLConfig  s_config  = nullptr;
static NWindow*   s_win     = nullptr;

extern "C" bool gl_context_init(NWindow* win) {
    s_display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (s_display == EGL_NO_DISPLAY) {
        std::printf("eglGetDisplay failed\n");
        return false;
    }
    if (!eglInitialize(s_display, nullptr, nullptr)) {
        std::printf("eglInitialize failed: 0x%x\n", eglGetError());
        return false;
    }

    eglBindAPI(EGL_OPENGL_API);

    EGLConfig config;
    EGLint num_configs = 0;
    const EGLint config_attrs[] = {
        EGL_RED_SIZE,        8,
        EGL_GREEN_SIZE,      8,
        EGL_BLUE_SIZE,       8,
        EGL_ALPHA_SIZE,      8,
        EGL_DEPTH_SIZE,      24,
        EGL_STENCIL_SIZE,    8,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT,
        EGL_NONE
    };
    if (!eglChooseConfig(s_display, config_attrs, &config, 1, &num_configs) || num_configs == 0) {
        std::printf("eglChooseConfig failed: 0x%x\n", eglGetError());
        return false;
    }
    s_config = config;
    s_win = win;

    s_surface = eglCreateWindowSurface(s_display, config, win, nullptr);
    if (s_surface == EGL_NO_SURFACE) {
        std::printf("eglCreateWindowSurface failed: 0x%x\n", eglGetError());
        return false;
    }

    const EGLint context_attrs[] = {
        EGL_CONTEXT_OPENGL_PROFILE_MASK, EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,
        EGL_CONTEXT_MAJOR_VERSION,       4,
        EGL_CONTEXT_MINOR_VERSION,       3,
        EGL_NONE
    };
    s_context = eglCreateContext(s_display, config, EGL_NO_CONTEXT, context_attrs);
    if (s_context == EGL_NO_CONTEXT) {
        std::printf("eglCreateContext failed: 0x%x\n", eglGetError());
        return false;
    }

    if (!eglMakeCurrent(s_display, s_surface, s_surface, s_context)) {
        std::printf("eglMakeCurrent failed: 0x%x\n", eglGetError());
        return false;
    }

    return true;
}

extern "C" void gl_context_shutdown(void) {
    if (s_display != EGL_NO_DISPLAY) {
        eglMakeCurrent(s_display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
        if (s_context != EGL_NO_CONTEXT) {
            eglDestroyContext(s_display, s_context);
            s_context = EGL_NO_CONTEXT;
        }
        if (s_surface != EGL_NO_SURFACE) {
            eglDestroySurface(s_display, s_surface);
            s_surface = EGL_NO_SURFACE;
        }
        eglTerminate(s_display);
        s_display = EGL_NO_DISPLAY;
    }
}

extern "C" void gl_context_swap(void) {
    eglSwapBuffers(s_display, s_surface);
}

/// Change the INTERNAL render resolution by recreating just the window surface.
///
/// The display scaler upscales whatever the surface is to the panel, so a smaller
/// surface is a straight fill-rate saving. `nwindowSetDimensions` refuses to run
/// while buffers are registered, and eglCreateWindowSurface is what registers
/// them — hence destroy, resize, recreate.
///
/// Crucially the CONTEXT is preserved: in EGL, textures / shaders / VAOs belong to
/// the context, not the surface, so nothing GPU-side is lost and callers do not
/// have to rebuild their resources. Used to render the UI at panel resolution
/// while games (which are the actual fill load) run lower.
///
/// Returns false and leaves the old surface in place if anything fails.
extern "C" bool gl_context_resize(unsigned int w, unsigned int h) {
    if (s_display == EGL_NO_DISPLAY || s_context == EGL_NO_CONTEXT || !s_win) {
        return false;
    }
    // Release the surface before touching the window's dimensions.
    eglMakeCurrent(s_display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    if (s_surface != EGL_NO_SURFACE) {
        eglDestroySurface(s_display, s_surface);
        s_surface = EGL_NO_SURFACE;
    }
    Result rc = nwindowSetDimensions(s_win, w, h);
    if (R_FAILED(rc)) {
        std::printf("gl_context_resize: nwindowSetDimensions(%u,%u) failed 0x%x\n", w, h, rc);
    }
    s_surface = eglCreateWindowSurface(s_display, s_config, s_win, nullptr);
    if (s_surface == EGL_NO_SURFACE) {
        std::printf("gl_context_resize: eglCreateWindowSurface failed 0x%x\n", eglGetError());
        return false;
    }
    if (!eglMakeCurrent(s_display, s_surface, s_surface, s_context)) {
        std::printf("gl_context_resize: eglMakeCurrent failed 0x%x\n", eglGetError());
        return false;
    }
    std::printf("gl_context_resize: internal resolution now %ux%u\n", w, h);
    std::fflush(stdout);
    return true;
}
