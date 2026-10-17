// Phase 3.7 — HTTPS networking layer (archive.org imports).
//
// libcurl + mbedtls (statically linked via switch-curl/switch-mbedtls
// portlibs). CA bundle is embedded in the Rust staticlib via
// `include_bytes!` and written to SD on first boot via
// `write_cacert_to_sd`. libcurl 7.69.x lacks CURLOPT_CAINFO_BLOB (added
// in 7.77), so we pass a filesystem path via CURLOPT_CAINFO.
//
// **Two entry styles**:
//   - `https_get_into_buf` (synchronous, blocks ~1-3 s) — for the small
//     archive.org metadata JSON. UI freezes during the call; acceptable
//     because the user expects to wait after pressing A on "FETCH".
//   - `https_download_start` + `https_download_tick` (async via the curl
//     multi interface) — for SWF downloads that can take 10-60 s. We
//     poll one tick per render frame so the library UI keeps updating
//     (progress bar, animations, home-button responsiveness).
//
// The multi-handle state is a single global (we only ever have one
// download in flight at a time — the library serialises them).

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cstdint>
#include <string>
#include <sys/stat.h>

#include <switch.h>
#include <curl/curl.h>

// Free space on the SD card in bytes, or -1 when the card cannot say.
//
// Debug marker, the same shape as `noalloc.on`: a number in
// `sdmc:/switch/FlashNX/fakefree.mb` is taken as the free space in MB, which is
// how "the card is full" gets tried on a card that is not.
extern "C" long long flashnx_sd_free_bytes(void) {
    if (FILE* f = std::fopen("sdmc:/switch/FlashNX/fakefree.mb", "rb")) {
        char buf[32] = {};
        std::fread(buf, 1, sizeof(buf) - 1, f);
        std::fclose(f);
        const long long mb = std::atoll(buf);
        std::printf("sd: fakefree.mb present -> %lld MB free\n", mb);
        std::fflush(stdout);
        return mb * 1024 * 1024;
    }
    FsFileSystem* fs = fsdevGetDeviceFileSystem("sdmc");
    if (!fs) return -1;
    s64 free_bytes = 0;
    if (R_FAILED(fsFsGetFreeSpace(fs, "/", &free_bytes))) return -1;
    return (long long)free_bytes;
}

namespace {

constexpr const char* CACERT_PATH = "sdmc:/switch/FlashNX/cacert.pem";

bool g_curl_inited = false;

struct BufferWriter {
    char* buf;
    int   cap;
    int   pos;
    bool  overflow;
};

size_t write_to_buffer(char* ptr, size_t size, size_t nmemb, void* userdata) {
    BufferWriter* w = (BufferWriter*)userdata;
    const size_t bytes = size * nmemb;
    if (w->overflow || w->pos + (int)bytes >= w->cap) {
        w->overflow = true;
        return 0;
    }
    std::memcpy(w->buf + w->pos, ptr, bytes);
    w->pos += (int)bytes;
    return bytes;
}

// Growing in-memory response for the ASYNC metadata GET (`https_get_*`). It
// reuses the single g_multi/g_handle below (a metadata GET and a file download
// are never in flight at once), writing here instead of to a file.
std::string g_get_buf;

size_t write_to_string(char* ptr, size_t size, size_t nmemb, void* userdata) {
    std::string* s = (std::string*)userdata;
    const size_t bytes = size * nmemb;
    // Cap at 8 MB so a pathological response can't exhaust the heap.
    if (s->size() + bytes > 8u * 1024u * 1024u) {
        return 0; // signal write error -> curl aborts the transfer
    }
    s->append(ptr, bytes);
    return bytes;
}

// Async download state (curl multi interface). One in-flight transfer at
// a time — `https_download_start` returns an error if g_multi is non-null.
CURLM*    g_multi      = nullptr;
CURL*     g_handle     = nullptr;
FILE*     g_dl_file    = nullptr;
uint64_t  g_bytes_done = 0;
uint64_t  g_bytes_total = 0;
char      g_dl_out_path[768] = {0};
// Last HTTP status / curl result recorded by `https_download_tick` after
// the transfer finishes; used by the Rust layer to log specific errors.
int       g_last_curl_result = 0;
long      g_last_http_code   = 0;
uint64_t  g_dl_start_tick    = 0;   // armGetSystemTick at start, for the speed log

// Room on the card, checked once per download as soon as its size is known
// (`xfer_progress`). A download that cannot fit stops there, in the first
// second, instead of filling the card and failing at the end. `g_dl_unpack`
// is set for a GameZIP, which is extracted next to itself before the zip is
// deleted: it needs its own size twice.
bool      g_dl_space_checked = false;
bool      g_dl_no_space      = false;
bool      g_dl_unpack        = false;
uint64_t  g_dl_space_size    = 0;
uint64_t  g_dl_space_need    = 0;
uint64_t  g_dl_space_free    = 0;

// Kept free on top of what a download needs, for everything else that writes
// to the card while it runs (saves, covers, the log).
constexpr uint64_t SPACE_MARGIN = 8ull * 1024 * 1024;

void multi_cleanup(bool delete_partial) {
    // Drop the CPU boost we raised for the download (idempotent).
    appletSetCpuBoostMode(ApmCpuBoostMode_Normal);
    if (g_handle) {
        if (g_multi) {
            curl_multi_remove_handle(g_multi, g_handle);
        }
        curl_easy_cleanup(g_handle);
        g_handle = nullptr;
    }
    if (g_multi) {
        curl_multi_cleanup(g_multi);
        g_multi = nullptr;
    }
    if (g_dl_file) {
        std::fclose(g_dl_file);
        g_dl_file = nullptr;
    }
    if (delete_partial && g_dl_out_path[0] != 0) {
        std::remove(g_dl_out_path);
    }
    g_bytes_done = 0;
    g_bytes_total = 0;
    g_dl_out_path[0] = 0;
    g_dl_unpack = false;
}

int xfer_progress(void* /*ud*/, curl_off_t dltotal, curl_off_t dlnow,
                  curl_off_t /*ultotal*/, curl_off_t /*ulnow*/) {
    g_bytes_done = (uint64_t)dlnow;
    g_bytes_total = (uint64_t)dltotal;
    // Only on the answer that carries the file: a redirect has a length of its
    // own (a few bytes of HTML), and checking that would pass every download.
    if (!g_dl_space_checked && dltotal > 0 && g_handle) {
        long code = 0;
        curl_easy_getinfo(g_handle, CURLINFO_RESPONSE_CODE, &code);
        if (code >= 200 && code < 300) {
            g_dl_space_checked = true;
            const long long free_bytes = flashnx_sd_free_bytes();
            if (free_bytes >= 0) {
                const uint64_t left = (uint64_t)(dltotal - dlnow);
                const uint64_t need = left + (g_dl_unpack ? (uint64_t)dltotal : 0) + SPACE_MARGIN;
                if (need > (uint64_t)free_bytes) {
                    g_dl_no_space = true;
                    g_dl_space_size = (uint64_t)dltotal;
                    g_dl_space_need = need;
                    g_dl_space_free = (uint64_t)free_bytes;
                    std::printf("https_download: %llu bytes needed, %lld free on the card -> stopping\n",
                                (unsigned long long)need, free_bytes);
                    std::fflush(stdout);
                    return 1; // aborts the transfer (CURLE_ABORTED_BY_CALLBACK)
                }
            }
        }
    }
    return 0;
}

// Big page-aligned stdio buffer for the download FILE* (via setvbuf): batches SD
// writes into large fsdev calls instead of many small ones (a classic Switch
// bottleneck). One download in flight at a time, so one file-scope buffer is OK.
alignas(0x1000) char g_dl_iobuf[512 * 1024];

// Time spent inside fwrite() for the current download = the SD-write cost. Logged
// against the total at completion so we can see SD vs network/TLS share.
uint64_t g_fwrite_ticks = 0;

// Download write callback: same as curl's default (fwrite to the FILE*) but times
// the write so we can attribute the SD-card cost.
size_t dl_write_cb(char* ptr, size_t size, size_t nmemb, void* userdata) {
    FILE* f = (FILE*)userdata;
    const u64 t0 = armGetSystemTick();
    const size_t w = std::fwrite(ptr, size, nmemb, f);
    g_fwrite_ticks += armGetSystemTick() - t0;
    return w;
}

} // namespace

