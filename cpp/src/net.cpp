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
#include <sys/stat.h>

#include <switch.h>
#include <curl/curl.h>

namespace {

constexpr const char* CACERT_PATH = "sdmc:/switch/flash-for-switch/cacert.pem";

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

void multi_cleanup(bool delete_partial) {
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
    ::mkdir("sdmc:/switch/flash-for-switch", 0755);
    struct stat st;
    if (::stat(CACERT_PATH, &st) == 0 && (int)st.st_size == len) {
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
    curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/0.0.1 (Nintendo Switch homebrew)");
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
        std::printf("https_get %s: curl=%d (%s) http=%ld\n",
                    url, (int)res, curl_easy_strerror(res), http_code);
        std::fflush(stdout);
        return -2;
    }
    buf[w.pos] = '\0';
    return w.pos;
}

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
    curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/0.0.1 (Nintendo Switch homebrew)");
    curl_easy_setopt(c, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(c, CURLOPT_MAXREDIRS, 5L);
    curl_easy_setopt(c, CURLOPT_CAINFO, CACERT_PATH);
    curl_easy_setopt(c, CURLOPT_WRITEDATA, f);   // default WRITEFUNCTION = fwrite
    curl_easy_setopt(c, CURLOPT_XFERINFOFUNCTION, xfer_progress);
    curl_easy_setopt(c, CURLOPT_NOPROGRESS, 0L);
    curl_easy_setopt(c, CURLOPT_TIMEOUT, 600L);
    curl_easy_setopt(c, CURLOPT_CONNECTTIMEOUT, 10L);

    curl_multi_add_handle(m, c);

    g_multi = m;
    g_handle = c;
    g_dl_file = f;
    g_bytes_done = 0;
    g_bytes_total = 0;
    g_last_curl_result = 0;
    g_last_http_code = 0;
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
    CURLMcode mc = curl_multi_perform(g_multi, &still_running);
    if (mc != CURLM_OK) {
        std::printf("https_download_tick: curl_multi_perform mc=%d\n", (int)mc);
        std::fflush(stdout);
        multi_cleanup(true);
        return -2;
    }
    if (still_running > 0) return 0;

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
    std::printf("https_download_tick: OK %ld bytes -> %s\n",
                (long)g_bytes_done, g_dl_out_path);
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

// Generic swkbd prompt for a display name (Phase 3.4.bis RENOMMER). Pre-
// fills with `initial` if non-NULL, otherwise empty. The header / guide
// strings are RENOMMER-specific; if we end up needing yet another swkbd
// flavour we should refactor to a single `swkbd_prompt_text(...)` that
// takes them as params. For now two specialised helpers are fine.
extern "C" int swkbd_prompt_rename(const char* initial, char* out, int cap) {
    if (cap < 2) return -1;
    SwkbdConfig kbd;
    Result rc = swkbdCreate(&kbd, 0);
    if (R_FAILED(rc)) {
        std::printf("swkbd_prompt_rename: swkbdCreate failed 0x%x\n", rc);
        std::fflush(stdout);
        return -1;
    }
    swkbdConfigMakePresetDefault(&kbd);
    swkbdConfigSetType(&kbd, SwkbdType_QWERTY);
    swkbdConfigSetHeaderText(&kbd, "FlashNX - Renommer le jeu");
    swkbdConfigSetGuideText(&kbd, "Nom d'affichage (laisser vide pour revenir au nom de fichier)");
    if (initial && *initial) {
        swkbdConfigSetInitialText(&kbd, initial);
    }
    swkbdConfigSetStringLenMax(&kbd, (u32)(cap - 1));
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
extern "C" int swkbd_prompt_url(const char* initial, char* out, int cap) {
    if (cap < 2) return -1;
    SwkbdConfig kbd;
    Result rc = swkbdCreate(&kbd, 0);
    if (R_FAILED(rc)) {
        std::printf("swkbd_prompt_url: swkbdCreate failed 0x%x\n", rc);
        std::fflush(stdout);
        return -1;
    }
    swkbdConfigMakePresetDefault(&kbd);
    swkbdConfigSetType(&kbd, SwkbdType_QWERTY);
    swkbdConfigSetGuideText(&kbd, "URL archive.org de l'item (ex: https://archive.org/download/banned-from-equestria-daily-1.5)");
    swkbdConfigSetHeaderText(&kbd, "FlashNX - Import distant");
    const char* prefill = (initial && *initial) ? initial : "https://archive.org/download/";
    swkbdConfigSetInitialText(&kbd, prefill);
    swkbdConfigSetStringLenMax(&kbd, (u32)(cap - 1));
    rc = swkbdShow(&kbd, out, (size_t)cap);
    swkbdClose(&kbd);
    if (R_FAILED(rc)) {
        std::printf("swkbd_prompt_url: swkbdShow rc=0x%x\n", rc);
        std::fflush(stdout);
        return -1;
    }
    return 0;
}
