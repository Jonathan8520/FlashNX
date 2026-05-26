// Phase 3.4 — enumerate every .swf on SD into the library UI.
//
// We do this in C++ rather than Rust because Rust's `std::fs::read_dir` on
// Horizon corrupts entry names (observed 2026-05-24: filenames came back
// missing their first 2 bytes — suspected dirent struct layout mismatch
// between Rust's Unix model and devkitPro's newlib on aarch64). C-level
// opendir/readdir via libnx fsdev does not exhibit this bug.
//
// Two locations are scanned: `sdmc:/ruffle/` (the preferred drop dir, used
// by the desktop Ruffle frontend convention) and `sdmc:/switch/ruffle/` (a
// fallback under the Switch homebrew convention). Every `.swf` we find is
// pushed into the Rust library state via `ruffle_library_add_path` — Rust
// then opens each file briefly to parse SWF version + size for the
// metadata panel.

#include <cstdio>
#include <cstring>
#include <dirent.h>
#include <strings.h>
#include <sys/stat.h>

#include "ruffle_bridge.h"

// Defined in the Rust staticlib: pushes one (path) onto the library's
// scan list. Rust handles reading SWF header lazily. Returns 0 on success.
extern "C" int ruffle_library_add_path(const char* path);

namespace {

bool ends_with_swf(const char* name) {
    if (!name) return false;
    const size_t n = std::strlen(name);
    if (n < 4) return false;
    return strcasecmp(name + n - 4, ".swf") == 0;
}

// Enumerate every `.swf` in `dir` and forward absolute paths to Rust.
// Silent if the directory doesn't exist (SD often only has one of the two
// candidate locations populated).
void scan_dir_all(const char* dir) {
    DIR* d = opendir(dir);
    if (!d) {
        std::printf("library scan: opendir(%s) failed (skip — dir absent)\n", dir);
        std::fflush(stdout);
        return;
    }
    char path[512];
    int found = 0;
    while (struct dirent* ent = readdir(d)) {
        if (!ends_with_swf(ent->d_name)) continue;
        const int n = std::snprintf(path, sizeof(path), "%s%s%s",
                                    dir,
                                    (dir[std::strlen(dir) - 1] == '/') ? "" : "/",
                                    ent->d_name);
        if (n <= 0 || (size_t)n >= sizeof(path)) {
            std::printf("library scan: path too long, skipping %s\n", ent->d_name);
            continue;
        }
        // Confirm regular file (DT_UNKNOWN on some fsdev volumes — fall back
        // to stat). Skip dirs / symlinks / etc.
        bool is_regular = (ent->d_type == DT_REG);
        if (!is_regular && ent->d_type == DT_UNKNOWN) {
            struct stat st;
            if (::stat(path, &st) == 0 && S_ISREG(st.st_mode)) {
                is_regular = true;
            }
        }
        if (!is_regular) continue;
        if (ruffle_library_add_path(path) == 0) {
            ++found;
        }
    }
    closedir(d);
    std::printf("library scan: %s -> %d .swf\n", dir, found);
    std::fflush(stdout);
}

} // namespace

// Public entry: populate the Rust library state with every .swf on SD.
// Called from the worker thread BEFORE `ruffle_library_init` opens the UI.
//
// Scan order (Phase 3.4 / 2026-05-26 nuit rename):
//   1. `sdmc:/flashnx/`         — new default (matches brand)
//   2. `sdmc:/ruffle/`          — backward-compat for users coming from
//                                 the pre-rename releases
//   3. `sdmc:/switch/ruffle/`   — homebrew convention path, legacy
//   4. `sdmc:/switch/flashnx/`  — homebrew convention path, new name
//
// Same file present in two dirs would produce two list entries (deduped
// on basename later via `add_or_replace_path`).
extern "C" void swf_picker_run(void) {
    static const char* DIRS[] = {
        "sdmc:/flashnx/",
        "sdmc:/ruffle/",
        "sdmc:/switch/flashnx/",
        "sdmc:/switch/ruffle/",
    };
    for (const char* dir : DIRS) {
        scan_dir_all(dir);
    }
}
