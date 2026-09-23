// Sampling profiler for the Ruffle worker thread. A dev tool: it does nothing
// at all unless `sdmc:/switch/FlashNX/prof.on` exists at boot, except in the
// `--prof` build (FLASHNX_PROF_ALWAYS, see cpp/Makefile), made only for it.
//
// Why: the per-action AVM1 counters (the `instr` build) say which bytecode is
// expensive, never why. GetMember at 900 ns could be the name lookup, the
// child scan, the prototype chain or the allocator, and every guess costs a
// build and a play session. A sampler answers all of them at once, for the
// whole frame, without touching the code being measured.
//
// How: a thread on another core pauses the worker about once a millisecond
// (svcSetThreadActivity), reads its registers (svcGetThreadContext3), walks
// its frame-pointer chain, lets it run again, and appends the addresses to
// `sdmc:/switch/FlashNX/prof.bin`. `scripts/prof_report.py` turns that file
// and the build's FlashNX.elf into flat and inclusive profiles. The kernel
// only lets a process do this to its own threads, which is all we need.
//
// Frame pointers: Rust omits them on this target, so a stack deeper than the
// sampled function and its caller needs the `--prof` build (scripts/build.sh),
// which adds `-C force-frame-pointers=yes`. The C side (libnx, newlib, Mesa)
// is prebuilt without them: a sample inside it keeps its PC and LR, and the
// chain resumes at the first Rust frame when the C code left x29 alone.
//
// Safety: between the pause and the resume the sampler takes no lock,
// allocates nothing and reads only inside the worker's stack mirror, so it
// cannot leave the worker paused. Every write to the SD card happens after
// the resume.

#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <cstdlib>
#include <switch.h>

// `_start` sits at address 0 of the ELF (see switch.ld), so its runtime
// address is the image base, and `pc - base` is an address `nm` knows.
extern "C" char _start[];

namespace {

constexpr int      kMaxFrames  = 200; // u8 in the record; AVM1 recursion is deep
constexpr size_t   kBufBytes   = 512 * 1024;
constexpr uint64_t kIntervalNs = 1000000; // ~1 kHz
constexpr const char* kMarker  = "sdmc:/switch/FlashNX/prof.on";
constexpr const char* kOutPath = "sdmc:/switch/FlashNX/prof.bin";

// Record types in prof.bin, after the 32-byte header.
constexpr uint8_t kRecSample = 1; // u8 type, u8 n, u16 regime, u32 frame, u32 addr[n]
constexpr uint8_t kRecEvent  = 2; // u8 type, u8 kind, u16 game, u32 frame

Thread*  g_target = nullptr;
Thread   g_sampler;
bool     g_sampler_started = false;
FILE*    g_file = nullptr;
uint8_t* g_buf  = nullptr;
size_t   g_len  = 0;

std::atomic<bool>     g_run{false};
std::atomic<bool>     g_active{false};
std::atomic<uint32_t> g_frame{0};
std::atomic<uint16_t> g_regime{0};

// Sampler-thread statistics, printed at shutdown.
uint64_t g_samples = 0;
uint64_t g_pause_failed = 0;
uint64_t g_ctx_failed = 0;
uint64_t g_frames_total = 0;
uint64_t g_bytes = 0;

void flush_buf() {
    if (g_len == 0 || !g_file) return;
    fwrite(g_buf, 1, g_len, g_file);
    fflush(g_file);
    g_bytes += g_len;
    g_len = 0;
}

void put(const void* p, size_t n) {
    if (g_len + n > kBufBytes) flush_buf();
    std::memcpy(g_buf + g_len, p, n);
    g_len += n;
}

void put_event(uint8_t kind, uint16_t game, uint32_t frame) {
    uint8_t hdr[4] = {kRecEvent, kind, (uint8_t)(game & 0xff), (uint8_t)(game >> 8)};
    put(hdr, 4);
    put(&frame, 4);
}

void sampler_main(void*) {
    const uintptr_t base = (uintptr_t)_start;
    const uintptr_t lo = (uintptr_t)g_target->stack_mirror;
    const uintptr_t hi = lo + g_target->stack_sz;
    const Handle h = g_target->handle;

    uint32_t frames[kMaxFrames];
    ThreadContext ctx;
    bool was_active = false;
    uint16_t game = 0;

    while (g_run.load(std::memory_order_relaxed)) {
        svcSleepThread(kIntervalNs);

        const bool active = g_active.load(std::memory_order_acquire);
        if (active != was_active) {
            if (active) ++game;
            put_event(active ? 1 : 2, game, g_frame.load(std::memory_order_relaxed));
            // A game just ended: get its samples onto the card now, in case
            // the next thing that happens is the app being closed.
            if (!active) flush_buf();
            was_active = active;
        }
        if (!active) continue;

        if (R_FAILED(svcSetThreadActivity(h, ThreadActivity_Paused))) {
            ++g_pause_failed;
            continue;
        }
        // ---- worker paused: no locks, no allocation, bounded reads ----
        int n = 0;
        const uint32_t frame = g_frame.load(std::memory_order_relaxed);
        const uint16_t regime = g_regime.load(std::memory_order_relaxed);
        if (R_SUCCEEDED(svcGetThreadContext3(&ctx, h))) {
            frames[n++] = (uint32_t)(ctx.pc.x - base);
            // LR is only meaningful as an address in the image; the image is
            // far below 4 GB, so an offset that does not fit is not one.
            if (ctx.lr >= base && ctx.lr - base < 0x100000000ULL) {
                frames[n++] = (uint32_t)(ctx.lr - base);
            }
            // Frame records are {previous x29, return address}, each one higher
            // up the stack than the last. Stop at the first that is not.
            uintptr_t fp = (uintptr_t)ctx.fp;
            uintptr_t floor = (uintptr_t)ctx.sp;
            while (n < kMaxFrames && (fp & 7) == 0 && fp >= floor && fp >= lo &&
                   fp + 16 <= hi) {
                const uint64_t* rec = (const uint64_t*)fp;
                const uint64_t next = rec[0];
                const uint64_t ret = rec[1];
                if (ret < base || ret - base >= 0x100000000ULL) break;
                frames[n++] = (uint32_t)(ret - base);
                floor = fp + 16;
                fp = (uintptr_t)next;
            }
        }
        svcSetThreadActivity(h, ThreadActivity_Runnable);
        // ---- worker running again ----
        if (n == 0) {
            ++g_ctx_failed;
            continue;
        }
        uint8_t hdr[4] = {kRecSample, (uint8_t)n, (uint8_t)(regime & 0xff),
                          (uint8_t)(regime >> 8)};
        put(hdr, 4);
        put(&frame, 4);
        put(frames, (size_t)n * 4);
        ++g_samples;
        g_frames_total += (uint64_t)n;
    }
    flush_buf();
}

} // namespace

