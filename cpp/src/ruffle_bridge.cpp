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
// Why only the CPU. Measured 2026-08-25 on the one clean three-way A/B in the
// corpus (temp/mesure.txt, Papa Louie 3, identical scene, dc/win pinned at
// 4680, controller idle):
//
//     NORMAL 1020/307.2   15.15 fps   tick 60.06 ms   render 4.47 ms
//     GPU    1020/460.8   14.89 fps   tick 61.01 ms   render 4.57 ms
//     BOTH   1785/768     24.45 fps   tick 36.53 ms   render 3.50 ms
//
// Raising the GPU alone costs 1.7% of the framerate, i.e. nothing, in the wrong
// direction. Of the 25.1 ms per frame that raising BOTH buys, 23.53 ms (93.7%)
// is tick. A CPU-only mode therefore keeps ~90% of the gain. On Mario 63 the
// `render` timer even follows the CPU clock rather than the GPU one (10.0 ms ->
// 5.74 ms, against 5.71 ms predicted by the CPU ratio alone), because most of
// what it measures is command submission, not the GPU.
//
// Why not the GPU too, for that last 10%. 1785 MHz CPU on battery in handheld
// is capped by nobody: not sys-clk, not sys-clk-OC, not Horizon OC, and no
// hardware failure has ever been attributed to it. 768 MHz GPU on battery in
// handheld is blocked by all of them on Erista (sys-clk caps the handheld GPU
// at 460.8 there, 614.4 on Mariko, and allows 768 only on a charger), and it is
// the configuration the recurring battery-swelling reports point at. Nintendo
// itself never pairs the two: in its own profile table 1785 always comes with
// the GPU throttled to 76.8, and 768 only ever appears docked with a 1020 CPU.
// Four percent of framerate does not buy console-model detection, charger
// detection, and a configuration every other project refuses.
//
// What this does NOT protect against. Handheld thermal response on Horizon is
// not throttling, it is SLEEP: skin at 58C arms a 60-second timer, 63C sleeps
// immediately, and the handheld fan table is capped near 60%. Nothing here
// reads a temperature. The longest sustained raised-clock run ever captured by
// this project is 118 seconds, which is nowhere near thermal steady state, so
// "no drift observed" means "not measured". A thermal guard belongs here later,
// reading tcGetSkinTemperatureMilliC in the 30-frame slot that already exists.

static bool     s_clk_inited   = false;
static uint32_t s_clk_orig_cpu = 0;   // captured fresh on every raise
static int      s_clk_mode     = 0;
static uint32_t s_clk_drift    = 0;   // times the OS took the clock back

// 1785 MHz: Nintendo's own boost clock, and an entry in the rate list the
// sysmodule advertises. Never invent a frequency; clk_set refuses anything the
// hardware does not list.
static const uint32_t CPU_HIGH_HZ = 1785000000u;

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

// Set one module's rate, but only to a value the sysmodule itself lists as
// possible.
//
// Read that guarantee narrowly: the list is the raw DVFS table. It encodes no
// power, thermal, dock or charger policy, and it contains rates every fork
// blocks on battery. It stops us inventing a frequency the silicon does not
// have; it is not a safety argument on its own. The safety argument here is
// that we only ever touch the CPU, and only to 1785.
static bool clk_set(PcvModuleId mod, uint32_t hz, const char* what, bool verbose = true) {
    if (!clk_init_once()) return false;
    ClkrstSession s;
    if (R_FAILED(clkrstOpenSession(&s, mod, 3))) {
        if (verbose) {
            std::printf("clocks: %s open failed\n", what);
            std::fflush(stdout);
        }
        return false;
    }
    uint32_t rates[32] = {0};
    PcvClockRatesListType type = PcvClockRatesListType_Discrete;
    int32_t count = 0;
    bool allowed = false;
    if (R_SUCCEEDED(clkrstGetPossibleClockRates(&s, rates, 32, &type, &count))) {
        // Only a discrete list can be searched entry by entry. A Range list
        // means rates[] is {min, max} and the old code would have read it as
        // two discrete values; refuse rather than guess.
        if (type == PcvClockRatesListType_Discrete) {
            for (int32_t i = 0; i < count && i < 32; ++i) {
                if (rates[i] == hz) { allowed = true; break; }
            }
        }
    }
    if (!allowed) {
        if (verbose) {
            std::printf("clocks: %s %u Hz not offered (list type %d, %d entries) — refused\n",
                        what, (unsigned)hz, (int)type, (int)count);
            std::fflush(stdout);
        }
        clkrstCloseSession(&s);
        return false;
    }
    Result rc = clkrstSetClockRate(&s, hz);
    clkrstCloseSession(&s);
    if (verbose) {
        std::printf("clocks: %s -> %u Hz rc=0x%x\n", what, (unsigned)hz, rc);
        std::fflush(stdout);
    }
    return R_SUCCEEDED(rc);
}

