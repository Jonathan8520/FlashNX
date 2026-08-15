#include "ruffle_bridge.h"

#include <cstdio>
#include <cstring>
#include <sys/stat.h>
#include <switch.h>

// Bridge between C++ frontend and the Rust staticlib.
// The Rust side exports ruffle_init / ruffle_render_frame / ruffle_shutdown
// with #[no_mangle] extern "C" linkage; the linker resolves them when
// libruffle_switch.a is pulled in.
//
// This translation unit also provides callbacks the Rust side calls back
// into — printf-style logging via nxlink so panics/shader errors are visible.

// ── Optional persistent trace to SD ───────────────────────────────────────
//
// Our logs normally go to nxlink stdout and nowhere else, which makes any
// OFFLINE bug undebuggable by construction: turning the WiFi off to reproduce
// also kills the only channel the logs travel on. That is exactly how the
// "no connection" crash stayed opaque — no nxlink, no crash log, no trace.
//
// When `sdmc:/switch/FlashNX/trace.on` exists, mirror every log line into
// `sdmc:/switch/flashnx-trace.log`, flushed line by line, so the LAST line in
// the file names the last thing that ran before the process died. Truncated
// at each boot, so the file always describes the most recent run.
//
// Gated on the marker so normal users pay nothing: an SD write per log line
// is far too expensive to leave on (each fsdev op is an IPC round trip).
// Checked once, on the first log call.
static FILE* g_trace = nullptr;
static bool  g_trace_checked = false;

static FILE* trace_file(void) {
    if (!g_trace_checked) {
        g_trace_checked = true;
        struct stat st;
        if (::stat("sdmc:/switch/FlashNX/trace.on", &st) == 0) {
            g_trace = std::fopen("sdmc:/switch/flashnx-trace.log", "w");
        }
    }
    return g_trace;
}

