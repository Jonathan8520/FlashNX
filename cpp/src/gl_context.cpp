#include <switch.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <cstdio>

static EGLDisplay s_display = EGL_NO_DISPLAY;
static EGLContext s_context = EGL_NO_CONTEXT;
static EGLSurface s_surface = EGL_NO_SURFACE;

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
