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
}

int xfer_progress(void* /*ud*/, curl_off_t dltotal, curl_off_t dlnow,
                  curl_off_t /*ultotal*/, curl_off_t /*ulnow*/) {
    g_bytes_done = (uint64_t)dlnow;
    g_bytes_total = (uint64_t)dltotal;
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
    curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/1.8.0 (Nintendo Switch homebrew)");
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
    curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/1.8.0 (Nintendo Switch homebrew)");
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
extern "C" int https_download_start(const char* url, const char* out_path) {
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
    curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/1.8.0 (Nintendo Switch homebrew)");
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
    // Boost the CPU for the download (TLS decrypt + curl can be the limit on the
    // base clock). Reset by multi_cleanup when the transfer ends.
    appletSetCpuBoostMode(ApmCpuBoostMode_FastLoad);
    std::printf("https_download_start: %s -> %s\n", url, out_path);
    std::fflush(stdout);
    return 0;
}

// Pump the multi handle once. Returns:
//   0 = still in progress
//   1 = finished successfully
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
        std::printf("https_download_tick: failed curl=%d (%s) http=%ld\n",
                    g_last_curl_result, curl_easy_strerror((CURLcode)g_last_curl_result),
                    g_last_http_code);
        std::fflush(stdout);
        multi_cleanup(true);
        return -2;
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

extern "C" void https_download_cancel(void) {
    if (g_multi) {
        std::printf("https_download_cancel\n"); std::fflush(stdout);
        multi_cleanup(true);
    }
}

// ── Async in-memory GET (archive.org metadata, non-blocking) ──────────────
// Same curl-multi machinery as the download, but the response accumulates in
// g_get_buf instead of a file, so the UI keeps rendering (a spinner) while it
// runs. Returns: 0 on success (in progress), -1 init, -5 if busy.
extern "C" int https_get_start(const char* url) {
    // A GET already in flight is SUPERSEDED, not a reason to refuse.
    //
    // Refusing left the slot occupied forever: the caller that got -5 shows its
    // error and stops calling https_get_tick, so nothing ever cleans up the
    // handles of the fetch that was running, and every later request in the
    // session fails the same way. Measured on hardware: two Flashpoint searches
    // in a row (X pressed while the previous result list was still loading) and
    // the launcher had no network left at all -- searches, archive.org metadata,
    // everything.
    //
    // Superseding is also what the user means: a new search replaces the old
    // one, it does not queue behind it.
    if ((g_multi || g_handle) && !g_dl_file) {
        std::printf("https_get_start: superseding an in-flight GET\n");
        std::fflush(stdout);
        multi_cleanup(false);
        g_get_buf.clear();
    }
    // A DOWNLOAD owns the slot (it writes to g_dl_file and the caller shows a
    // progress bar). That one is not ours to cancel behind the user's back.
    if (g_multi || g_handle) {
        return -5;
    }
    if (net_init() != 0) return -1;
    CURLM* m = curl_multi_init();
    CURL* c = curl_easy_init();
    if (!m || !c) {
        if (m) curl_multi_cleanup(m);
        if (c) curl_easy_cleanup(c);
        return -1;
    }
    g_get_buf.clear();
    curl_easy_setopt(c, CURLOPT_URL, url);
    curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/1.8.0 (Nintendo Switch homebrew)");
    curl_easy_setopt(c, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(c, CURLOPT_MAXREDIRS, 5L);
    curl_easy_setopt(c, CURLOPT_CAINFO, CACERT_PATH);
    curl_easy_setopt(c, CURLOPT_WRITEFUNCTION, write_to_string);
    curl_easy_setopt(c, CURLOPT_WRITEDATA, &g_get_buf);
    curl_easy_setopt(c, CURLOPT_TIMEOUT, 60L);
    curl_easy_setopt(c, CURLOPT_CONNECTTIMEOUT, 10L);
    curl_multi_add_handle(m, c);
    g_multi = m;
    g_handle = c;
    g_dl_file = nullptr; // in-memory GET, not a file download
    g_bytes_done = 0;
    g_bytes_total = 0;
    g_last_curl_result = 0;
    g_last_http_code = 0;
    std::printf("https_get_start: %s\n", url);
    std::fflush(stdout);
    return 0;
}

// Pump the GET once. Returns: 0 = in progress, 1 = done (response in g_get_buf),
// <0 = error (handles cleaned up; g_get_buf left as-is).
extern "C" int https_get_tick(void) {
    if (!g_multi) return -1;
    int still_running = 0;
    CURLMcode mc = curl_multi_perform(g_multi, &still_running);
    if (mc != CURLM_OK) {
        std::printf("https_get_tick: curl_multi_perform mc=%d\n", (int)mc);
        std::fflush(stdout);
        multi_cleanup(false);
        return -2;
    }
    if (still_running > 0) return 0;

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
    multi_cleanup(false); // frees g_multi/g_handle (g_dl_file is null); keeps g_get_buf
    if (!ok) {
        std::printf("https_get_tick: failed curl=%d (%s) http=%ld\n",
                    g_last_curl_result, curl_easy_strerror((CURLcode)g_last_curl_result),
                    g_last_http_code);
        std::fflush(stdout);
        return -2;
    }
    std::printf("https_get_tick: OK %zu bytes\n", g_get_buf.size());
    std::fflush(stdout);
    return 1;
}

// Copy the completed response into `out` (NUL-terminated). Returns bytes copied,
// or -3 if the response doesn't fit in `cap`. Call after `https_get_tick` == 1.
extern "C" int https_get_buffer(char* out, int cap) {
    if (!out || cap < 1) return -1;
    const int n = (int)g_get_buf.size();
    if (n >= cap) return -3;
    std::memcpy(out, g_get_buf.data(), (size_t)n);
    out[n] = '\0';
    return n;
}

extern "C" void https_get_cancel(void) {
    if (g_multi) {
        std::printf("https_get_cancel\n"); std::fflush(stdout);
        multi_cleanup(false);
    }
    g_get_buf.clear();
}

// ── Async thumbnail GET (Flashpoint logos, non-blocking, PARALLEL) ─────────
// A SECOND, isolated curl-multi handle dedicated to cover/logo thumbnails, with
// a POOL of slots so several logos download CONCURRENTLY (curl_multi naturally
// runs them in parallel) instead of one-at-a-time. It deliberately does NOT
// touch g_multi/g_handle (the archive.org metadata + download handle). The
// FpGallery / cover-picker render starts a fetch per free slot, pumps
// `https_thumb_tick` once per frame, and takes each finished slot, so the grid
// fills several covers at a time without ever blocking the UI.
namespace {
constexpr int THUMB_SLOTS = 4;
CURLM* g_thumb_multi = nullptr;
struct ThumbSlot {
    CURL*       handle = nullptr;
    std::string buf;
    long        http_code = 0;
    bool        active = false; // a transfer is in flight on this slot
    bool        done   = false; // finished; bytes ready to take
    bool        ok     = false; // finished successfully (CURLE_OK + 2xx/3xx)
};
ThumbSlot g_thumb[THUMB_SLOTS];

void thumb_slot_free(int i) {
    if (g_thumb[i].handle) {
        if (g_thumb_multi) curl_multi_remove_handle(g_thumb_multi, g_thumb[i].handle);
        curl_easy_cleanup(g_thumb[i].handle);
        g_thumb[i].handle = nullptr;
    }
    g_thumb[i].buf.clear();
    g_thumb[i].http_code = 0;
    g_thumb[i].active = false;
    g_thumb[i].done = false;
    g_thumb[i].ok = false;
}
} // namespace

// Start a thumbnail GET in a free slot. Returns the slot index (>=0), -5 if the
// pool is full (all slots busy/unread), -1 on init error.
extern "C" int https_thumb_start(const char* url) {
    if (net_init() != 0) return -1;
    if (!g_thumb_multi) {
        g_thumb_multi = curl_multi_init();
        if (!g_thumb_multi) return -1;
    }
    int slot = -1;
    for (int i = 0; i < THUMB_SLOTS; i++) {
        if (!g_thumb[i].active && !g_thumb[i].done) { slot = i; break; }
    }
    if (slot < 0) return -5;
    CURL* c = curl_easy_init();
    if (!c) return -1;
    g_thumb[slot].buf.clear();
    curl_easy_setopt(c, CURLOPT_URL, url);
    curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/1.8.0 (Nintendo Switch homebrew)");
    curl_easy_setopt(c, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(c, CURLOPT_MAXREDIRS, 5L);
    curl_easy_setopt(c, CURLOPT_CAINFO, CACERT_PATH);
    curl_easy_setopt(c, CURLOPT_WRITEFUNCTION, write_to_string);
    curl_easy_setopt(c, CURLOPT_WRITEDATA, &g_thumb[slot].buf);
    curl_easy_setopt(c, CURLOPT_TIMEOUT, 20L);
    curl_easy_setopt(c, CURLOPT_CONNECTTIMEOUT, 8L);
    curl_multi_add_handle(g_thumb_multi, c);
    g_thumb[slot].handle = c;
    g_thumb[slot].active = true;
    g_thumb[slot].done = false;
    g_thumb[slot].ok = false;
    g_thumb[slot].http_code = 0;
    return slot;
}

// Pump ALL in-flight thumbnail transfers once; mark completed slots `done`
// (their bytes wait in the slot until taken). Returns the number still active.
extern "C" int https_thumb_tick(void) {
    if (!g_thumb_multi) return 0;
    int still_running = 0;
    if (curl_multi_perform(g_thumb_multi, &still_running) != CURLM_OK) {
        return still_running;
    }
    CURLMsg* msg;
    int left = 0;
    while ((msg = curl_multi_info_read(g_thumb_multi, &left))) {
        if (msg->msg != CURLMSG_DONE) continue;
        for (int i = 0; i < THUMB_SLOTS; i++) {
            if (g_thumb[i].handle != msg->easy_handle) continue;
            curl_easy_getinfo(g_thumb[i].handle, CURLINFO_RESPONSE_CODE, &g_thumb[i].http_code);
            g_thumb[i].ok = (msg->data.result == CURLE_OK)
                && (g_thumb[i].http_code >= 200) && (g_thumb[i].http_code < 400);
            curl_multi_remove_handle(g_thumb_multi, g_thumb[i].handle);
            curl_easy_cleanup(g_thumb[i].handle);
            g_thumb[i].handle = nullptr;
            g_thumb[i].active = false;
            g_thumb[i].done = true; // keep buf for the take below
            break;
        }
    }
    return still_running;
}

// Poll one slot: 1 = done OK, -2 = done error, 0 = in flight, -1 = invalid/free.
extern "C" int https_thumb_slot_status(int slot) {
    if (slot < 0 || slot >= THUMB_SLOTS) return -1;
    if (g_thumb[slot].active) return 0;
    if (g_thumb[slot].done) return g_thumb[slot].ok ? 1 : -2;
    return -1;
}

// Copy a done slot's bytes into `out` and FREE the slot. Returns bytes copied,
// -3 if it doesn't fit `cap`, -2 if the slot errored, -1 invalid. Call once
// after https_thumb_slot_status(slot) != 0.
extern "C" int https_thumb_slot_take(int slot, char* out, int cap) {
    if (slot < 0 || slot >= THUMB_SLOTS || !out || cap < 1) return -1;
    if (!g_thumb[slot].done) return -1;
    int rc;
    if (!g_thumb[slot].ok) {
        rc = -2;
    } else {
        const int n = (int)g_thumb[slot].buf.size();
        if (n > cap) {
            rc = -3;
        } else {
            std::memcpy(out, g_thumb[slot].buf.data(), (size_t)n);
            rc = n;
        }
    }
    thumb_slot_free(slot);
    return rc;
}

extern "C" void https_thumb_cancel(void) {
    for (int i = 0; i < THUMB_SLOTS; i++) thumb_slot_free(i);
    if (g_thumb_multi) {
        curl_multi_cleanup(g_thumb_multi);
        g_thumb_multi = nullptr;
    }
}

// Synchronous HEAD: return the Content-Length of `url` (following redirects),
// or -1 if unavailable. Used by the Flashpoint details popup to show a game's
// download size without fetching the whole GameZIP. Blocks ~a few hundred ms.
extern "C" long long https_head_content_length(const char* url) {
    if (net_init() != 0) return -1;
    CURL* c = curl_easy_init();
    if (!c) return -1;
    curl_easy_setopt(c, CURLOPT_URL, url);
    curl_easy_setopt(c, CURLOPT_NOBODY, 1L); // HEAD request
    curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/1.8.0 (Nintendo Switch homebrew)");
    curl_easy_setopt(c, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(c, CURLOPT_MAXREDIRS, 5L);
    curl_easy_setopt(c, CURLOPT_CAINFO, CACERT_PATH);
    curl_easy_setopt(c, CURLOPT_TIMEOUT, 15L);
    curl_easy_setopt(c, CURLOPT_CONNECTTIMEOUT, 8L);
    CURLcode res = curl_easy_perform(c);
    long long len = -1;
    if (res == CURLE_OK) {
        curl_off_t cl = -1;
        curl_easy_getinfo(c, CURLINFO_CONTENT_LENGTH_DOWNLOAD_T, &cl);
        if (cl > 0) len = (long long)cl;
    } else {
        std::printf("https_head %s: curl=%d (%s)\n", url, (int)res, curl_easy_strerror(res));
        std::fflush(stdout);
    }
    curl_easy_cleanup(c);
    return len;
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

// `initial` may be NULL or empty — falls back to a sensible default so
// the user lands on a usable prefix even on the very first import.
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
    const char* prefill = (initial && *initial) ? initial : "https://archive.org/download/";
    swkbdConfigSetInitialText(&kbd, prefill);
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
// current text) so the user edits in place. Writes the entered string into
// `out` and returns 0 on accept, -1 on cancel or applet error (field unchanged).
extern "C" int swkbd_prompt_game_field(const char* initial, int flags, int maxlen, char* out, int cap) {
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
    swkbdConfigSetHeaderText(&kbd, "Text input");
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
