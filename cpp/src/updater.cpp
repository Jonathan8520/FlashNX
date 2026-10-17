// FlashNX updating itself: the C++ half of rust/src/updater.rs.
//
// Three pieces, each one the Rust side cannot do well on Horizon:
//
//   - asking GitHub for the latest release, on a thread of its own. Not the
//     request worker of net.cpp: that one holds one request at a time and a new
//     one supersedes it, so a check at boot would cancel the importer's shelves,
//     or be cancelled by the first search;
//   - the SHA-256 of the downloaded file (mbedTLS is linked already, for curl);
//   - putting the new `.nro` in place of the running one. The running file can
//     be replaced: nx-hbloader read all of it into memory before starting it,
//     and a normal build has no RomFS, so nothing keeps it open (libnx closes
//     the file when it finds no RomFS). A self-contained build (#106) has one
//     and is never updated in place.

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <sys/stat.h>

#include <switch.h>
#include <curl/curl.h>
#include <mbedtls/sha256.h>

extern "C" int net_init(void);

namespace {

constexpr const char* CACERT_PATH = "sdmc:/switch/FlashNX/cacert.pem";
// A release's JSON is a few kilobytes; past this it is not what was asked for.
constexpr size_t CHECK_MAX = 256u * 1024;

enum class CheckState : int { Idle = 0, Running, Done, Failed };

Thread      g_check_thread;
bool        g_check_thread_live = false;
volatile int g_check_state = (int)CheckState::Idle;
volatile bool g_check_abort = false;
std::string g_check_url;
std::string g_check_out;
long        g_check_http = 0;
int         g_check_curl = 0;

size_t check_write(char* ptr, size_t size, size_t nmemb, void* userdata) {
    std::string* out = (std::string*)userdata;
    const size_t n = size * nmemb;
    if (out->size() + n > CHECK_MAX) return 0;
    out->append(ptr, n);
    return n;
}

int check_progress(void*, curl_off_t, curl_off_t, curl_off_t, curl_off_t) {
    return g_check_abort ? 1 : 0;
}

void check_worker(void*) {
    std::string out;
    CURLcode res = CURLE_FAILED_INIT;
    long http = 0;
    CURL* c = curl_easy_init();
    struct curl_slist* headers = nullptr;
    if (c) {
        // GitHub's API refuses a request without a User-Agent.
        headers = curl_slist_append(headers, "Accept: application/vnd.github+json");
        headers = curl_slist_append(headers, "X-GitHub-Api-Version: 2022-11-28");
        curl_easy_setopt(c, CURLOPT_URL, g_check_url.c_str());
        curl_easy_setopt(c, CURLOPT_USERAGENT, "FlashNX/" FLASHNX_VERSION " (Nintendo Switch homebrew)");
        curl_easy_setopt(c, CURLOPT_HTTPHEADER, headers);
        curl_easy_setopt(c, CURLOPT_FOLLOWLOCATION, 1L);
        curl_easy_setopt(c, CURLOPT_MAXREDIRS, 5L);
        curl_easy_setopt(c, CURLOPT_CAINFO, CACERT_PATH);
        curl_easy_setopt(c, CURLOPT_WRITEFUNCTION, check_write);
        curl_easy_setopt(c, CURLOPT_WRITEDATA, &out);
        curl_easy_setopt(c, CURLOPT_TIMEOUT, 30L);
        curl_easy_setopt(c, CURLOPT_CONNECTTIMEOUT, 10L);
        curl_easy_setopt(c, CURLOPT_NOSIGNAL, 1L);
        curl_easy_setopt(c, CURLOPT_XFERINFOFUNCTION, check_progress);
        curl_easy_setopt(c, CURLOPT_NOPROGRESS, 0L);
        res = curl_easy_perform(c);
        curl_easy_getinfo(c, CURLINFO_RESPONSE_CODE, &http);
        curl_easy_cleanup(c);
        curl_slist_free_all(headers);
    }
    g_check_http = http;
    g_check_curl = (int)res;
    if (res == CURLE_OK && http >= 200 && http < 300) {
        g_check_out.swap(out);
        g_check_state = (int)CheckState::Done;
    } else {
        std::printf("update: check failed, curl=%d (%s) http=%ld\n", (int)res, curl_easy_strerror(res),
                    http);
        std::fflush(stdout);
        g_check_state = (int)CheckState::Failed;
    }
}

void check_join() {
    if (!g_check_thread_live) return;
    threadWaitForExit(&g_check_thread);
    threadClose(&g_check_thread);
    g_check_thread_live = false;
}

} // namespace

