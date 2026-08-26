#include "ruffle_bridge.h"

#include <cstdio>
#include <cstring>
#include <cstdlib>
#include <malloc.h>
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

// Start a fresh window. Called when a game boots, so the tail a bug report
// carries is that ONE game's session and not a blend of everything played since
// the launcher started.
extern "C" void ruffle_log_ring_reset(void) {
    mutexLock(&g_ring_mutex);
    g_ring_len = 0;
    g_ring_pos = 0;
    g_prev[0] = '\0';
    g_prev_rep = 0;
    g_last_hb[0] = '\0';
    mutexUnlock(&g_ring_mutex);
}

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

// Bytes currently handed out by malloc, which is what a Rust allocation failure
// actually runs out of. `ruffle_query_ram` cannot answer that: it reports the
// heap the crt0 RESERVED (a flat 3185/3189 MB all session), so an abort like
// "memory allocation of 3744000 bytes failed" reads as if there were 3 GB free.
// mallinfo's fields are `int`, so this saturates rather than wrapping past 2 GB.
extern "C" uint64_t ruffle_heap_used(void) {
    struct mallinfo mi = mallinfo();
    return mi.uordblks < 0 ? UINT64_C(0x7FFFFFFF) : (uint64_t)mi.uordblks;
}

// How many bytes malloc will actually hand out, found by asking until it says
// no. Everything is freed again before returning, so this only costs the time
// of the probe (~ms) and tells us the one number every OOM investigation so far
// has had to guess: `ruffle_query_ram` reports the RESERVED heap (a flat
// 3185/3189 MB), while allocations were observed failing around 1.07 GB.
//
// Chunked rather than one big ask, because a single huge malloc can fail for
// want of one contiguous run while plenty of total memory remains — the sum of
// the chunks is the number that matters to us, since our own consumers (arenas,
// atlases, decoded frames) allocate in pieces of a few MB.
extern "C" uint64_t ruffle_probe_heap_ceiling(uint64_t chunk_bytes, uint64_t* biggest_single_out) {
    static void* blocks[4096];
    size_t n = 0;
    uint64_t total = 0;
    while (n < sizeof(blocks) / sizeof(blocks[0])) {
        void* p = std::malloc((size_t)chunk_bytes);
        if (!p) break;
        blocks[n++] = p;
        total += chunk_bytes;
    }
    for (size_t i = 0; i < n; ++i) std::free(blocks[i]);
    // Then the largest SINGLE block, halving from the total until one lands.
    if (biggest_single_out) {
        uint64_t want = total > 0 ? total : chunk_bytes;
        uint64_t best = 0;
        while (want >= 1024 * 1024) {
            void* p = std::malloc((size_t)want);
            if (p) {
                std::free(p);
                best = want;
                break;
            }
            want /= 2;
        }
        *biggest_single_out = best;
    }
    return total;
}

extern "C" uint64_t ruffle_tick_now(void) {
    return armGetSystemTick();
}

extern "C" uint64_t ruffle_tick_freq(void) {
    return armGetSystemTickFreq();
}
// Actual current CPU and GPU clocks in Hz, straight from the clkrst sysmodule
// (firmware 8.0+), so the heartbeat reports what the hardware IS doing rather
// than what we asked for. `gpu=` matters as much as `cpu=`: the power mode only
// ever touches the CPU, so a GPU reading that is not the OS default would mean
// something else moved it. Both are defined with the clock code below, sharing
// its one lazy clkrst init: two separate inits meant the diagnostic could go
// dark at the same instant the feature did, for the same reason, and read as
// two unrelated problems.
extern "C" uint32_t ruffle_cpu_clock_hz(void);
extern "C" uint32_t ruffle_gpu_clock_hz(void);

// 1 when docked (AppletOperationMode_Console), 0 handheld. Pairs with the CPU
// clock so a low clock can be read as "handheld + boost dropped" vs "expected".
extern "C" int ruffle_is_docked(void) {
    return appletGetOperationMode() == AppletOperationMode_Console ? 1 : 0;
}