extern "C" int net_init(void) {
    if (g_curl_inited) return 0;
    CURLcode rc = curl_global_init(CURL_GLOBAL_DEFAULT);
    if (rc != CURLE_OK) {
        std::printf("net_init: curl_global_init failed (%d)\n", (int)rc);
        std::fflush(stdout);
        return -1;
    }
    g_curl_inited = true;
    std::printf("net_init: libcurl %s up\n", curl_version());
    std::fflush(stdout);
    return 0;
}

extern "C" void net_shutdown(void) {
    multi_cleanup(false);
    if (g_curl_inited) {
        curl_global_cleanup();
        g_curl_inited = false;
    }
}

extern "C" int write_cacert_to_sd(const char* data, int len) {
    if (!data || len <= 0) return -1;
    ::mkdir("sdmc:/switch", 0755);
    ::mkdir("sdmc:/switch/FlashNX", 0755);
    struct stat st;
    if (::stat(CACERT_PATH, &st) == 0 && (int)st.st_size == len) {
        std::printf("write_cacert_to_sd: present, %d bytes at %s (skip)\n",
                    (int)st.st_size, CACERT_PATH);
        std::fflush(stdout);
        return 0;
    }
    FILE* f = std::fopen(CACERT_PATH, "wb");
    if (!f) {
        std::printf("write_cacert_to_sd: fopen(%s) failed\n", CACERT_PATH);
        std::fflush(stdout);
        return -2;
    }
    const size_t written = std::fwrite(data, 1, (size_t)len, f);
    std::fclose(f);
    if ((int)written != len) {
        std::printf("write_cacert_to_sd: short write (%zu/%d)\n", written, len);
        std::fflush(stdout);
        return -3;
    }
    std::printf("write_cacert_to_sd: wrote %d bytes to %s\n", len, CACERT_PATH);
    std::fflush(stdout);
    return 0;
}

