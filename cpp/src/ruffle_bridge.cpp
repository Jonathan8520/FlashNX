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

// Rust stdlib on target_os=horizon calls libc::getrandom for HashMap key
// seeding (hash-flooding mitigation). Newlib has no such symbol. Stub it
// with a xorshift LCG — HashMap doesn't need crypto-strength entropy, and
// our SWF inputs aren't adversarial. Phase 2 can wire this to libnx's
// `csrngGetRandomBytes` for real entropy.
extern "C" long getrandom(void* buf, std::size_t buflen, unsigned int /*flags*/) {
    static uint64_t state = 0xC0FFEE12345ULL;
    auto* out = static_cast<uint8_t*>(buf);
    for (std::size_t i = 0; i < buflen; ++i) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out[i] = static_cast<uint8_t>(state * 0x2545F4914F6CDD1DULL >> 32);
    }
    return static_cast<long>(buflen);
}