// ─── CPU power mode ───────────────────────────────────────────────────────
//
// Two modes, and only the CPU is ever touched:
//
//   0  NORMAL   the OS profile, untouched (1020 MHz CPU / 307.2 MHz GPU handheld)
//   1  HIGH     CPU 1785 MHz, GPU left exactly where the OS put it
//
// Why only the CPU. The one A/B in the corpus that is same-session, same-binary
// and same-scene is temp/maj2.txt + temp/power.txt (Papa Louie 3, dc/win pinned
// at 4680, controller idle), both post small-object-cache:
//
//     NORMAL  1020/307.2   15.33 fps   tick 59.68 ms   render 3.94 ms
//     BOTH    1785/768     25.12 fps   tick 36.06 ms   render 2.87 ms
//     CPU     1785/307.2   25.15 fps   tick 36.04 ms   render 2.83 ms
//
// CPU-only and CPU+GPU are indistinguishable: 0.1% of framerate, inside the
// window-to-window spread. The GPU half buys nothing measurable and costs
// nothing measurable. Do not write "we measured it and it was worse"; an
// earlier revision of this comment did, on figures that turned out to compare
// two different binaries, and that does not survive a check.
//
// That the gain is the clock and not the scene: the tick ratio 59.68/36.04 =
// 1.656 tracks the clock ratio 1785/1020 = 1.750 to within 5%. Mario 63 gives
// 18.30 -> 27.30 fps, tick ratio 1.479, but that pair is cross-session and
// cross-binary, so treat 1.49x as a floor.
//
// Why not the GPU too. It buys nothing, so it is all downside: 768 MHz GPU on
// battery in handheld is refused by sys-clk (460.8 Erista, 614.4 Mariko, 768
// only on a charger) and by every fork, and it is the only clock the community
// consistently reports hitting a wall on. The CPU rail is the cheap one.
//
// Read that narrowly: this code never WRITES the GPU, it does not refuse to
// coexist with a GPU the OS itself raised. Dock the console while HIGH is on
// and apm sets 768 on its own, then the periodic check puts our 1785 back, and
// the console sits at 1785 + 768 for as long as it stays docked. Measured
// 2026-08-26 (`temp/test2_trace.log`, 16 heartbeats): skin FELL to 36.9-38.2C
// there, against 41-42C handheld, because docked means mains power and a fan
// that is no longer pinned at 60%. It is the coolest state in that whole
// session. Docking must not silently cancel what the player asked for, so this
// is deliberate, not an oversight.
//
// What is true about 1785, and what is not. Upstream sys-clk caps no CPU rate
// in any profile. sys-clk-OC pins CPU_SAFE_MAX to exactly 1785 on Erista, in
// its *safe* tier, in every profile including handheld-on-battery. But Horizon
// OC does clamp it: hoc-clk/sysmodule/src/mgr/clock_manager.cpp:136-141 returns
// 1581000000 for handheld-on-battery on Erista. 1581 is the Erista DFLL tbreak
// (the same constant is used for SetDfllTunings in that file), and the same
// project publishes "Max Safe Clocks on Battery: CPU 1785 MHz" in its own
// guide, so the clamp is at least as likely to be about undervolt tuning above
// the breakpoint as about 1785 being unsafe. Unexplained by any commit or
// issue. Do not write "capped by nobody"; that was wrong.
//
// Nintendo ships 1785 in exactly two of its sixteen performance configurations,
// 0x92220009 and 0x9222000A, and both pair it with the GPU throttled to 76.8.
// (1785, 307.2) is in no Horizon profile at all. The honest reading, which is
// still favourable: a high CPU with a floored GPU is a shipped, sanctioned
// shape, and this adds 230 MHz on the cheap rail. It is not "inside a Nintendo
// profile".
//
// What this does NOT protect against, and cannot. Handheld thermal response on
// Horizon is not throttling, it is SLEEP: skin below 58C clears the timers,
// 58-61C arms a 60 s timer, 61-63C arms a 10 s timer, 63C sleeps at once, and
// SoC or PCB at 84C sleeps at once in either mode. The handheld fan table pins
// at 60% from 53C. So the risk here is not damage, it is the console sleeping
// mid-game and the player losing progress. Horizon IS the thermal guard, the
// `tc` service has no clock command at all, and clkrst cannot defeat it.
//
// The longest raised-clock run ever captured by this project is 121 s, roughly
// 7% of the way to a thermal plateau, with no sleep, no HOME and no dock in it.
// "No drift observed" therefore means "not measured". flashnx_skin_temp_mc and
// friends below exist to close that, and a guard belongs here only once the
// curve is known: a threshold nobody has calibrated is a mechanism that can
// oscillate and drop the clock for no reason.