extern "C" int https_get_into_buf(const char* url, char* buf, int cap) {
    if (net_init() != 0) return -1;
    if (cap < 2) return -3;
    CURL* c = curl_easy_init();
    if (!c) return -1;
    BufferWriter w = { buf, cap - 1, 0, false };
    curl_easy_setopt(c, CURLOPT_URL, url);
    curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/2.0.0 (Nintendo Switch homebrew)");
    curl_easy_setopt(c, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(c, CURLOPT_MAXREDIRS, 5L);
    curl_easy_setopt(c, CURLOPT_CAINFO, CACERT_PATH);
    curl_easy_setopt(c, CURLOPT_WRITEFUNCTION, write_to_buffer);
    curl_easy_setopt(c, CURLOPT_WRITEDATA, &w);
    curl_easy_setopt(c, CURLOPT_TIMEOUT, 30L);
    curl_easy_setopt(c, CURLOPT_CONNECTTIMEOUT, 10L);
    CURLcode res = curl_easy_perform(c);
    long http_code = 0;
    curl_easy_getinfo(c, CURLINFO_RESPONSE_CODE, &http_code);
    curl_easy_cleanup(c);
    if (w.overflow) {
        std::printf("https_get %s: overflow (cap=%d, want >=%d)\n", url, cap, w.pos);
        std::fflush(stdout);
        return -3;
    }
    if (res != CURLE_OK || http_code < 200 || http_code >= 400) {
        // Record the real cause so the Rust layer can SHOW it on screen
        // (the user has no nxlink). Otherwise "-2" is all they ever see.
        g_last_curl_result = (int)res;
        g_last_http_code = http_code;
        std::printf("https_get %s: curl=%d (%s) http=%ld\n",
                    url, (int)res, curl_easy_strerror(res), http_code);
        std::fflush(stdout);
        return -2;
    }
    buf[w.pos] = '\0';
    return w.pos;
}

// Synchronous HTTPS POST with a JSON body. Used by the bug-report flow
// (`crate::bugreport`) to hand a small JSON payload to the relay endpoint that
// opens the GitHub issue. Same TLS/CA setup as `https_get_into_buf`; sets the
// `Content-Type: application/json` header and POSTs `body` (NUL-terminated).
// The response (the relay's JSON, e.g. the created issue URL) is written into
// `buf`. Returns bytes written, or a negative code (-1 init, -2 transfer
// failed, -3 response overflow) — mirrors `https_get_into_buf` so the Rust
// layer can reuse `https_last_error_desc`.
extern "C" int https_post_json(const char* url, const char* body, char* buf, int cap) {
    if (net_init() != 0) return -1;
    if (cap < 2) return -3;
    CURL* c = curl_easy_init();
    if (!c) return -1;
    struct curl_slist* headers = nullptr;
    headers = curl_slist_append(headers, "Content-Type: application/json");
    headers = curl_slist_append(headers, "Accept: application/json");
    BufferWriter w = { buf, cap - 1, 0, false };
    curl_easy_setopt(c, CURLOPT_URL, url);
    curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/2.0.0 (Nintendo Switch homebrew)");
    curl_easy_setopt(c, CURLOPT_HTTPHEADER, headers);
    curl_easy_setopt(c, CURLOPT_POST, 1L);
    curl_easy_setopt(c, CURLOPT_POSTFIELDS, body ? body : "");
    curl_easy_setopt(c, CURLOPT_POSTFIELDSIZE, (long)(body ? std::strlen(body) : 0));
    curl_easy_setopt(c, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(c, CURLOPT_MAXREDIRS, 5L);
    curl_easy_setopt(c, CURLOPT_CAINFO, CACERT_PATH);
    curl_easy_setopt(c, CURLOPT_WRITEFUNCTION, write_to_buffer);
    curl_easy_setopt(c, CURLOPT_WRITEDATA, &w);
    curl_easy_setopt(c, CURLOPT_TIMEOUT, 30L);
    curl_easy_setopt(c, CURLOPT_CONNECTTIMEOUT, 10L);
    CURLcode res = curl_easy_perform(c);
    long http_code = 0;
    curl_easy_getinfo(c, CURLINFO_RESPONSE_CODE, &http_code);
    curl_easy_cleanup(c);
    curl_slist_free_all(headers);
    if (w.overflow) {
        std::printf("https_post %s: overflow (cap=%d)\n", url, cap);
        std::fflush(stdout);
        return -3;
    }
    if (res != CURLE_OK || http_code < 200 || http_code >= 400) {
        g_last_curl_result = (int)res;
        g_last_http_code = http_code;
        std::printf("https_post %s: curl=%d (%s) http=%ld\n",
                    url, (int)res, curl_easy_strerror(res), http_code);
        std::fflush(stdout);
        return -2;
    }
    buf[w.pos] = '\0';
    return w.pos;
}

// Short human description of the most recent transfer failure recorded by
// `https_get_into_buf` (or `https_download_tick`). Rust calls this after a
// negative return to compose the on-screen error, e.g.
//   "curl 60 (SSL peer certificate or SSH remote key was not OK) http 0"
// curl 60/77 -> certificate/cacert problem; curl 6 -> DNS; curl 7/28 ->
// connect/timeout; http 403/429 -> blocked/rate-limited.
extern "C" void https_last_error_desc(char* out, int cap) {
    if (!out || cap < 1) return;
    std::snprintf(out, (size_t)cap, "curl %d (%s) http %ld",
                  g_last_curl_result,
                  curl_easy_strerror((CURLcode)g_last_curl_result),
                  g_last_http_code);
}

// Same failure, as raw numbers. `https_last_error_desc` is for the log; these
// let Rust MAP the cause to a specific, actionable message ("check the console
// clock" for a TLS failure, "narrow your search" for an oversized response)
// instead of showing one catch-all sentence for every network problem.
extern "C" int https_last_curl_code(void) { return g_last_curl_result; }
extern "C" int https_last_http_code(void) { return (int)g_last_http_code; }

// Begin an async download. Sets up a curl multi handle so `_tick` can be
// called once per render frame without blocking. Returns:
//   0  on success (download is now in progress)
//   -1 on curl init failure
//   -4 on fopen(out_path) failure
//   -5 if a download is already in flight
// `unpack`: the file is a GameZIP that will be extracted beside itself, so the
// room checked for is twice its size (see `g_dl_unpack`).
extern "C" int https_download_start_ex(const char* url, const char* out_path, int unpack) {
    if (g_multi || g_handle) {
        return -5;
    }
    if (net_init() != 0) return -1;
    FILE* f = std::fopen(out_path, "wb");
    if (!f) {
        std::printf("https_download_start: fopen(%s) failed\n", out_path);
        std::fflush(stdout);
        return -4;
    }
    // Full buffering with a big aligned buffer: turns curl's stream of small
    // writes into a few large fsdev writes (much faster on the Switch SD).
    std::setvbuf(f, g_dl_iobuf, _IOFBF, sizeof(g_dl_iobuf));
    CURLM* m = curl_multi_init();
    CURL* c = curl_easy_init();
    if (!m || !c) {
        if (m) curl_multi_cleanup(m);
        if (c) curl_easy_cleanup(c);
        std::fclose(f);
        std::remove(out_path);
        return -1;
    }
    std::strncpy(g_dl_out_path, out_path, sizeof(g_dl_out_path) - 1);
    g_dl_out_path[sizeof(g_dl_out_path) - 1] = 0;

    curl_easy_setopt(c, CURLOPT_URL, url);
    curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/2.0.0 (Nintendo Switch homebrew)");
    curl_easy_setopt(c, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(c, CURLOPT_MAXREDIRS, 5L);
    curl_easy_setopt(c, CURLOPT_CAINFO, CACERT_PATH);
    curl_easy_setopt(c, CURLOPT_WRITEFUNCTION, dl_write_cb);
    curl_easy_setopt(c, CURLOPT_WRITEDATA, f);
    curl_easy_setopt(c, CURLOPT_XFERINFOFUNCTION, xfer_progress);
    curl_easy_setopt(c, CURLOPT_NOPROGRESS, 0L);
    // NO hard total-time cap here. A multi-GB GameZIP (e.g. Super Smash Flash 2
    // ~3.4 GB) over the Switch's WiFi legitimately takes far longer than the old
    // 600 s limit, which aborted a perfectly-progressing transfer mid-way with a
    // curl error (the infamous "wait ~10 min, then error -2"). Instead abort only
    // on a genuine STALL: throughput under 4 KB/s sustained for 60 s. A healthy
    // slow download keeps going for as long as it needs; only a dead connection
    // (or a server that stops sending) trips it.
    curl_easy_setopt(c, CURLOPT_LOW_SPEED_LIMIT, 4096L);
    curl_easy_setopt(c, CURLOPT_LOW_SPEED_TIME, 60L);
    curl_easy_setopt(c, CURLOPT_CONNECTTIMEOUT, 10L);
    // 256 KB receive buffer (default is 16 KB): lets each perform() pull a lot
    // more off the socket, which matters because we only pump per render frame.
    curl_easy_setopt(c, CURLOPT_BUFFERSIZE, 262144L);

    curl_multi_add_handle(m, c);

    g_multi = m;
    g_handle = c;
    g_dl_file = f;
    g_bytes_done = 0;
    g_bytes_total = 0;
    g_last_curl_result = 0;
    g_last_http_code = 0;
    g_dl_start_tick = armGetSystemTick();
    g_fwrite_ticks = 0;
    g_dl_space_checked = false;
    g_dl_no_space = false;
    g_dl_unpack = unpack != 0;
    // Boost the CPU for the download (TLS decrypt + curl can be the limit on the
    // base clock). Reset by multi_cleanup when the transfer ends.
    appletSetCpuBoostMode(ApmCpuBoostMode_FastLoad);
    std::printf("https_download_start: %s -> %s\n", url, out_path);
    std::fflush(stdout);
    return 0;
}

extern "C" int https_download_start(const char* url, const char* out_path) {
    return https_download_start_ex(url, out_path, 0);
}

// What the last download stopped for lack of room was (its size), needed and
// found, in bytes. Meaningful after `https_download_tick` returned -6.
extern "C" void https_download_space(uint64_t* size, uint64_t* need, uint64_t* free_bytes) {
    if (size) *size = g_dl_space_size;
    if (need) *need = g_dl_space_need;
    if (free_bytes) *free_bytes = g_dl_space_free;
}

// Pump the multi handle once. Returns:
//   0 = still in progress
//   1 = finished successfully
//   -6 = stopped because the card has no room for it (`https_download_space`)
//   <0 = error (download cleaned up, file deleted)
extern "C" int https_download_tick(void) {
    if (!g_multi) return -1;
    int still_running = 0;
    // Drain the socket hard this frame instead of a single perform(): loop
    // perform + a short poll until the transfer finishes or a ~12 ms budget is
    // spent, then resume next frame. Pumping once per frame throttled throughput
    // to roughly one socket buffer per 16 ms; this keeps the link saturated while
    // the library UI still renders (~50 fps during a download).
    const u64 deadline = armGetSystemTick() + (armGetSystemTickFreq() * 12) / 1000;
    for (;;) {
        CURLMcode mc = curl_multi_perform(g_multi, &still_running);
        if (mc != CURLM_OK) {
            std::printf("https_download_tick: curl_multi_perform mc=%d\n", (int)mc);
            std::fflush(stdout);
            multi_cleanup(true);
            return -2;
        }
        if (still_running == 0) break;
        if (armGetSystemTick() >= deadline) return 0; // resume next frame
        int numfds = 0;
        curl_multi_poll(g_multi, nullptr, 0, 3, &numfds); // up to 3 ms waiting for data
        if (numfds == 0) {
            // curl_multi_poll does NOT wait here on Switch, so without this the
            // loop busy-spins for the whole 12 ms budget, every frame.
            //
            // Two libnx facts combine: `socketpair()` is a hard ENOSYS stub, so
            // curl's multi wakeup socket never exists; and `poll(NULL, 0, t)`
            // returns -1/EFAULT instead of sleeping. When the transfer has no
            // socket of its own either — no connection yet, or a dropped link —
            // curl has nothing to poll and falls back to exactly that no-op
            // wait, so it returns instantly and we re-enter curl_multi_perform
            // (and, with the synchronous resolver, another blocking DNS lookup)
            // as fast as the CPU allows. Sleep the wait we asked for ourselves.
            svcSleepThread(3ULL * 1000 * 1000); // 3 ms
        }
    }

    // Transfer finished — extract per-handle result.
    bool ok = false;
    CURLMsg* msg;
    int msgs_left = 0;
    while ((msg = curl_multi_info_read(g_multi, &msgs_left))) {
        if (msg->msg == CURLMSG_DONE && msg->easy_handle == g_handle) {
            g_last_curl_result = (int)msg->data.result;
            curl_easy_getinfo(g_handle, CURLINFO_RESPONSE_CODE, &g_last_http_code);
            ok = (msg->data.result == CURLE_OK) && (g_last_http_code >= 200) && (g_last_http_code < 400);
            break;
        }
    }
    if (!ok) {
        if (g_dl_no_space) {
            multi_cleanup(true);
            return -6;
        }
        std::printf("https_download_tick: failed curl=%d (%s) http=%ld\n",
                    g_last_curl_result, curl_easy_strerror((CURLcode)g_last_curl_result),
                    g_last_http_code);
        std::fflush(stdout);
        multi_cleanup(true);
        return -2;
    }
    // The last buffered bytes (up to the 512 KB of `g_dl_iobuf`) reach the card
    // only now. A card that fills here used to pass for a finished download and
    // leave a truncated file behind, announced as a success.
    if (g_dl_file) {
        const int close_rc = std::fclose(g_dl_file);
        g_dl_file = nullptr;
        if (close_rc != 0) {
            g_last_curl_result = (int)CURLE_WRITE_ERROR;
            std::printf("https_download_tick: the last write to %s failed\n", g_dl_out_path);
            std::fflush(stdout);
            multi_cleanup(true);
            return -2;
        }
    }
    {
        u64 freq = armGetSystemTickFreq();
        double secs = freq ? (double)(armGetSystemTick() - g_dl_start_tick) / (double)freq : 0.0;
        double sd_secs = freq ? (double)g_fwrite_ticks / (double)freq : 0.0;
        double mbps = secs > 0.0 ? ((double)g_bytes_done / (1024.0 * 1024.0)) / secs : 0.0;
        std::printf("https_download_tick: OK %ld bytes in %.1fs = %.2f MB/s (SD write %.1fs / net+TLS %.1fs) -> %s\n",
                    (long)g_bytes_done, secs, mbps, sd_secs, secs - sd_secs, g_dl_out_path);
    }
    std::fflush(stdout);
    multi_cleanup(false);
    return 1;
}

// Read current progress. `done` / `total` may be 0 before curl has
// received the Content-Length header. Caller treats total=0 as
// "indeterminate" (just show a spinner instead of a fill bar).
extern "C" void https_download_progress(uint64_t* done, uint64_t* total) {
    if (done) *done = g_bytes_done;
    if (total) *total = g_bytes_total;
}

// 1 while a download is writing to the card (the trash worker waits for it).
// Read from another thread: a hint, not a guarantee, which is all it needs.
extern "C" int https_download_active(void) {
    return (g_multi && g_dl_file) ? 1 : 0;
}

extern "C" void https_download_cancel(void) {
    if (g_multi) {
        std::printf("https_download_cancel\n"); std::fflush(stdout);
        multi_cleanup(true);
    }
}

// ── One request at a time, on a thread of its own ─────────────────────────
// Everything else that ran curl on the launcher's thread goes through here:
// the async GET below (Flashpoint "see all" / "see more", archive.org
// metadata), which was pumped from the frame like the thumbnails were, and
// the blocking GET/POST of the deferred flows (community profiles, a cover's
// search and download, the bug report), which froze their loading card for the
// whole transfer: 1.3 to 3.9 s measured on the profiles (2026-10-14). curl
// 7.69 does each connection's DNS lookup and TLS handshake inside the call
// that drives it, and re-parses all of cacert.pem for each new connection.
//
// A caller queues a request and polls it; one that has to block (net.rs
// `http_get` / `post_json` while the launcher has a frame to draw) draws its
// frames between polls. A new request supersedes the one before; a cancelled
// or superseded transfer is aborted from its progress callback.
namespace {
enum class JobState : u8 { Idle, Queued, Running, Done };
struct Job {
    JobState    state    = JobState::Idle;
    int         id       = 0;   // bumped by every start and cancel
    bool        post     = false;
    std::string url;
    std::string body;
    std::string out;
    bool        overflow = false;
    int         curl     = 0;
    long        http     = 0;
    bool        ok       = false;
};
Job     g_job;
Mutex   g_job_mutex;
CondVar g_job_wake;
Thread  g_job_thread;
bool    g_job_started = false;
bool    g_job_quit    = false;

// An answer past this is refused, as `https_get_into_buf` refuses one past its
// buffer: the largest anything here asks for is 8 MB (archive.org metadata).
constexpr size_t JOB_MAX = 16u << 20;

struct JobSink {
    std::string* out;
    bool*        overflow;
};

size_t job_write(char* ptr, size_t size, size_t nmemb, void* userdata) {
    JobSink* k = (JobSink*)userdata;
    const size_t n = size * nmemb;
    if (k->out->size() + n > JOB_MAX) {
        *k->overflow = true;
        return 0;
    }
    k->out->append(ptr, n);
    return n;
}

// Abort the transfer once its request is no longer the current one.
int job_progress(void* userdata, curl_off_t, curl_off_t, curl_off_t, curl_off_t) {
    const int mine = (int)(intptr_t)userdata;
    mutexLock(&g_job_mutex);
    const bool stale = g_job.id != mine;
    mutexUnlock(&g_job_mutex);
    return stale ? 1 : 0;
}

void job_worker(void*) {
    while (true) {
        mutexLock(&g_job_mutex);
        while (!g_job_quit && g_job.state != JobState::Queued) {
            condvarWait(&g_job_wake, &g_job_mutex);
        }
        if (g_job_quit) {
            mutexUnlock(&g_job_mutex);
            break;
        }
        const int id = g_job.id;
        const bool post = g_job.post;
        const std::string url = g_job.url;
        const std::string body = g_job.body;
        g_job.state = JobState::Running;
        mutexUnlock(&g_job_mutex);

        std::string out;
        bool overflow = false;
        JobSink sink = {&out, &overflow};
        CURLcode res = CURLE_FAILED_INIT;
        long http = 0;
        CURL* c = curl_easy_init();
        struct curl_slist* headers = nullptr;
        if (c) {
            curl_easy_setopt(c, CURLOPT_URL, url.c_str());
            curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/2.0.0 (Nintendo Switch homebrew)");
            curl_easy_setopt(c, CURLOPT_FOLLOWLOCATION, 1L);
            curl_easy_setopt(c, CURLOPT_MAXREDIRS, 5L);
            curl_easy_setopt(c, CURLOPT_CAINFO, CACERT_PATH);
            curl_easy_setopt(c, CURLOPT_WRITEFUNCTION, job_write);
            curl_easy_setopt(c, CURLOPT_WRITEDATA, &sink);
            curl_easy_setopt(c, CURLOPT_TIMEOUT, 60L);
            curl_easy_setopt(c, CURLOPT_CONNECTTIMEOUT, 10L);
            curl_easy_setopt(c, CURLOPT_NOSIGNAL, 1L);
            curl_easy_setopt(c, CURLOPT_XFERINFOFUNCTION, job_progress);
            curl_easy_setopt(c, CURLOPT_XFERINFODATA, (void*)(intptr_t)id);
            curl_easy_setopt(c, CURLOPT_NOPROGRESS, 0L);
            if (post) {
                headers = curl_slist_append(headers, "Content-Type: application/json");
                headers = curl_slist_append(headers, "Accept: application/json");
                curl_easy_setopt(c, CURLOPT_HTTPHEADER, headers);
                curl_easy_setopt(c, CURLOPT_POST, 1L);
                curl_easy_setopt(c, CURLOPT_POSTFIELDS, body.c_str());
                curl_easy_setopt(c, CURLOPT_POSTFIELDSIZE, (long)body.size());
            }
            res = curl_easy_perform(c);
            curl_easy_getinfo(c, CURLINFO_RESPONSE_CODE, &http);
            curl_easy_cleanup(c);
            if (headers) curl_slist_free_all(headers);
        }
        const bool ok = res == CURLE_OK && http >= 200 && http < 400;
        if (!ok && res != CURLE_ABORTED_BY_CALLBACK) {
            std::printf("net job %s %s: curl=%d (%s) http=%ld%s\n", post ? "POST" : "GET",
                        url.c_str(), (int)res, curl_easy_strerror(res), http,
                        overflow ? " (answer too big)" : "");
            std::fflush(stdout);
        }
        mutexLock(&g_job_mutex);
        if (g_job.id == id && g_job.state == JobState::Running) {
            g_job.out.swap(out);
            g_job.overflow = overflow;
            g_job.curl = (int)res;
            g_job.http = http;
            g_job.ok = ok;
            g_job.state = JobState::Done;
        }
        mutexUnlock(&g_job_mutex);
    }
}

// Start the worker once, beside the thumbnails' (core 2, below the audio
// worker and the save writer; 256 KB of stack for mbedtls' handshake).
bool job_worker_start() {
    if (g_job_started) return true;
    mutexInit(&g_job_mutex);
    condvarInit(&g_job_wake);
    g_job_quit = false;
    Result rc = threadCreate(&g_job_thread, job_worker, nullptr, nullptr, 256 * 1024, 0x2F, 2);
    if (R_FAILED(rc)) rc = threadCreate(&g_job_thread, job_worker, nullptr, nullptr, 256 * 1024, 0x2F, -2);
    if (R_FAILED(rc)) {
        std::printf("net: request worker threadCreate failed 0x%x\n", (unsigned)rc);
        std::fflush(stdout);
        return false;
    }
    threadStart(&g_job_thread);
    g_job_started = true;
    return true;
}

int g_get_job = 0; // the async GET's request, 0 when none
} // namespace

// Queue a request: a GET, or with `post` a POST of the JSON `body`. Returns its
// id (> 0) to poll, or -1. Supersedes the request before it.
extern "C" int https_job_start(int post, const char* url, const char* body) {
    if (!url || net_init() != 0 || !job_worker_start()) return -1;
    mutexLock(&g_job_mutex);
    g_job.id = (g_job.id >= 0x7FFFFFF0) ? 1 : g_job.id + 1;
    g_job.state = JobState::Queued;
    g_job.post = post != 0;
    g_job.url = url;
    g_job.body = body ? body : "";
    g_job.out.clear();
    g_job.overflow = false;
    g_job.curl = 0;
    g_job.http = 0;
    g_job.ok = false;
    const int id = g_job.id;
    condvarWakeOne(&g_job_wake);
    mutexUnlock(&g_job_mutex);
    return id;
}

// 0 = still running, 1 = done (take the answer), -2 = failed (the cause is in
// https_last_curl_code / https_last_http_code, as after a blocking call), -3 =
// the answer was too big, -4 = not the current request any more.
extern "C" int https_job_poll(int id) {
    if (!g_job_started) return -4;
    mutexLock(&g_job_mutex);
    int rc = -4;
    if (g_job.id == id) {
        switch (g_job.state) {
            case JobState::Queued:
            case JobState::Running: rc = 0; break;
            case JobState::Done:
                if (g_job.overflow) {
                    rc = -3;
                } else if (g_job.ok) {
                    rc = 1;
                } else {
                    g_last_curl_result = g_job.curl;
                    g_last_http_code = g_job.http;
                    rc = -2;
                }
                break;
            default: break;
        }
    }
    mutexUnlock(&g_job_mutex);
    return rc;
}

// Copy a done request's answer into `out` (NUL-terminated) and let it go.
// Returns bytes copied, -3 if it does not fit `cap`, -1 if there is none.
extern "C" int https_job_take(int id, char* out, int cap) {
    if (!out || cap < 1 || !g_job_started) return -1;
    mutexLock(&g_job_mutex);
    int rc = -1;
    if (g_job.id == id && g_job.state == JobState::Done) {
        const size_t n = g_job.out.size();
        if (n >= (size_t)cap) {
            rc = -3;
        } else {
            std::memcpy(out, g_job.out.data(), n);
            out[n] = '\0';
            rc = (int)n;
        }
        g_job.state = JobState::Idle;
        g_job.out.clear();
        g_job.out.shrink_to_fit();
    }
    mutexUnlock(&g_job_mutex);
    return rc;
}

// Drop the current request: a transfer still running is aborted.
extern "C" void https_job_cancel(void) {
    if (!g_job_started) return;
    mutexLock(&g_job_mutex);
    g_job.id = (g_job.id >= 0x7FFFFFF0) ? 1 : g_job.id + 1;
    g_job.state = JobState::Idle;
    g_job.out.clear();
    g_job.out.shrink_to_fit();
    mutexUnlock(&g_job_mutex);
}

// Stop the worker and wait for it, before the sockets go at exit.
extern "C" void https_job_stop(void) {
    if (!g_job_started) return;
    mutexLock(&g_job_mutex);
    g_job.id = (g_job.id >= 0x7FFFFFF0) ? 1 : g_job.id + 1;
    g_job.state = JobState::Idle;
    g_job_quit = true;
    condvarWakeOne(&g_job_wake);
    mutexUnlock(&g_job_mutex);
    threadWaitForExit(&g_job_thread);
    threadClose(&g_job_thread);
    g_job_started = false;
}

// ── Async in-memory GET (archive.org metadata, Flashpoint searches) ────────
// A request of the worker above: nothing of it runs on the caller's thread.
// Returns: 0 on success (in progress), -1 init.
extern "C" int https_get_start(const char* url) {
    // A GET already in flight is SUPERSEDED, not a reason to refuse: refusing
    // left the slot occupied for good (two Flashpoint searches in a row and the
    // launcher had no network left), and a new search replaces the old one in
    // the player's mind too. `https_job_start` supersedes on its own.
    const int id = https_job_start(0, url, nullptr);
    if (id < 0) return -1;
    g_get_job = id;
    g_last_curl_result = 0;
    g_last_http_code = 0;
    std::printf("https_get_start: %s\n", url);
    std::fflush(stdout);
    return 0;
}

// Poll the GET. Returns: 0 = in progress, 1 = done (read it with
// https_get_buffer), <0 = error (the cause in https_last_*).
extern "C" int https_get_tick(void) {
    if (!g_get_job) return -1;
    const int rc = https_job_poll(g_get_job);
    if (rc == 0) return 0;
    if (rc == 1) {
        std::printf("https_get_tick: OK\n");
        std::fflush(stdout);
        return 1;
    }
    std::printf("https_get_tick: failed (%d) curl=%d (%s) http=%ld\n", rc, g_last_curl_result,
                curl_easy_strerror((CURLcode)g_last_curl_result), g_last_http_code);
    std::fflush(stdout);
    g_get_job = 0;
    return -2;
}

// Copy the completed response into `out` (NUL-terminated). Returns bytes copied,
// or -3 if the response doesn't fit in `cap`. Call after `https_get_tick` == 1.
extern "C" int https_get_buffer(char* out, int cap) {
    if (!out || cap < 1 || !g_get_job) return -1;
    const int rc = https_job_take(g_get_job, out, cap);
    g_get_job = 0;
    return rc;
}

extern "C" void https_get_cancel(void) {
    if (g_get_job) {
        std::printf("https_get_cancel\n"); std::fflush(stdout);
        https_job_cancel();
        g_get_job = 0;
    }
    g_get_buf.clear();
}

// ── Async thumbnail GET (Flashpoint logos, non-blocking, PARALLEL) ─────────
// A SECOND, isolated curl-multi handle dedicated to cover/logo thumbnails and
// the importer's shelf searches, with a POOL of slots so several transfers run
// CONCURRENTLY. It deliberately does NOT touch g_multi/g_handle (the
// archive.org metadata + download handle).
//
// The transfers run on a thread of their own. They used to be pumped from the
// launcher's frame (`https_thumb_tick` called `curl_multi_perform`), and that
// call is where curl 7.69 does a connection's DNS lookup and its whole TLS
// handshake with mbedtls -- which also parses all of cacert.pem again for
// every new connection (curl caches the parsed CA store only from 7.87, and
// not for mbedtls). Measured on the importer (2026-10-14): frames of 105 to
// 217 ms each time a shelf search or a screenshot opened a connection, while
// choosing and parsing the shelves cost 2 to 12 ms. Now the UI side only
// queues a URL and reads a finished slot, under a mutex held for a few copies;
// it never waits behind curl.
namespace {
constexpr int THUMB_SLOTS = 4;

// A slot as the UI sees it. `Queued`: handed over, not picked up yet;
// `Running`: the worker has it; `Done`: the answer waits to be taken.
enum class ThumbState : u8 { Free, Queued, Running, Done };
struct ThumbSlot {
    ThumbState  state  = ThumbState::Free;
    u32         gen    = 0;     // bumped by every start and cancel: a transfer
                                // the worker finishes for an older one is dropped
    bool        head   = false; // HEAD: only the Content-Length is wanted
    std::string url;
    std::string buf;            // the answer, moved in by the worker
    bool        ok     = false; // finished successfully (CURLE_OK + 2xx/3xx)
    long long   length = -1;    // Content-Length of the answer, -1 when not given
};
ThumbSlot g_thumb[THUMB_SLOTS];
Mutex     g_thumb_mutex;
CondVar   g_thumb_wake;         // the worker sleeps on it when it has nothing
bool      g_thumb_quit    = false;
bool      g_thumb_started = false;
Thread    g_thumb_thread;

// Worker side only: the transfer each slot runs, if any.
struct ThumbXfer {
    CURL*       handle = nullptr;
    u32         gen    = 0;
    std::string buf;
};

void thumb_xfer_drop(CURLM* multi, ThumbXfer& x) {
    if (x.handle) {
        curl_multi_remove_handle(multi, x.handle);
        curl_easy_cleanup(x.handle);
        x.handle = nullptr;
    }
    x.buf.clear();
}

void thumb_worker(void*) {
    CURLM* multi = curl_multi_init();
    ThumbXfer xfer[THUMB_SLOTS];
    while (true) {
        // What the UI asked for since the last pass, and what it took back.
        struct Start { int slot; u32 gen; std::string url; bool head; };
        Start starts[THUMB_SLOTS];
        int n_start = 0;
        bool drop[THUMB_SLOTS] = {};
        mutexLock(&g_thumb_mutex);
        if (g_thumb_quit) {
            mutexUnlock(&g_thumb_mutex);
            break;
        }
        for (int i = 0; i < THUMB_SLOTS; i++) {
            ThumbSlot& s = g_thumb[i];
            if (xfer[i].handle && (s.gen != xfer[i].gen || s.state != ThumbState::Running)) {
                drop[i] = true;
            }
            if (s.state == ThumbState::Queued) {
                starts[n_start++] = {i, s.gen, s.url, s.head};
                s.state = ThumbState::Running;
            }
        }
        mutexUnlock(&g_thumb_mutex);

        for (int i = 0; i < THUMB_SLOTS; i++) {
            if (drop[i]) thumb_xfer_drop(multi, xfer[i]);
        }
        for (int k = 0; k < n_start; k++) {
            const Start& st = starts[k];
            ThumbXfer& x = xfer[st.slot];
            thumb_xfer_drop(multi, x);
            CURL* c = multi ? curl_easy_init() : nullptr;
            if (!c) {
                mutexLock(&g_thumb_mutex);
                if (g_thumb[st.slot].gen == st.gen) {
                    g_thumb[st.slot].ok = false;
                    g_thumb[st.slot].state = ThumbState::Done;
                }
                mutexUnlock(&g_thumb_mutex);
                continue;
            }
            curl_easy_setopt(c, CURLOPT_URL, st.url.c_str());
            curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/2.0.0 (Nintendo Switch homebrew)");
            curl_easy_setopt(c, CURLOPT_FOLLOWLOCATION, 1L);
            curl_easy_setopt(c, CURLOPT_MAXREDIRS, 5L);
            curl_easy_setopt(c, CURLOPT_CAINFO, CACERT_PATH);
            curl_easy_setopt(c, CURLOPT_WRITEFUNCTION, write_to_string);
            curl_easy_setopt(c, CURLOPT_WRITEDATA, &x.buf);
            curl_easy_setopt(c, CURLOPT_TIMEOUT, 20L);
            curl_easy_setopt(c, CURLOPT_CONNECTTIMEOUT, 8L);
            // A thread of our own, so no alarm-based DNS timeout either.
            curl_easy_setopt(c, CURLOPT_NOSIGNAL, 1L);
            if (st.head) curl_easy_setopt(c, CURLOPT_NOBODY, 1L);
            curl_multi_add_handle(multi, c);
            x.handle = c;
            x.gen = st.gen;
        }

        bool busy = false;
        if (multi) {
            int running = 0;
            curl_multi_perform(multi, &running);
            CURLMsg* msg;
            int left = 0;
            while ((msg = curl_multi_info_read(multi, &left))) {
                if (msg->msg != CURLMSG_DONE) continue;
                for (int i = 0; i < THUMB_SLOTS; i++) {
                    ThumbXfer& x = xfer[i];
                    if (x.handle != msg->easy_handle) continue;
                    long code = 0;
                    curl_easy_getinfo(x.handle, CURLINFO_RESPONSE_CODE, &code);
                    const bool ok = (msg->data.result == CURLE_OK) && code >= 200 && code < 400;
                    curl_off_t cl = -1;
                    if (ok) curl_easy_getinfo(x.handle, CURLINFO_CONTENT_LENGTH_DOWNLOAD_T, &cl);
                    curl_multi_remove_handle(multi, x.handle);
                    curl_easy_cleanup(x.handle);
                    x.handle = nullptr;
                    mutexLock(&g_thumb_mutex);
                    ThumbSlot& s = g_thumb[i];
                    if (s.gen == x.gen && s.state == ThumbState::Running) {
                        s.buf.swap(x.buf);
                        s.ok = ok;
                        s.length = ok ? (long long)cl : -1;
                        s.state = ThumbState::Done;
                    }
                    mutexUnlock(&g_thumb_mutex);
                    x.buf.clear();
                    break;
                }
            }
            for (int i = 0; i < THUMB_SLOTS; i++) busy = busy || xfer[i].handle;
        }

        if (busy) {
            // Woken by the sockets; a new request waits at most this long.
            int numfds = 0;
            curl_multi_wait(multi, nullptr, 0, 20, &numfds);
        } else {
            mutexLock(&g_thumb_mutex);
            bool queued = false;
            for (int i = 0; i < THUMB_SLOTS; i++) queued = queued || g_thumb[i].state == ThumbState::Queued;
            if (!queued && !g_thumb_quit) condvarWait(&g_thumb_wake, &g_thumb_mutex);
            mutexUnlock(&g_thumb_mutex);
        }
    }
    for (int i = 0; i < THUMB_SLOTS; i++) thumb_xfer_drop(multi, xfer[i]);
    if (multi) curl_multi_cleanup(multi);
}

// Start the worker once. Core 2 below the audio worker and the save writer: it
// spends its time on the network, and a TLS handshake must never delay a sound
// buffer or a save. 256 KB of stack for mbedtls' handshake.
bool thumb_worker_start() {
    if (g_thumb_started) return true;
    mutexInit(&g_thumb_mutex);
    condvarInit(&g_thumb_wake);
    g_thumb_quit = false;
    Result rc = threadCreate(&g_thumb_thread, thumb_worker, nullptr, nullptr, 256 * 1024, 0x2F, 2);
    if (R_FAILED(rc)) rc = threadCreate(&g_thumb_thread, thumb_worker, nullptr, nullptr, 256 * 1024, 0x2F, -2);
    if (R_FAILED(rc)) {
        std::printf("net: thumbnail worker threadCreate failed 0x%x\n", (unsigned)rc);
        std::fflush(stdout);
        return false;
    }
    threadStart(&g_thumb_thread);
    g_thumb_started = true;
    return true;
}

// Queue a transfer in a free slot: a GET, or with `head` a HEAD, whose answer
// is only its Content-Length (https_thumb_slot_take_length).
int thumb_start(const char* url, bool head) {
    if (!url || net_init() != 0) return -1;
    if (!thumb_worker_start()) return -1;
    mutexLock(&g_thumb_mutex);
    int slot = -1;
    for (int i = 0; i < THUMB_SLOTS; i++) {
        if (g_thumb[i].state == ThumbState::Free) { slot = i; break; }
    }
    if (slot >= 0) {
        ThumbSlot& s = g_thumb[slot];
        s.gen++;
        s.state = ThumbState::Queued;
        s.head = head;
        s.url = url;
        s.buf.clear();
        s.ok = false;
        s.length = -1;
        condvarWakeOne(&g_thumb_wake);
    }
    mutexUnlock(&g_thumb_mutex);
    return slot >= 0 ? slot : -5;
}

// Free a slot (lock held): its answer goes, and a transfer still running for
// it is dropped by the worker on its next pass.
void thumb_slot_free_locked(int i) {
    g_thumb[i].gen++;
    g_thumb[i].state = ThumbState::Free;
    g_thumb[i].url.clear();
    g_thumb[i].buf.clear();
    g_thumb[i].buf.shrink_to_fit();
    g_thumb[i].ok = false;
    g_thumb[i].length = -1;
}
} // namespace

// Start a thumbnail GET in a free slot. Returns the slot index (>=0), -5 if the
// pool is full (all slots busy/unread), -1 on init error.
extern "C" int https_thumb_start(const char* url) {
    return thumb_start(url, false);
}

// The same as a HEAD, in the same pool: the size of what is at `url` without
// fetching it, read with https_thumb_slot_take_length once the slot is done.
// The Flashpoint details window's download size. It was a blocking HEAD, which
// froze the window, its spinner and its buttons for as long as the server took
// to answer (up to its 15 s timeout on a bad connection).
extern "C" int https_thumb_start_head(const char* url) {
    return thumb_start(url, true);
}

// Kept for the callers that pump once per frame: the worker runs the transfers
// now, so this only says how many are still queued or running.
extern "C" int https_thumb_tick(void) {
    if (!g_thumb_started) return 0;
    int n = 0;
    mutexLock(&g_thumb_mutex);
    for (int i = 0; i < THUMB_SLOTS; i++) {
        if (g_thumb[i].state == ThumbState::Queued || g_thumb[i].state == ThumbState::Running) n++;
    }
    mutexUnlock(&g_thumb_mutex);
    return n;
}

// Poll one slot: 1 = done OK, -2 = done error, 0 = in flight, -1 = invalid/free.
extern "C" int https_thumb_slot_status(int slot) {
    if (slot < 0 || slot >= THUMB_SLOTS || !g_thumb_started) return -1;
    mutexLock(&g_thumb_mutex);
    int rc;
    switch (g_thumb[slot].state) {
        case ThumbState::Queued:
        case ThumbState::Running: rc = 0; break;
        case ThumbState::Done:    rc = g_thumb[slot].ok ? 1 : -2; break;
        default:                  rc = -1; break;
    }
    mutexUnlock(&g_thumb_mutex);
    return rc;
}

// Copy a done slot's bytes into `out` and FREE the slot. Returns bytes copied,
// -3 if it doesn't fit `cap`, -2 if the slot errored, -1 invalid. Call once
// after https_thumb_slot_status(slot) != 0.
extern "C" int https_thumb_slot_take(int slot, char* out, int cap) {
    if (slot < 0 || slot >= THUMB_SLOTS || !out || cap < 1 || !g_thumb_started) return -1;
    mutexLock(&g_thumb_mutex);
    int rc;
    if (g_thumb[slot].state != ThumbState::Done) {
        rc = -1;
    } else if (!g_thumb[slot].ok) {
        rc = -2;
        thumb_slot_free_locked(slot);
    } else {
        const int n = (int)g_thumb[slot].buf.size();
        if (n > cap) {
            rc = -3;
        } else {
            std::memcpy(out, g_thumb[slot].buf.data(), (size_t)n);
            rc = n;
        }
        thumb_slot_free_locked(slot);
    }
    mutexUnlock(&g_thumb_mutex);
    return rc;
}

// A done HEAD's answer: the Content-Length it found (-1 when the server gave
// none, or on an error), and the slot FREED, as https_thumb_slot_take does.
extern "C" long long https_thumb_slot_take_length(int slot) {
    if (slot < 0 || slot >= THUMB_SLOTS || !g_thumb_started) return -1;
    mutexLock(&g_thumb_mutex);
    long long len = -1;
    if (g_thumb[slot].state == ThumbState::Done) {
        len = g_thumb[slot].ok ? g_thumb[slot].length : -1;
        thumb_slot_free_locked(slot);
    }
    mutexUnlock(&g_thumb_mutex);
    return len;
}

// Every slot freed at once (a page closed, the launcher leaving). The worker
// drops what it was running for them on its next pass, within 20 ms.
extern "C" void https_thumb_cancel(void) {
    if (!g_thumb_started) return;
    mutexLock(&g_thumb_mutex);
    for (int i = 0; i < THUMB_SLOTS; i++) thumb_slot_free_locked(i);
    condvarWakeOne(&g_thumb_wake);
    mutexUnlock(&g_thumb_mutex);
}

// Stop the worker and wait for it: before the sockets go at exit, so no TLS
// handshake is left half-way in a socket that no longer exists.
extern "C" void https_thumb_stop(void) {
    if (!g_thumb_started) return;
    mutexLock(&g_thumb_mutex);
    for (int i = 0; i < THUMB_SLOTS; i++) thumb_slot_free_locked(i);
    g_thumb_quit = true;
    condvarWakeOne(&g_thumb_wake);
    mutexUnlock(&g_thumb_mutex);
    threadWaitForExit(&g_thumb_thread);
    threadClose(&g_thumb_thread);
    g_thumb_started = false;
}

// swkbd counts its length limit in CHARACTERS; our buffers are sized in BYTES.
// While every prompt was Latin-only the two were interchangeable, so the limit
// was simply `cap - 1`. With the CJK keyboards enabled (issue #75) a character
// is up to four bytes, and that limit becomes a promise the buffer cannot keep:
// libnx truncates the UTF-16 to UTF-8 conversion to fit, and the Rust side then
// hands a cut string to String::from_utf8, which drops the entire entry. Budget
// the UTF-8 maximum per character instead, so anything the keyboard accepts
// comes back whole. Callers size their buffers 4x the character count they want.
static u32 swkbd_char_limit(int cap) {
    u32 lim = (u32)((cap - 1) / 4);
    return lim < 1 ? 1 : lim;
}

// Generic swkbd prompt for a display name (Phase 3.4.bis RENOMMER). Pre-
// fills with `initial` if non-NULL, otherwise empty. The header / guide
// strings are RENOMMER-specific; if we end up needing yet another swkbd
// flavour we should refactor to a single `swkbd_prompt_text(...)` that
// takes them as params. For now two specialised helpers are fine.
extern "C" int swkbd_prompt_rename(const char* header, const char* guide, const char* initial, char* out, int cap) {
    if (cap < 2) return -1;
    SwkbdConfig kbd;
    Result rc = swkbdCreate(&kbd, 0);
    if (R_FAILED(rc)) {
        std::printf("swkbd_prompt_rename: swkbdCreate failed 0x%x\n", rc);
        std::fflush(stdout);
        return -1;
    }
    swkbdConfigMakePresetDefault(&kbd);
    // SwkbdType_All, not QWERTY: the preset's QWERTY means "Latin keyboard
    // only", with no way to reach the Chinese / Korean input methods even on a
    // console whose system language is Chinese. A player renaming a Chinese
    // game could only type its name in Latin (issue #75). The launcher renders
    // whatever comes back: draw_text sends every codepoint the 5x7 bitmap font
    // lacks to the shared-font atlas (backend/glyphs.rs).
    swkbdConfigSetType(&kbd, SwkbdType_All);
    if (header && *header) swkbdConfigSetHeaderText(&kbd, header);
    if (guide && *guide) swkbdConfigSetGuideText(&kbd, guide);
    if (initial && *initial) {
        swkbdConfigSetInitialText(&kbd, initial);
    }
    swkbdConfigSetStringLenMax(&kbd, swkbd_char_limit(cap));
    rc = swkbdShow(&kbd, out, (size_t)cap);
    swkbdClose(&kbd);
    if (R_FAILED(rc)) {
        std::printf("swkbd_prompt_rename: swkbdShow rc=0x%x\n", rc);
        std::fflush(stdout);
        return -1;
    }
    return 0;
}

// `initial` may be NULL or empty: the field then opens EMPTY. It used to fall
// back to "https://archive.org/download/", which hid the guide text (swkbd
// shows it only in an empty field), pointed at archive.org alone, and glued
// that prefix to a game's name typed after it, so the importer could no longer
// tell a name from an address.
extern "C" int swkbd_prompt_url(const char* header, const char* guide, const char* initial, char* out, int cap) {
    if (cap < 2) return -1;
    SwkbdConfig kbd;
    Result rc = swkbdCreate(&kbd, 0);
    if (R_FAILED(rc)) {
        std::printf("swkbd_prompt_url: swkbdCreate failed 0x%x\n", rc);
        std::fflush(stdout);
        return -1;
    }
    swkbdConfigMakePresetDefault(&kbd);
    // Stays QWERTY while the others moved to SwkbdType_All (issue #75): a URL
    // typed here goes straight into an HTTP request, and an input method that
    // composes Chinese would only produce a link no server can answer. The
    // Latin keyboards still carry the accented letters an IDN might need.
    swkbdConfigSetType(&kbd, SwkbdType_QWERTY);
    if (guide && *guide) swkbdConfigSetGuideText(&kbd, guide);
    if (header && *header) swkbdConfigSetHeaderText(&kbd, header);
    if (initial && *initial) swkbdConfigSetInitialText(&kbd, initial);
    swkbdConfigSetStringLenMax(&kbd, swkbd_char_limit(cap));
    rc = swkbdShow(&kbd, out, (size_t)cap);
    swkbdClose(&kbd);
    if (R_FAILED(rc)) {
        std::printf("swkbd_prompt_url: swkbdShow rc=0x%x\n", rc);
        std::fflush(stdout);
        return -1;
    }
    return 0;
}

// Search prompt used by the DistantFiles list (X button). Empty input
// clears the filter — caller decides what that means.
extern "C" int swkbd_prompt_search(const char* header, const char* guide, const char* initial, char* out, int cap) {
    if (cap < 2) return -1;
    SwkbdConfig kbd;
    Result rc = swkbdCreate(&kbd, 0);
    if (R_FAILED(rc)) {
        std::printf("swkbd_prompt_search: swkbdCreate failed 0x%x\n", rc);
        std::fflush(stdout);
        return -1;
    }
    swkbdConfigMakePresetDefault(&kbd);
    // All keyboards (issue #75). This helper serves the search filter, the
    // nickname and the bug-report description, and every one of them can
    // legitimately be written in a non-Latin script.
    swkbdConfigSetType(&kbd, SwkbdType_All);
    if (header && *header) swkbdConfigSetHeaderText(&kbd, header);
    if (guide && *guide) swkbdConfigSetGuideText(&kbd, guide);
    if (initial && *initial) {
        swkbdConfigSetInitialText(&kbd, initial);
    }
    swkbdConfigSetStringLenMax(&kbd, swkbd_char_limit(cap));
    rc = swkbdShow(&kbd, out, (size_t)cap);
    swkbdClose(&kbd);
    if (R_FAILED(rc)) {
        std::printf("swkbd_prompt_search: swkbdShow rc=0x%x\n", rc);
        std::fflush(stdout);
        return -1;
    }
    return 0;
}

// In-game text entry for a focused Flash TextField. Configured from the field's
// own properties (queried via ruffle_keyboard_field): `flags` bit0 = password,
// bit1 = multiline, bit2 = numeric (digits-only restrict); `maxlen` = the
// field's max char count (0 = unlimited). Pre-fills with `initial` (the field's
// current text) so the user edits in place. `header` is the localized title
// (ruffle_keyboard_header), like every other keyboard's. Writes the entered
// string into `out` and returns 0 on accept, -1 on cancel or applet error
// (field unchanged).
extern "C" int swkbd_prompt_game_field(const char* header, const char* initial, int flags, int maxlen, char* out, int cap) {
    if (cap < 2) return -1;
    SwkbdConfig kbd;
    Result rc = swkbdCreate(&kbd, 0);
    if (R_FAILED(rc)) {
        std::printf("swkbd_prompt_game_field: swkbdCreate failed 0x%x\n", rc);
        std::fflush(stdout);
        return -1;
    }
    swkbdConfigMakePresetDefault(&kbd);
    // All keyboards for free text (issue #75). A game's own text fields can be
    // Chinese too, and Ruffle can draw the result: SHARED_DEVICE_FONTS in
    // backend/ui.rs hands it the Switch CJK fonts as device fonts (issue #54).
    // Numeric fields keep the number pad, which has no script to choose.
    swkbdConfigSetType(&kbd, (flags & 4) ? SwkbdType_NumPad : SwkbdType_All);
    if (flags & 1) {
        // Masked entry for password fields.
        swkbdConfigSetPasswordFlag(&kbd, 1);
    }
    if (flags & 2) {
        // Multiline fields: let the return key insert a newline instead of
        // submitting, so the user can enter multi-line text.
        swkbdConfigSetReturnButtonFlag(&kbd, 1);
    }
    if (header && *header) swkbdConfigSetHeaderText(&kbd, header);
    if (initial && *initial) {
        swkbdConfigSetInitialText(&kbd, initial);
    }
    // Cap to the field's max (when set) and always to our buffer size. The
    // field's own maximum is a character count, which is exactly what swkbd
    // wants; ours is a byte budget, hence swkbd_char_limit.
    u32 lim = swkbd_char_limit(cap);
    if (maxlen > 0 && (u32)maxlen < lim) lim = (u32)maxlen;
    swkbdConfigSetStringLenMax(&kbd, lim);
    rc = swkbdShow(&kbd, out, (size_t)cap);
    swkbdClose(&kbd);
    if (R_FAILED(rc)) {
        // Most commonly the user cancelled — leave the field as it was.
        return -1;
    }
    return 0;
}
