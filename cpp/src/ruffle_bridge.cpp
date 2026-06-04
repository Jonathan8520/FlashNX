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

// Map the console's system language to FlashNX's locale index:
//   0 = English, 1 = French, 2 = Spanish, 3 = Russian, -1 = unsupported.
// Called once from loc::init() when no settings.json language is stored.
extern "C" int ruffle_detect_system_lang(void) {
    if (R_FAILED(setInitialize())) return -1;
    u64 lcode = 0;
    SetLanguage lang = SetLanguage_ENUS;
    int idx = -1;
    if (R_SUCCEEDED(setGetSystemLanguage(&lcode)) &&
        R_SUCCEEDED(setMakeLanguage(lcode, &lang))) {
        switch (lang) {
            case SetLanguage_ENUS:
            case SetLanguage_ENGB:  idx = 0; break;
            case SetLanguage_FR:
            case SetLanguage_FRCA:  idx = 1; break;
            case SetLanguage_ES:
            case SetLanguage_ES419: idx = 2; break;
            case SetLanguage_RU:    idx = 3; break;
            default:                idx = -1; break;
        }
    }
    setExit();
    return idx;
}

// Called from the Rust panic hook. Mirrors the panic message to a file on
// the SD card AND to nxlink stdout, then blocks briefly so the kernel's TCP
// buffer for the nxlink socket has time to drain before Rust's `panic = abort`
// short-circuits the process. Without the sleep, a previous crash logged
// PANIC to stdout but the bytes never made it across the wire — we saw
// `socket error 0x0 on poll` with no panic info in the host-side log.
extern "C" void ruffle_crash_dump(const char* msg) {
    if (!msg) return;
    // 1. nxlink stdout (best-effort, may not flush before abort)
    std::fputs(msg, stdout);
    std::fflush(stdout);
    // 2. Persist to SD so we can read it post-mortem even if the socket
    // was truncated. We append so successive panics in one boot accumulate.
    FILE* f = std::fopen("sdmc:/switch/ruffle-crash.log", "a");
    if (f) {
        std::fputs(msg, f);
        std::fflush(f);
        std::fclose(f);
    }
    // 3. Give the kernel ~150 ms to push the nxlink TCP buffer to the host.
    // svcSleepThread takes nanoseconds. The actual abort() happens right
    // after this returns, so without the sleep the buffered bytes get lost.
    svcSleepThread(150 * 1000 * 1000);
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

// Actual current CPU clock in Hz, read from the clkrst sysmodule (firmware
// 8.0+). Lets the heartbeat CONFIRM whether CpuBoostMode(FastLoad) is really
// holding the A57 at its boosted 1785 MHz during heavy AVM1 scenes (the water
// lake) — i.e. whether there's any CPU headroom left, or the lag is just the
// interpreter maxing a stock-clocked core. Returns 0 if the service is
// unavailable. Service is opened lazily once and left open for the process
// lifetime (cleaned up on exit).
extern "C" uint32_t ruffle_cpu_clock_hz(void) {
    static bool inited = false;
    if (!inited) {
        if (R_FAILED(clkrstInitialize())) return 0;
        inited = true;
    }
    ClkrstSession session;
    // PcvModuleId_CpuBus = the Cortex-A57 cluster clock. The trailing arg is
    // the clkrst "unk" device-id (3 is what Nintendo/atmosphère use here).
    if (R_FAILED(clkrstOpenSession(&session, PcvModuleId_CpuBus, 3))) return 0;
    uint32_t hz = 0;
    Result rc = clkrstGetClockRate(&session, &hz);
    clkrstCloseSession(&session);
    return R_SUCCEEDED(rc) ? hz : 0;
}

// 1 when docked (AppletOperationMode_Console), 0 handheld. Pairs with the CPU
// clock so a low clock can be read as "handheld + boost dropped" vs "expected".
extern "C" int ruffle_is_docked(void) {
    return appletGetOperationMode() == AppletOperationMode_Console ? 1 : 0;
}

// 1 when running with the SMALL applet memory pool (~448-560 MB), 0 when we
// have the full title-takeover application heap (~3.2 GB). Ruffle + our
// backend need the full heap; in applet mode launching a SWF OOMs and falls
// back to the embedded red screen. The library UI uses this to show a clear
// "launch via title takeover" notice instead of that red screen. hbmenu's
// album-takeover runs us as a LibraryApplet; a forwarder / title takeover
// runs us as a (System)Application.
extern "C" int ruffle_is_applet_mode(void) {
    AppletType t = appletGetAppletType();
    if (t == AppletType_Application || t == AppletType_SystemApplication) {
        return 0;
    }
    return 1;
}

// Flush buffered `sdmc:` writes to the physical card. libnx's fsdev mount
// buffers writes; data written via newlib (Rust `std::fs`) does NOT hit the
// card until the device is committed or the process exits cleanly. Two
// instances of FlashNX (library-applet album-takeover vs title-takeover
// application) can otherwise observe divergent, uncommitted state — the
// classic symptom being the URL history reading empty in one mode and then
// getting overwritten. The Rust side calls this right after every write we
// want durable (URL history, settings, saves, keymap/meta sidecars).
// Returns 0 on success, -1 on failure (logged, never fatal).
extern "C" int flashnx_commit_sd(void) {
    Result rc = fsdevCommitDevice("sdmc");
    if (R_FAILED(rc)) {
        std::printf("flashnx_commit_sd: fsdevCommitDevice(sdmc) failed 0x%x\n", rc);
        std::fflush(stdout);
        return -1;
    }
    return 0;
}