static bool     s_clk_inited   = false;
static bool     s_psm_inited   = false;
static bool     s_tc_inited    = false;
static uint32_t s_clk_orig_cpu = 0;    // rate to come back to, latched on raise
static int      s_clk_mode     = 0;    // the mode we WANT
static bool     s_clk_applied  = true; // whether the hardware agrees with it
static uint32_t s_clk_drift    = 0;    // times the OS took the clock back
static bool     s_clk_hooked   = false;
static AppletHookCookie s_clk_hook_cookie;

// 1785 MHz: an entry in the rate list the sysmodule advertises. Never invent a
// frequency; clk_set refuses anything the hardware does not list.
static const uint32_t CPU_HIGH_HZ = 1785000000u;
// The handheld and docked stock CPU rate, and the only sane fallback when the
// rate we read at raise time is not something to come back to.
static const uint32_t CPU_STOCK_HZ = 1020000000u;

// One clkrst session for the whole process. `ruffle_cpu_clock_hz` used to carry
// its own separate lazy init, which meant the diagnostic could go dark at the
// same moment the feature did, for the same reason, and look like two problems.
static bool clk_init_once(void) {
    if (!s_clk_inited) {
        if (R_FAILED(clkrstInitialize())) return false;
        s_clk_inited = true;
    }
    return true;
}

// Read one module's current rate. 0 on any failure.
static uint32_t clk_get(PcvModuleId mod) {
    if (!clk_init_once()) return 0;
    ClkrstSession s;
    // The trailing arg is the clkrst "unk" device-id; 3 is what Nintendo and
    // Atmosphere use here.
    if (R_FAILED(clkrstOpenSession(&s, mod, 3))) return 0;
    uint32_t hz = 0;
    Result rc = clkrstGetClockRate(&s, &hz);
    clkrstCloseSession(&s);
    return R_SUCCEEDED(rc) ? hz : 0;
}

// Set one module's rate.
//
// `require_listed` asks the sysmodule for its rate list first and refuses
// anything absent from it. Read that guarantee narrowly: the list is the raw
// DVFS table, it encodes no power, thermal, dock or charger policy, and it
// contains rates every fork blocks on battery. It stops us inventing a
// frequency the silicon does not have; it is not a safety argument on its own.
//
// It is asked for when RAISING and deliberately not when LOWERING. Applied to
// the way down it points backwards: a transient failure of
// clkrstGetPossibleClockRates, or a Range-typed list, would refuse the safe
// direction and leave the console at 1785.
static bool clk_set(PcvModuleId mod, uint32_t hz, const char* what,
                    bool verbose = true, bool require_listed = true) {
    if (!clk_init_once()) return false;
    ClkrstSession s;
    if (R_FAILED(clkrstOpenSession(&s, mod, 3))) {
        if (verbose) {
            std::printf("clocks: %s open failed\n", what);
            std::fflush(stdout);
        }
        return false;
    }
    if (require_listed) {
        uint32_t rates[32] = {0};
        PcvClockRatesListType type = PcvClockRatesListType_Discrete;
        int32_t count = 0;
        bool allowed = false;
        if (R_SUCCEEDED(clkrstGetPossibleClockRates(&s, rates, 32, &type, &count))) {
            // Only a discrete list can be searched entry by entry. A Range list
            // means rates[] is {min, max} and reading it as two discrete values
            // would be a guess; refuse instead.
            if (type == PcvClockRatesListType_Discrete) {
                for (int32_t i = 0; i < count && i < 32; ++i) {
                    if (rates[i] == hz) { allowed = true; break; }
                }
            }
        }
        if (!allowed) {
            if (verbose) {
                std::printf("clocks: %s %u Hz not offered (list type %d, %d entries), refused\n",
                            what, (unsigned)hz, (int)type, (int)count);
                std::fflush(stdout);
            }
            clkrstCloseSession(&s);
            return false;
        }
    }
    Result rc = clkrstSetClockRate(&s, hz);
    clkrstCloseSession(&s);
    if (verbose) {
        std::printf("clocks: %s -> %u Hz rc=0x%x\n", what, (unsigned)hz, rc);
        std::fflush(stdout);
    }
    return R_SUCCEEDED(rc);
}

static bool psm_init_once(void) {
    if (!s_psm_inited) {
        if (R_FAILED(psmInitialize())) return false;
        s_psm_inited = true;
    }
    return true;
}