// 0 = leave the OS alone, 1 = CPU 1785 MHz.
extern "C" void flashnx_set_clock_mode(int mode) {
    if (!clk_init_once()) {
        std::printf("clocks: clkrst unavailable, staying on OS defaults\n");
        std::fflush(stdout);
        return;
    }
    mode = (mode == 1) ? 1 : 0;
    if (mode == s_clk_mode) return;

    if (mode == 1) {
        // Capture the rate to come back to at the moment of raising, never
        // once per process. A single boot-time latch would freeze whatever apm
        // happened to have raised at an uncontrolled instant and write it back
        // for the rest of the session. Re-reading here also means a dock change
        // between two raises cannot pin a stale value.
        uint32_t orig = clk_get(PcvModuleId_CpuBus);
        if (orig == 0) {
            std::printf("clocks: cannot read the current CPU rate, staying on OS defaults\n");
            std::fflush(stdout);
            return;
        }
        s_clk_orig_cpu = orig;
        if (!clk_set(PcvModuleId_CpuBus, CPU_HIGH_HZ, "cpu")) return;
        s_clk_mode = 1;
        std::printf("clocks: power mode HIGH (was %u Hz)\n", (unsigned)orig);
    } else {
        if (s_clk_orig_cpu != 0) {
            clk_set(PcvModuleId_CpuBus, s_clk_orig_cpu, "cpu(restore)");
        }
        s_clk_mode = 0;
        std::printf("clocks: power mode NORMAL\n");
    }
    std::fflush(stdout);
}

extern "C" int flashnx_clock_mode(void) { return s_clk_mode; }

// Times the OS took the clock back from under us, published in the heartbeat.
// Until this exists the revocation question cannot be answered: the re-assert
// is deliberately silent, so a revoke followed by a repair leaves no trace at
// all. apm force-revoked CpuBoostMode(FastLoad) after 25-30 s of sustained load
// in 2026-05, whatever the cadence, so the same must be assumed of clkrst until
// this counter says otherwise.
extern "C" uint32_t flashnx_clock_drift(void) { return s_clk_drift; }

// Silent, cheap re-assert for the in-game loop. Waking from sleep or returning
// from HOME resets the rate, so a raised clock has to be re-applied. Nothing
// here prints, and a write only happens when the rate has actually drifted,
// because this runs inside the capture it is meant to serve.
extern "C" void flashnx_clocks_reassert(void) {
    if (s_clk_mode != 1) return;
    if (clk_get(PcvModuleId_CpuBus) != CPU_HIGH_HZ) {
        s_clk_drift++;
        clk_set(PcvModuleId_CpuBus, CPU_HIGH_HZ, "cpu", false);
    }
}

// Put the console back where we found it, on the way out of a game and again
// before the .nro exits. A crash mid-session is the only way to leave a raised
// clock behind, and Horizon resets clkrst state when the process dies.
extern "C" void flashnx_clocks_restore(void) {
    flashnx_set_clock_mode(0);
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
