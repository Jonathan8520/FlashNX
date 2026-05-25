// Phase 2.6 — scan SD card for .swf files via newlib's opendir/readdir.
//
// We do this in C++ rather than Rust because Rust's `std::fs::read_dir` on
// Horizon corrupts entry names (observed 2026-05-24: filenames came back
// missing their first 2 bytes — suspected dirent struct layout mismatch
// between Rust's Unix model and devkitPro's newlib on aarch64). C-level
// opendir/readdir via libnx fsdev does not exhibit this bug.
//
// We scan two locations: `sdmc:/ruffle/` (the preferred drop dir, used by
// the desktop Ruffle frontend convention) and `sdmc:/switch/ruffle/` (a
// fallback under the Switch homebrew convention). The first `.swf` we find
// becomes the SWF to load. The library UI (Phase 3.4) will replace this
// with a real list+picker once we have a font/text-rendering stack.

#include <cstdio>
#include <cstring>
#include <dirent.h>
#include <strings.h>
#include <sys/stat.h>

#include "ruffle_bridge.h"

// Defined in the Rust staticlib: stores the path and overrides the SWF
// resolution that would otherwise fall back to the hardcoded candidates
// list in lib.rs. Returns 0 on success.
extern "C" int ruffle_set_swf_path(const char* path);

namespace {

bool ends_with_swf(const char* name) {
    if (!name) return false;
    const size_t n = std::strlen(name);
    if (n < 4) return false;
    return strcasecmp(name + n - 4, ".swf") == 0;
}

// Scan one directory for the first `.swf` it contains. Writes the full path
// into `out` (up to out_cap bytes including NUL). Returns true on hit.
bool scan_dir(const char* dir, char* out, size_t out_cap) {
    DIR* d = opendir(dir);
    if (!d) {
        std::printf("swf_picker: opendir(%s) failed (errno may be unmounted/missing dir)\n", dir);
        std::fflush(stdout);
        return false;
    }
    bool found = false;
    while (struct dirent* ent = readdir(d)) {
        if (!ends_with_swf(ent->d_name)) continue;
        // Build "<dir>/<name>" with NUL-safe truncation.
        const int n = std::snprintf(out, out_cap, "%s%s%s",
                                    dir,
                                    (dir[std::strlen(dir) - 1] == '/') ? "" : "/",
                                    ent->d_name);
        if (n <= 0 || (size_t)n >= out_cap) continue;
        // Confirm it's a regular file (some dirents have d_type=DT_UNKNOWN on
        // newlib's fsdev — fall back to stat).
        if (ent->d_type == DT_REG || ent->d_type == DT_UNKNOWN) {
            struct stat st;
            if (ent->d_type == DT_REG || (::stat(out, &st) == 0 && S_ISREG(st.st_mode))) {
                found = true;
                break;
            }
        }
    }
    closedir(d);
    return found;
}

} // namespace

// Public entry: scan known SWF locations and tell Rust the path to use.
// Called from the worker thread before `ruffle_init`. Idempotent — if no
// SWF is found, Rust falls back to its hardcoded candidates / embedded
// red-background SWF, so we never block boot.
extern "C" void swf_picker_run(void) {
    static const char* DIRS[] = {
        "sdmc:/ruffle/",
        "sdmc:/switch/ruffle/",
    };
    char path[512];
    for (const char* dir : DIRS) {
        if (scan_dir(dir, path, sizeof(path))) {
            std::printf("swf_picker: found %s\n", path);
            std::fflush(stdout);
            if (ruffle_set_swf_path(path) != 0) {
                std::printf("swf_picker: ruffle_set_swf_path rejected %s\n", path);
                std::fflush(stdout);
            }
            return;
        }
    }
    std::printf("swf_picker: no .swf found in known SD locations — Rust will use hardcoded candidates / fallback\n");
    std::fflush(stdout);
}