// True when the pack is healthy enough to carry a raised clock.
//
// Frame this honestly: it is an anti-brownout guard, not a battery-health one.
// These states fire once the pack has already sagged under load, and all of the
// wear accumulates inside `Normal`. NoPerformanceBoost is literally documented
// as "performance boost modes cannot be entered", so entering one against it is
// the one case where this would be contradicting the OS outright.
static bool batt_allows_boost(void) {
    if (!psm_init_once()) return true;   // no reading is not a reason to refuse
    PsmBatteryVoltageState st;
    if (R_FAILED(psmGetBatteryVoltageState(&st))) return true;
    return st == PsmBatteryVoltageState_Normal;
}

// Drive the hardware towards the mode we want.
//
// Every path goes through here so there is exactly one place that decides what
// the CPU rate should be. `count_drift` is false for a deliberate mode change
// and true for the periodic check, so the counter only ever means "the OS moved
// it under us", never "we moved it ourselves".
static void clk_drive(bool verbose, bool count_drift) {
    if (!clk_init_once()) return;
    uint32_t want = (s_clk_mode == 1) ? CPU_HIGH_HZ : s_clk_orig_cpu;
    if (want == 0) { s_clk_applied = true; return; }   // nothing to restore to

    uint32_t cur = clk_get(PcvModuleId_CpuBus);
    if (cur == want) { s_clk_applied = true; return; }
    if (count_drift && cur != 0 && s_clk_applied) s_clk_drift++;

    const bool raising = (s_clk_mode == 1);
    s_clk_applied = clk_set(PcvModuleId_CpuBus, want,
                            raising ? "cpu" : "cpu(restore)", verbose,
                            /*require_listed=*/raising);
    if (!s_clk_applied && !raising && verbose) {
        // The console is still raised while the rest of the program believes it
        // is not. Say so, and leave the desired mode where it is so the
        // periodic check keeps retrying the way down.
        std::printf("clocks: RESTORE FAILED, cpu still at %u Hz, will retry\n", (unsigned)cur);
        std::fflush(stdout);
    }
}

// Called by the applet hook on dock, undock, performance-mode change and
// resume. apm reprograms pcv on each of those, so both the current rate and the
// rate latched to come back to are stale. Re-latch from the known stock rate
// rather than from a read, because a read taken after our own re-assert would
// hand back 1785 as the thing to restore.
static void clk_reapply_after_transition(void) {
    if (s_clk_orig_cpu == CPU_HIGH_HZ || s_clk_orig_cpu == 0) {
        s_clk_orig_cpu = CPU_STOCK_HZ;
    }
    s_clk_applied = true;   // whatever the OS just did is not our drift
    clk_drive(/*verbose=*/false, /*count_drift=*/false);
}

static void clk_applet_hook(AppletHookType type, void* param) {
    (void)param;
    switch (type) {
        case AppletHookType_OnOperationMode:
        case AppletHookType_OnPerformanceMode:
        case AppletHookType_OnResume:
            clk_reapply_after_transition();
            break;
        case AppletHookType_OnFocusState:
            // Give the clock back while we are not the foreground app, and take
            // it again on the way back. Three of the four homebrews that ship a
            // CPU raise do exactly this. Horizon resets the rate on suspend
            // anyway; this covers the window before that, and the case where we
            // keep running without focus.
            if (appletGetFocusState() == AppletFocusState_InFocus) {
                clk_reapply_after_transition();
            } else if (s_clk_mode == 1) {
                uint32_t back = (s_clk_orig_cpu && s_clk_orig_cpu != CPU_HIGH_HZ)
                              ? s_clk_orig_cpu : CPU_STOCK_HZ;
                clk_set(PcvModuleId_CpuBus, back, "cpu(unfocus)", false,
                        /*require_listed=*/false);
                s_clk_applied = false;   // the periodic check will take it back
            }
            break;
        default:
            break;
    }
}

