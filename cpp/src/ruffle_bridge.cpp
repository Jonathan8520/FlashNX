#include "ruffle_bridge.h"

#include <cstdio>
#include <switch.h>

// Bridge between C++ frontend and the Rust staticlib.
// The Rust side exports ruffle_init / ruffle_render_frame / ruffle_shutdown
// with #[no_mangle] extern "C" linkage; the linker resolves them when
// libruffle_switch.a is pulled in.
//
// This translation unit also provides callbacks the Rust side calls back
// into — printf-style logging via nxlink so panics/shader errors are visible.

extern "C" void ruffle_log_cstr(const char* msg) {
    if (msg) {
        std::fputs(msg, stdout);
        // Force-flush so a subsequent abort()/panic doesn't drop the message
        // in the stdout buffer. Costs ~µs per log call, negligible vs the
        // network round-trip to nxlink.
        std::fflush(stdout);
    }
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

// Newlib (devkitPro) ships no `sysconf`. jpeg_decoder's multithreaded worker
// calls it via std::thread::available_parallelism() to size its mpsc pool.
// Always report 1 CPU so the worker falls back to its single-threaded path.
// Any other query (page size, etc.) returns -1 / EINVAL semantics implicitly.
extern "C" long sysconf(int name) {
    // _SC_NPROCESSORS_ONLN is 84 on glibc, but newlib doesn't define it.
    // Returning 1 unconditionally is safe: the only meaningful caller in our
    // dep graph is jpeg_decoder asking for parallelism.
    (void)name;
    return 1;
}

// ─── Diagnostic FFI ──────────────────────────────────────────────────────────
// Used by Rust to measure decode times and watch RAM headroom so we can
// pin down the Mario-63 sprite-cascade crash empirically rather than guess.

extern "C" int ruffle_query_ram(uint64_t* used_out, uint64_t* total_out) {
    if (!used_out || !total_out) return -1;
    uint64_t used = 0, total = 0;
    Result r1 = svcGetInfo(&used,  InfoType_UsedMemorySize,  CUR_PROCESS_HANDLE, 0);
    Result r2 = svcGetInfo(&total, InfoType_TotalMemorySize, CUR_PROCESS_HANDLE, 0);
    if (R_FAILED(r1) || R_FAILED(r2)) return -1;
    *used_out  = used;
    *total_out = total;
    return 0;
}

extern "C" uint64_t ruffle_tick_now(void) {
    return armGetSystemTick();
}

extern "C" uint64_t ruffle_tick_freq(void) {
    return armGetSystemTickFreq();
}