// Ask `url` (the latest release, GitHub's API) on a thread of its own. 0 when
// it started, -1 when it could not, -5 while a check is still running.
extern "C" int flashnx_update_check_start(const char* url) {
    if (!url) return -1;
    if (g_check_state == (int)CheckState::Running) return -5;
    check_join();
    if (net_init() != 0) return -1;
    g_check_url = url;
    g_check_out.clear();
    g_check_abort = false;
    g_check_state = (int)CheckState::Running;
    // Beside net.cpp's request worker: core 2, below the audio worker, 256 KB of
    // stack for mbedTLS' handshake.
    Result rc = threadCreate(&g_check_thread, check_worker, nullptr, nullptr, 256 * 1024, 0x2F, 2);
    if (R_FAILED(rc)) rc = threadCreate(&g_check_thread, check_worker, nullptr, nullptr, 256 * 1024, 0x2F, -2);
    if (R_FAILED(rc)) {
        std::printf("update: threadCreate failed 0x%x\n", (unsigned)rc);
        std::fflush(stdout);
        g_check_state = (int)CheckState::Idle;
        return -1;
    }
    threadStart(&g_check_thread);
    g_check_thread_live = true;
    return 0;
}

// 1 while the check is running: the cheap question asked each frame, before
// a buffer is made for the answer.
extern "C" int flashnx_update_check_busy(void) {
    return g_check_state == (int)CheckState::Running ? 1 : 0;
}

// 0 = still running, n > 0 = done (the answer copied into `out`,
// NUL-terminated), -1 = nothing started, -2 = failed, -3 = does not fit `cap`.
extern "C" int flashnx_update_check_poll(char* out, int cap) {
    const int st = g_check_state;
    if (st == (int)CheckState::Running) return 0;
    if (st == (int)CheckState::Idle) return -1;
    check_join();
    g_check_state = (int)CheckState::Idle;
    if (st == (int)CheckState::Failed) return -2;
    const size_t n = g_check_out.size();
    if (!out || cap < 1 || n >= (size_t)cap) {
        g_check_out.clear();
        return -3;
    }
    std::memcpy(out, g_check_out.data(), n);
    out[n] = '\0';
    g_check_out.clear();
    g_check_out.shrink_to_fit();
    return n > 0 ? (int)n : -2;
}

// At exit, before the sockets go: a check still running is aborted and waited.
extern "C" void flashnx_update_check_stop(void) {
    g_check_abort = true;
    check_join();
}

// The SHA-256 of the file at `path` into `out` (32 bytes). 1 on success.
extern "C" int flashnx_sha256_file(const char* path, unsigned char* out) {
    FILE* f = std::fopen(path, "rb");
    if (!f) return 0;
    static unsigned char buf[256 * 1024];
    mbedtls_sha256_context ctx;
    mbedtls_sha256_init(&ctx);
    bool ok = mbedtls_sha256_starts_ret(&ctx, 0) == 0;
    while (ok) {
        const size_t n = std::fread(buf, 1, sizeof(buf), f);
        if (n > 0 && mbedtls_sha256_update_ret(&ctx, buf, n) != 0) ok = false;
        if (n < sizeof(buf)) {
            if (std::ferror(f)) ok = false;
            break;
        }
    }
    std::fclose(f);
    if (ok) ok = mbedtls_sha256_finish_ret(&ctx, out) == 0;
    mbedtls_sha256_free(&ctx);
    return ok ? 1 : 0;
}

// Put `fresh` in place of `target`. 1 on success. The old file is set aside
// first and only deleted once the new one has its name, so a failure at any
// point leaves a FlashNX at `target`.
extern "C" int flashnx_replace_file(const char* fresh, const char* target) {
    char old[600];
    std::snprintf(old, sizeof(old), "%s.old", target);
    std::remove(old); // a leftover of an update that stopped half-way
    if (std::rename(target, old) != 0) {
        std::printf("update: could not set %s aside\n", target);
        std::fflush(stdout);
        return 0;
    }
    if (std::rename(fresh, target) != 0) {
        std::printf("update: could not move %s to %s, putting the old one back\n", fresh, target);
        std::fflush(stdout);
        std::rename(old, target);
        return 0;
    }
    std::remove(old);
    return 1;
}