// 0 = leave the OS alone, 1 = CPU 1785 MHz. Returns the mode actually in force,
// which is not always the one asked for: the caller is a menu row and must not
// print HIGH while the console sits at 1020.
extern "C" int flashnx_set_clock_mode(int mode) {
    if (!clk_init_once()) {
        std::printf("clocks: clkrst unavailable, staying on OS defaults\n");
        std::fflush(stdout);
        return 0;
    }
    if (!s_clk_hooked) {
        appletHook(&s_clk_hook_cookie, clk_applet_hook, nullptr);
        s_clk_hooked = true;
    }
    mode = (mode == 1) ? 1 : 0;
    if (mode == s_clk_mode && s_clk_applied) return s_clk_mode;

    if (mode == 1) {
        if (!batt_allows_boost()) {
            std::printf("clocks: battery not in Normal voltage state, refusing to raise\n");
            std::fflush(stdout);
            return s_clk_mode;
        }
        // Latch the rate to come back to at the moment of raising, never once
        // per process: a boot-time latch would freeze whatever apm happened to
        // have raised at an uncontrolled instant and write it back for the rest
        // of the session.
        //
        // But never latch 1785 itself. An aborted download leaves
        // appletSetCpuBoostMode(FastLoad) stuck (see net.cpp), and latching its
        // boosted rate would make "restore" write 1785 back and then stop
        // watching, leaving the console raised in the library with nothing
        // re-asserting anything.
        // Clear any apm boost ONCE, here, on the way up, and READ THE RATE
        // BEFORE clearing it.
        //
        // The order matters and used to be wrong. Clearing first and reading
        // second made the `orig == CPU_HIGH_HZ` guard below unreachable by
        // construction: apm had already been told Normal two lines earlier, so
        // the read could never come back boosted, and a guard that cannot fire
        // is not a guard. Reading first turns it into a permanent instrument
        // that names what was actually held, at the cost of one extra IPC on a
        // path taken once per game.
        //
        // What this is NOT for, despite what the old comment here and the one
        // at main.cpp said: an aborted download. That was folklore with a
        // traceable origin. Commit 9a61fbc (2026-06-14) added the FastLoad
        // raise and its drop in the same diff; the 30-frame apm loop predates
        // it (5060196, 2026-05-31) and re-asserted FastLoad, for apm's own
        // forced revocation, nothing to do with transfers. 4da02ac
        // (2026-07-30) flipped that line and rewrote the comment around it as
        // "an aborted transfer COULD leave it raised", and the "could" became
        // "can" in later copies with no observation behind it. Every exit from
        // a transfer in net.cpp runs multi_cleanup, whose FIRST statement drops
        // the boost, and Screen::DistantDownloading accepts only B.
        //
        // What it IS for: an apm/pcv race, and a resident overclocking
        // sysmodule (sys-clk, Switch-OC-Suite), which is a real user setup.
        uint32_t before_apm = clk_get(PcvModuleId_CpuBus);
        appletSetCpuBoostMode(ApmCpuBoostMode_Normal);

        uint32_t orig = clk_get(PcvModuleId_CpuBus);
        if (orig == 0) {
            std::printf("clocks: cannot read the current CPU rate, staying on OS defaults\n");
            std::fflush(stdout);
            return s_clk_mode;
        }
        if (orig == CPU_HIGH_HZ) {
            std::printf("clocks: cpu already at %u Hz (stuck boost?), will restore to %u\n",
                        (unsigned)orig, (unsigned)CPU_STOCK_HZ);
            orig = CPU_STOCK_HZ;
        }
        s_clk_orig_cpu = orig;
        s_clk_mode = 1;
        s_clk_applied = true;             // do not count this change as drift
        clk_drive(/*verbose=*/true, /*count_drift=*/false);
        if (!s_clk_applied) {             // the raise itself failed
            s_clk_mode = 0;
            std::printf("clocks: raise failed, staying NORMAL\n");
        } else {
            // Two values on purpose. They differ only when something else was
            // holding the CPU up and apm let go of it synchronously, which is
            // the observation the old single-value line could never produce.
            std::printf("clocks: power mode HIGH (was %u Hz, %u before apm purge)\n",
                        (unsigned)orig, (unsigned)before_apm);
        }
    } else {
        s_clk_mode = 0;
        s_clk_applied = true;
        clk_drive(/*verbose=*/true, /*count_drift=*/false);
        // s_clk_applied is now the truth about the hardware. A failed restore
        // deliberately does NOT flip the desired mode back to 1: NORMAL is what
        // is wanted, and leaving it there is what makes the periodic check keep
        // retrying the way down instead of re-raising.
        std::printf("clocks: power mode NORMAL%s\n",
                    s_clk_applied ? "" : " (WRITE FAILED, still raised)");
    }
    std::fflush(stdout);
    return s_clk_mode;
}

// The mode actually in force. Reads as NORMAL whenever the hardware does not
// agree with what was asked for, so a menu row can never claim a raise that did
// not happen.
extern "C" int flashnx_clock_mode(void) {
    return (s_clk_mode == 1 && s_clk_applied) ? 1 : 0;
}