// ── Rolling in-memory tail of the log ─────────────────────────────────────
//
// The last few KB of everything that goes through this funnel, kept in RAM so
// the in-app bug report can carry it (see `crate::bugreport`). Memory, not the
// SD trace above: this one is ALWAYS on, and an SD write per log line would be
// far too expensive to leave enabled for every player.
//
// Worth having because Ruffle's own `warn!`/`error!` events come through here
// too (backend/tracing.rs pipes the tracing subscriber into this function), so
// the tail of a session is exactly the `[tr/WARN]` list for the game being
// reported — the first thing worth reading on a "this game renders wrong"
// report, and previously visible only over nxlink.
//
// A ring, so it can never grow: old lines fall off the back, which also means
// the boot-time library scan has aged out by the time anyone files a report.
namespace {
// 64 KB of a 3 GB heap. Sized by how far back a report needs to see, not by
// thrift: at 8 KB the window covered only the last few seconds of play, which
// on a bug noticed mid-session is just the walk back to the menu.
constexpr size_t LOG_RING_CAP = 64 * 1024;
char   g_ring[LOG_RING_CAP];
size_t g_ring_len = 0;  // valid bytes, saturates at LOG_RING_CAP
size_t g_ring_pos = 0;  // next write offset
Mutex  g_ring_mutex;    // zero-initialised == unlocked, per libnx

// Most recent per-frame heartbeat ("f1234: fps=... ram=..."), kept on its own
// instead of in the ring. One of these is genuine context for a "this game is
// broken" report — framerate, RAM, arena occupancy — but it repeats every N
// frames, so letting them accumulate would push out everything else.
char g_last_hb[512];

bool is_heartbeat(const char* msg) {
    return msg[0] == 'f' && std::strstr(msg, ": fps=") != nullptr;
}

// Lines kept OUT of the ring. Two reasons, both learned from a real report
// whose 6 KB of log turned out to be 100% telemetry and 0% diagnosis:
//
//  - Per-frame telemetry ("SLOW f...", the ·f tick) is ~470 bytes a line and
//    fires hardest on slow games, which are exactly the ones people report. It
//    flushed every Ruffle warning out of the ring before the report was sent,
//    and it is useless to a reader anyway.
//  - The library scan is one line per file on the card plus the saved import
//    addresses: no diagnostic value, and the most personal thing the log holds.
//    A report filed shortly after launch would have published the library.
//
// nxlink and the SD trace still receive all of it; only the ring skips it.
bool ring_excluded(const char* msg) {
    static const char* const PREFIXES[] = {
        "SLOW f",
        "\xc2\xb7" "f",  // "·f<frame>" tick, UTF-8 middle dot
        "library: added ",
        "library: history LOADED ",
        "library: history meta LOADED ",
    };
    for (const char* p : PREFIXES) {
        if (std::strncmp(msg, p, std::strlen(p)) == 0) return true;
    }
    return false;
}

// Previous line, for collapsing consecutive repeats. Bounded: a message longer
// than this is never deduplicated, which is deliberate. Long lines here are the
// sidecar warnings, and those differ only in a random suffix far into the
// string — comparing a truncated prefix would merge two genuinely different
// URLs. The lines that actually repeat ("root: frame 40/46", game traces) are
// all short.
char   g_prev[256];
size_t g_prev_rep = 0;

// Append raw bytes. Caller holds g_ring_mutex.
void ring_append_locked(const char* p, size_t n) {
    if (n > LOG_RING_CAP) { // keep the tail of an oversized single message
        p += n - LOG_RING_CAP;
        n = LOG_RING_CAP;
    }
    for (size_t i = 0; i < n; i++) {
        g_ring[g_ring_pos] = p[i];
        g_ring_pos = (g_ring_pos + 1) % LOG_RING_CAP;
    }
    g_ring_len = (g_ring_len + n > LOG_RING_CAP) ? LOG_RING_CAP : g_ring_len + n;
}

// Emit the pending "repeated N times" marker, if any. Caller holds the mutex.
void ring_flush_repeat_locked() {
    if (g_prev_rep == 0) return;
    char m[72];
    const int k = std::snprintf(m, sizeof(m),
                                "    (line above repeated %zu more times)\n", g_prev_rep);
    g_prev_rep = 0;
    if (k > 0) ring_append_locked(m, (size_t)k);
}

void ring_push(const char* msg) {
    if (is_heartbeat(msg)) {
        mutexLock(&g_ring_mutex);
        std::strncpy(g_last_hb, msg, sizeof(g_last_hb) - 1);
        g_last_hb[sizeof(g_last_hb) - 1] = '\0';
        mutexUnlock(&g_ring_mutex);
        return;
    }
    if (ring_excluded(msg)) return;
    const size_t n = std::strlen(msg);
    if (n == 0) return;
    mutexLock(&g_ring_mutex);
    // Collapse consecutive identical lines into one plus a count. Games trace
    // the same string every frame and our own per-frame notes repeat too, so
    // without this a handful of messages can own most of the window.
    const bool dedupable = n < sizeof(g_prev);
    if (dedupable && g_prev[0] != '\0' && std::strcmp(msg, g_prev) == 0) {
        g_prev_rep++;
        mutexUnlock(&g_ring_mutex);
        return;
    }
    ring_flush_repeat_locked();
    if (dedupable) {
        std::memcpy(g_prev, msg, n + 1);
    } else {
        g_prev[0] = '\0';
    }
    ring_append_locked(msg, n);
    mutexUnlock(&g_ring_mutex);
}

} // namespace

