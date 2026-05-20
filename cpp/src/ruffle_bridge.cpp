#include "ruffle_bridge.h"

#include <cstdio>

// Bridge between C++ frontend and the Rust staticlib.
// The Rust side exports ruffle_init / ruffle_render_frame / ruffle_shutdown
// with #[no_mangle] extern "C" linkage; the linker resolves them when
// libruffle_switch.a is pulled in.
//
// This translation unit also provides callbacks the Rust side calls back
// into — printf-style logging via nxlink so panics/shader errors are visible.

extern "C" void ruffle_log_cstr(const char* msg) {
    if (msg) std::fputs(msg, stdout);
}