// Times the OS took the clock back from under us, published in the heartbeat.
// Until this existed the revocation question could not be answered: the
// re-assert is deliberately silent, so a revoke followed by a repair leaves no
// trace at all. apm force-revoked CpuBoostMode(FastLoad) after 25-30 s of
// sustained load in 2026-05, so the same had to be assumed of clkrst until this
// counter said otherwise. It has since read 0 over 121 s and 115 s of
// continuous hold, with no sleep, no HOME and no dock in either window.
extern "C" uint32_t flashnx_clock_drift(void) { return s_clk_drift; }

// Silent, cheap periodic check for the in-game loop. Waking from sleep or
// returning from HOME resets the rate, so a raised clock has to be re-applied,
// and a failed restore has to be retried. Nothing here prints, and a write only
// happens when the rate actually disagrees, because this runs inside the
// capture it is meant to serve.
//
// Confirmed on hardware 2026-08-26 (`temp/test2_trace.log`), after four
// sessions in which `drift` never left 0 and this looked like dead code. It is
// not: returning from HOME took the rate back (drift 0 -> 1, a 26 s window
// where the others are 3.8 s) and so did waking from sleep (1 -> 2, a 200 s
// window). Both were repaired inside one check.
//
// What does NOT take it: a dock transition on its own. The undock, seen alone
// and cleanly, produced no drift at all, which fits, since CpuBus is 1020 in
// both the handheld and the docked profile and apm has nothing to rewrite. It
// is suspension that costs the rate, not the dock.
//
// Note the cadence is 30 frames, not 0.5 s: at the 25-27 fps these games run at
// it is 1.1-1.2 s, and 2.6 s in the worst heartbeat of the corpus.
// Returns 1 while it is managing the rate (a raise to hold, or a restore that
// failed and has to be retried), 0 when it is idle. The caller uses that to
// decide whether apm may be told "Normal": telling apm anything while clkrst
// holds a rate makes both undecidable.
extern "C" int flashnx_clocks_reassert(void) {
    // Not the foreground app: leave the clock alone. The focus hook has just
    // handed it back, and without this the two would fight, the hook lowering
    // it and this raising it again a second later for as long as the HOME menu
    // stayed open. Still reports 1 so apm is not poked either.
    if (appletGetFocusState() != AppletFocusState_InFocus) {
        return (s_clk_mode == 1 || !s_clk_applied) ? 1 : 0;
    }
    if (s_clk_mode == 1 && !batt_allows_boost()) {
        std::printf("clocks: battery left Normal voltage state, dropping to NORMAL\n");
        std::fflush(stdout);
        flashnx_set_clock_mode(0);
        return s_clk_applied ? 0 : 1;
    }
    if (s_clk_mode == 0 && s_clk_applied) return 0;   // nothing to hold
    clk_drive(/*verbose=*/false, /*count_drift=*/true);
    // A restore that has come through is the end of it: nothing left to hold.
    return (s_clk_mode == 1 || !s_clk_applied) ? 1 : 0;
}

// Put the console back where we found it, on the way out of a game, before the
// .nro exits, and from the crash paths.
//
// The crash paths matter and used to be waved away here with "Horizon resets
// clkrst state when the process dies". That is UNVERIFIED, and the mechanism it
// leans on is contradicted by our own instrument: clk_set closes its session
// immediately and every later read opens a fresh one and still sees 1785, so
// closing a session plainly does not hand the rate back. What actually restores
// on a stock console is apm reprogramming pcv at its next performance
// transition, which is precisely why sys-clk has to run resident and reapply on
// a timer. Until someone forces a panic and reads the clock from another
// homebrew, assume nothing, and call this from the handlers.
extern "C" void flashnx_clocks_restore(void) {
    // apm FIRST, and unconditionally. flashnx_set_clock_mode(0) early-returns
    // when the mode already reads NORMAL and is applied, so on that path it
    // touches nothing at all. That left a real hole neither half of the audit
    // saw on its own, because it sits exactly between them: die during a
    // download and FastLoad stays raised whichever net runs, including the one
    // validated on hardware. net_shutdown (net.cpp) is the function that would
    // have dropped it, and it has no caller anywhere in the project.
    appletSetCpuBoostMode(ApmCpuBoostMode_Normal);
    flashnx_set_clock_mode(0);
}