extern "C" int ruffle_log_tail(char* out, int cap) {
    if (!out || cap < 2) return 0;
    mutexLock(&g_ring_mutex);
    // A run of repeats may still be open; without this the last group of
    // collapsed lines would vanish from the report entirely.
    ring_flush_repeat_locked();
    // Latest heartbeat first, so the reader gets framerate and memory before
    // the log itself.
    size_t w = 0;
    const size_t hb = std::strlen(g_last_hb);
    if (hb > 0 && hb + 2 < (size_t)cap) {
        std::memcpy(out, g_last_hb, hb);
        w = hb;
        if (out[w - 1] != '\n') out[w++] = '\n';
    }
    const size_t room = (size_t)cap - 1 - w; // space left for the ring
    size_t want = g_ring_len;
    size_t start = (g_ring_pos + LOG_RING_CAP - want) % LOG_RING_CAP;
    if (want > room) {
        // Less room than ring: keep the NEWEST bytes.
        const size_t drop = want - room;
        start = (start + drop) % LOG_RING_CAP;
        want -= drop;
    }
    const size_t ring_at = w;
    for (size_t i = 0; i < want; i++) {
        out[w++] = g_ring[(start + i) % LOG_RING_CAP];
    }
    mutexUnlock(&g_ring_mutex);
    out[w] = '\0';
    // The ring cuts wherever it wrapped, so its first line is usually half a
    // line. Drop it rather than ship a fragment. Only the ring part is
    // realigned; the heartbeat above it is already whole.
    char* ring = out + ring_at;
    const size_t ring_len = w - ring_at;
    char* nl = (char*)std::memchr(ring, '\n', ring_len);
    if (nl && (size_t)(nl - ring) + 1 < ring_len) {
        const size_t skip = (size_t)(nl - ring) + 1;
        std::memmove(ring, ring + skip, ring_len - skip);
        w -= skip;
        out[w] = '\0';
    }
    return (int)w;
}

extern "C" void ruffle_log_cstr(const char* msg) {
    if (msg) {
        ring_push(msg);
        std::fputs(msg, stdout);
        // Force-flush so a subsequent abort()/panic doesn't drop the message
        // in the stdout buffer. Costs ~µs per log call, negligible vs the
        // network round-trip to nxlink.
        std::fflush(stdout);
        FILE* t = trace_file();
        if (t) {
            std::fputs(msg, t);
            std::fflush(t);
        }
    }
}

// Hand the Rust side a pointer to the console's SHARED system font for `kind`
// (a PlSharedFontType value; 1 = Chinese Simplified). plGetSharedFontByType
// returns DECRYPTED TTF/OTF bytes mapped into a shared-memory region that
// stays valid for the process lifetime, so Rust can borrow it as 'static and
// hand it straight to fontdue (no BFTTF deobfuscation needed). Used by
// backend/glyphs.rs to rasterize CJK glyphs the 5x7 bitmap font can't carry.
// Returns null (and *out_size = 0) if the pl service is unavailable.
extern "C" const unsigned char* ruffle_shared_font(int kind, unsigned int* out_size) {
    static bool pl_inited = false;
    if (!pl_inited) {
        if (R_FAILED(plInitialize(PlServiceType_User))) {
            if (out_size) *out_size = 0;
            return nullptr;
        }
        pl_inited = true;
    }
    PlFontData fd;
    if (R_FAILED(plGetSharedFontByType(&fd, (PlSharedFontType)kind))) {
        if (out_size) *out_size = 0;
        return nullptr;
    }
    if (out_size) *out_size = fd.size;
    return (const unsigned char*)fd.address;
}

// Map the console's system language to FlashNX's locale index:
//   0 = English, 1 = French, 2 = Spanish, 3 = Russian, 4 = German,
//   5 = Italian, 6 = Portuguese, 7 = Chinese, -1 = unsupported.
//   (All Chinese variants map to our Simplified UI; the user can switch.)
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
            case SetLanguage_DE:    idx = 4; break;
            case SetLanguage_IT:    idx = 5; break;
            case SetLanguage_PT:
            case SetLanguage_PTBR:  idx = 6; break;
            case SetLanguage_ZHCN:
            case SetLanguage_ZHHANS:
            case SetLanguage_ZHTW:
            case SetLanguage_ZHHANT: idx = 7; break;
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
