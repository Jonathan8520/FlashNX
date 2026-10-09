// Game saves written off the game thread (2026-10-09).
//
// Writing a SharedObject to the SD card costs ~50 ms (open, write and close in
// a games folder of hundreds of files, then the commit that makes it durable).
// It used to run inside the frame that saved: Burrito Bison stores its
// achievements during a run and each one froze a frame for 50-60 ms, and the
// four saves at the end of a run froze ~200 ms (sampling profiler + as3prof,
// `SharedObject/flush()` 46-54 ms for one call). Saves are now queued here and
// written by this worker; the Rust storage backend keeps the latest bytes of
// each one in memory, so a read right after a write sees it. Each save is
// written beside the old one and swapped in once committed (`write_one`).
//
// A save still being written when the game is quit is waited for
// (`flashnx_save_drain`, called once the player is gone). A second save of the
// same file queued before the first was written replaces it: only the newest
// is written. Turned off by `asyncsave.off` (Rust side).
#include <switch.h>

#include <cstdio>
#include <cstring>
#include <deque>
#include <string>
#include <vector>

namespace {

struct Job {
    std::string path;
    std::vector<unsigned char> data;
};

Mutex g_mutex;
CondVar g_work;
CondVar g_idle;
std::deque<Job> g_queue;
bool g_busy = false;
bool g_started = false;
Thread g_thread;

double ms_since(u64 t0) {
    return (double)armTicksToNs(armGetSystemTick() - t0) / 1e6;
}

// Never over the save itself: the new version goes to `<save>.tmp`, is
// committed, and only then takes the save's place. A power cut at any point
// leaves either the old save or the new one whole, never a truncated file (the
// way it used to be written, over the old file, a cut in the middle lost the
// whole save). fsdev's rename does not replace an existing file, hence the
// remove first; between the two, the complete `.tmp` is what a read falls back
// to (`storage.rs`, `read_save`).
void write_one(const Job& j) {
    const u64 t0 = armGetSystemTick();
    const std::string tmp = j.path + ".tmp";
    bool ok = false;
    FILE* f = std::fopen(tmp.c_str(), "wb");
    if (f) {
        ok = std::fwrite(j.data.data(), 1, j.data.size(), f) == j.data.size();
        ok = (std::fclose(f) == 0) && ok;
    }
    if (!ok) {
        std::remove(tmp.c_str());
        std::printf("save: FAILED to write %s (%zu bytes), previous save kept\n", tmp.c_str(), j.data.size());
        std::fflush(stdout);
        return;
    }
    fsdevCommitDevice("sdmc");
    std::remove(j.path.c_str());
    const bool moved = std::rename(tmp.c_str(), j.path.c_str()) == 0;
    const Result rc = fsdevCommitDevice("sdmc");
    std::printf("save: %s %s (%zu bytes) in %.1f ms off the game thread, commit rc=0x%x\n",
                moved ? "wrote" : "wrote (left as .tmp, rename failed)", j.path.c_str(),
                j.data.size(), ms_since(t0), (unsigned)rc);
    std::fflush(stdout);
}

void worker(void*) {
    mutexLock(&g_mutex);
    for (;;) {
        while (g_queue.empty()) condvarWait(&g_work, &g_mutex);
        Job j = std::move(g_queue.front());
        g_queue.pop_front();
        g_busy = true;
        mutexUnlock(&g_mutex);

        // Commits included (0.1 ms each, measured): they are what makes a save
        // survive a crash or a power cut (libnx buffers sdmc writes).
        write_one(j);

        mutexLock(&g_mutex);
        g_busy = false;
        if (g_queue.empty()) condvarWakeAll(&g_idle);
    }
}

bool start() {
    if (g_started) return true;
    mutexInit(&g_mutex);
    condvarInit(&g_work);
    condvarInit(&g_idle);
    // Core 2, beside the audio worker but below its priority: this one spends
    // its time blocked on the file system, and must never delay a sound buffer.
    Result rc = threadCreate(&g_thread, worker, nullptr, nullptr, 64 * 1024, 0x2E, 2);
    if (R_FAILED(rc)) rc = threadCreate(&g_thread, worker, nullptr, nullptr, 64 * 1024, 0x2E, -2);
    if (R_FAILED(rc)) {
        std::printf("save: worker threadCreate failed 0x%x, saves stay synchronous\n", (unsigned)rc);
        std::fflush(stdout);
        return false;
    }
    threadStart(&g_thread);
    g_started = true;
    return true;
}

}  // namespace

// Queue `data` to be written to `path`. 1 when queued (the caller must not
// write it itself), 0 when the worker could not be started.
extern "C" int flashnx_save_async(const char* path, const unsigned char* data, size_t len) {
    if (!path || !start()) return 0;
    mutexLock(&g_mutex);
    for (Job& j : g_queue) {
        if (j.path == path) {
            j.data.assign(data, data + len);
            mutexUnlock(&g_mutex);
            return 1;
        }
    }
    Job j;
    j.path = path;
    j.data.assign(data, data + len);
    g_queue.push_back(std::move(j));
    condvarWakeOne(&g_work);
    mutexUnlock(&g_mutex);
    return 1;
}

// Block until every queued save is written and committed.
extern "C" void flashnx_save_drain(void) {
    if (!g_started) return;
    const u64 t0 = armGetSystemTick();
    mutexLock(&g_mutex);
    const bool waited = !g_queue.empty() || g_busy;
    while (!g_queue.empty() || g_busy) condvarWait(&g_idle, &g_mutex);
    mutexUnlock(&g_mutex);
    if (waited) {
        std::printf("save: drained the queue in %.1f ms\n", ms_since(t0));
        std::fflush(stdout);
    }
}