// Last thing this process runs, on EVERY exit.
//
// libnx calls the weak `userAppExit` from `__appExit` before it tears anything
// down, so sm and pcv are still alive here. That matters because it is the only
// hook that covers the death this app actually dies of: a Rust allocation
// failure does not panic. The language calls handle_alloc_error, which reaches
// default_alloc_error_hook, which runs NO user code, then abort(), whose
// raise() is a no-op on Switch (ENOSYS), then _exit -> __libnx_exit ->
// svcExitProcess. No CPU exception, so __libnx_exception_handler never fires;
// no unwind, so the Rust panic hook never fires. Five deaths of exactly this
// shape are already in this project's own logs (temp/slab2.txt, slab.txt,
// maj3.txt, ab.txt, region.txt), and the slab2 one was running overclocked.
//
// No printf: on the OOM path the heap is gone. Raw clkrst rather than
// flashnx_set_clock_mode, which allocates nothing but does log.
//
// What it does NOT cover: svcBreak and diagAbortWithResult. Nothing can, and
// libnx uses them internally, so a libnx failure under memory pressure still
// dies with no exit path at all.
extern "C" void userAppExit(void) {
    appletSetCpuBoostMode(ApmCpuBoostMode_Normal);
    if (s_clk_orig_cpu != 0 && s_clk_orig_cpu != CPU_HIGH_HZ) {
        ClkrstSession s;
        if (R_SUCCEEDED(clkrstOpenSession(&s, PcvModuleId_CpuBus, 3))) {
            clkrstSetClockRate(&s, s_clk_orig_cpu);
            clkrstCloseSession(&s);
        }
    }
}

// ─── Thermal and battery instrumentation ──────────────────────────────────
//
// Read-only, published in the heartbeat. This is the whole point of shipping
// the setting and the probe in the same build: the 30-minute soak that nobody
// has run yet is what decides whether a guard is needed and where its threshold
// goes.
//
// Compare skin against the numbers the OS itself uses: 53000 (fan pinned at
// 60%), 58000 (60 s sleep timer), 61000 (10 s timer), 63000 (immediate sleep).
// tcGetSkinTemperatureMilliC is guarded only by hosversionBefore(5,0,0) and has
// no removal bound, unlike tsGetTemperature (dead >= 17.0.0) and
// tsGetTemperatureMilliC (dead >= 14.0.0), which is why neither is used here.
//
// Never touch tcDisableFanControl. It is the one function in this whole family
// that libnx bothers to put a @warning on, and it is the only way an app can
// actually damage the console.
extern "C" int32_t flashnx_skin_temp_mc(void) {
    if (!s_tc_inited) {
        if (R_FAILED(tcInitialize())) return 0;
        s_tc_inited = true;
    }
    s32 mc = 0;
    return R_SUCCEEDED(tcGetSkinTemperatureMilliC(&mc)) ? (int32_t)mc : 0;
}

// Battery cell temperature in milli-C. The second axis, and the one that
// matters for the pack rather than for the sleep decision.
extern "C" int32_t flashnx_batt_temp_mc(void) {
    if (!psm_init_once()) return 0;
    PsmBatteryChargeInfoFields f;
    if (R_FAILED(psmGetBatteryChargeInfoFields(&f))) return 0;
    return (int32_t)f.temperature_celcius;
}

// Charge in per-mille, i.e. 1000 = full.
//
// This is the only route to a power figure: psm exposes no instantaneous
// current (PsmBatteryChargeInfoFields carries a voltage and three current
// *limits*), and max17050PowerNow is an I2C helper inside a sysmodule with its
// own NPDM, out of reach of an NRO. Two readings twenty minutes apart in each
// mode on the same SWF give %/h, and with ~16 Wh nominal, average watts.
extern "C" int32_t flashnx_batt_permille(void) {
    if (!psm_init_once()) return 0;
    double pct = 0.0;
    if (R_FAILED(psmGetRawBatteryChargePercentage(&pct))) return 0;
    if (pct < 0.0) pct = 0.0;
    if (pct > 100.0) pct = 100.0;
    return (int32_t)(pct * 10.0 + 0.5);
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

extern "C" uint32_t ruffle_cpu_clock_hz(void) { return clk_get(PcvModuleId_CpuBus); }
extern "C" uint32_t ruffle_gpu_clock_hz(void) { return clk_get(PcvModuleId_GPU); }