extern "C" {

// Arms the profiler if the marker is on the card. `worker` must be created
// (threadCreate) and must outlive prof_shutdown().
void prof_boot(Thread* worker) {
#ifndef FLASHNX_PROF_ALWAYS
    FILE* marker = std::fopen(kMarker, "rb");
    if (!marker) return;
    std::fclose(marker);
#endif

    g_buf = (uint8_t*)std::malloc(kBufBytes);
    g_file = std::fopen(kOutPath, "wb");
    if (!g_buf || !g_file) {
        std::printf("prof: cannot open %s\n", kOutPath);
        std::fflush(stdout);
        if (g_file) std::fclose(g_file);
        std::free(g_buf);
        g_file = nullptr;
        g_buf = nullptr;
        return;
    }
    g_target = worker;

    // Header: magic, image base, where prof_boot really is (a check for the
    // report script), sampling interval, and the frame cap.
    const uint64_t base = (uint64_t)(uintptr_t)_start;
    const uint64_t self = (uint64_t)(uintptr_t)&prof_boot;
    const uint32_t interval_us = (uint32_t)(kIntervalNs / 1000);
    const uint32_t max_frames = kMaxFrames;
    put("FNXPROF1", 8);
    put(&base, 8);
    put(&self, 8);
    put(&interval_us, 4);
    put(&max_frames, 4);

    // Core 1, above everything else we run: it must wake on time, and it must
    // share a core neither with the worker it pauses (core 0, the default
    // core) nor with the audio thread (core 2).
    g_run.store(true);
    Result rc = threadCreate(&g_sampler, sampler_main, nullptr, nullptr, 64 * 1024, 0x20, 1);
    if (R_FAILED(rc)) {
        rc = threadCreate(&g_sampler, sampler_main, nullptr, nullptr, 64 * 1024, 0x20, -2);
    }
    if (R_FAILED(rc) || R_FAILED(threadStart(&g_sampler))) {
        std::printf("prof: cannot start the sampler thread (0x%x)\n", rc);
        std::fflush(stdout);
        g_run.store(false);
        std::fclose(g_file);
        std::free(g_buf);
        g_file = nullptr;
        g_buf = nullptr;
        return;
    }
    g_sampler_started = true;
    std::printf("prof: sampling the worker every %u us into %s (image base 0x%lx)\n",
                (unsigned)interval_us, kOutPath, (unsigned long)base);
    std::fflush(stdout);
}

// Samples are only taken while a game runs.
void prof_game_active(int active) {
    g_active.store(active != 0, std::memory_order_release);
}

// Once per host frame, so a sample can be matched with the SLOW lines.
void prof_frame(void) {
    g_frame.fetch_add(1, std::memory_order_relaxed);
}

// The A/B window the worker is in (main.cpp, profiling build), stamped on
// every sample.
void prof_set_regime(int regime) {
    g_regime.store((uint16_t)regime, std::memory_order_relaxed);
}

void prof_shutdown(void) {
    if (!g_sampler_started) return;
    g_active.store(false);
    g_run.store(false);
    threadWaitForExit(&g_sampler);
    threadClose(&g_sampler);
    g_sampler_started = false;
    std::fclose(g_file);
    g_file = nullptr;
    std::free(g_buf);
    g_buf = nullptr;
    std::printf("prof: %llu samples (avg %.1f frames), %llu pause failures, %llu context "
                "failures, %llu KB written\n",
                (unsigned long long)g_samples,
                g_samples ? (double)g_frames_total / (double)g_samples : 0.0,
                (unsigned long long)g_pause_failed, (unsigned long long)g_ctx_failed,
                (unsigned long long)(g_bytes / 1024));
    std::fflush(stdout);
}

} // extern "C"
