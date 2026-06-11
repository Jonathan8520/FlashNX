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

#include <cerrno>
#include <cstdio>
#include <cstring>
#include <dirent.h>
#include <strings.h>
#include <sys/stat.h>
#include <unistd.h>

#include "ruffle_bridge.h"

// Defined in the Rust staticlib: pushes one (path) onto the library's
// scan list. Rust handles reading SWF header lazily. Returns 0 on success.
extern "C" int ruffle_library_add_path(const char* path, unsigned long long mtime);

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
        // stat() for the regular-file check AND the mtime: the "recent" sort
        // needs a timestamp, d_type carries none (and is DT_UNKNOWN on some
        // fsdev volumes anyway), and Rust's std::fs metadata is unreliable on
        // Horizon — so the timestamp comes from here.
        struct stat st;
        if (::stat(path, &st) != 0 || !S_ISREG(st.st_mode)) continue;
        if (ruffle_library_add_path(path, (unsigned long long)st.st_mtime) == 0) {
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

// Recursively delete a directory and everything under it (one nested level is
// enough for our companion folders, but recurse anyway for safety). Uses
// opendir/readdir/unlink/rmdir — safe on Horizon, unlike Rust's read_dir.
// Returns the number of entries (files + dirs) removed; 0 if `path` is absent.
static int remove_dir_recursive(const char* path) {
    DIR* d = opendir(path);
    if (!d) return 0; // absent or not a directory — nothing to do (idempotent)
    int removed = 0;
    char child[512];
    while (struct dirent* ent = readdir(d)) {
        const char* n = ent->d_name;
        if (std::strcmp(n, ".") == 0 || std::strcmp(n, "..") == 0) continue;
        const int nl = std::snprintf(child, sizeof(child), "%s/%s", path, n);
        if (nl <= 0 || (size_t)nl >= sizeof(child)) continue;
        if (ent->d_type == DT_DIR) {
            removed += remove_dir_recursive(child);
        } else if (::unlink(child) == 0) {
            ++removed;
        }
    }
    closedir(d);
    if (::rmdir(path) == 0) ++removed;
    return removed;
}

// Phase 3.4.bis SUPPRIMER — delete a game's .swf + every sidecar / save
// file matching its basename. Pattern matched: file name == basename OR
// file name starts with `<basename>.` (catches `.meta.json`,
// `.keymap.json`, and the flat-layout `<basename>.<sol_name>.sol` saves
// from Phase 3.9). Same opendir/readdir path as the scan to dodge the
// Rust `read_dir` Horizon bug.
//
// Returns the number of files actually removed, or -1 on parameter /
// opendir failure. Missing matches are not an error (idempotent).
extern "C" int swf_picker_delete_game(const char* swf_path) {
    if (!swf_path || !*swf_path) return -1;
    const char* slash = std::strrchr(swf_path, '/');
    if (!slash) return -1;
    const size_t plen = std::strlen(swf_path);
    const size_t dirlen = (size_t)(slash - swf_path) + 1; // include trailing '/'
    const size_t blen = plen - dirlen;
    char dir[512];
    char basename[256];
    if (dirlen >= sizeof(dir) || blen == 0 || blen >= sizeof(basename)) return -1;
    std::memcpy(dir, swf_path, dirlen);
    dir[dirlen] = '\0';
    std::memcpy(basename, slash + 1, blen);
    basename[blen] = '\0';

    DIR* d = opendir(dir);
    if (!d) {
        std::printf("swf_picker_delete_game: opendir(%s) failed errno=%d\n", dir, errno);
        std::fflush(stdout);
        return -1;
    }
    int removed = 0;
    char path[512];
    while (struct dirent* ent = readdir(d)) {
        const char* n = ent->d_name;
        const bool exact = (std::strcmp(n, basename) == 0);
        const bool prefixed =
            !exact && std::strncmp(n, basename, blen) == 0 && n[blen] == '.';
        if (!exact && !prefixed) continue;
        if (ent->d_type == DT_DIR) continue; // defensive
        const int nl = std::snprintf(path, sizeof(path), "%s%s", dir, n);
        if (nl <= 0 || (size_t)nl >= sizeof(path)) continue;
        if (::unlink(path) == 0) {
            std::printf("swf_picker_delete_game: removed %s\n", path);
            ++removed;
        } else {
            std::printf("swf_picker_delete_game: unlink(%s) failed errno=%d\n",
                        path, errno);
        }
    }
    closedir(d);

    // Multi-file games (v1.3.0): also remove the companion folder
    // `<stem>.files/` (sibling SWFs fetched by gamezip::fetch_siblings, plus any
    // nested asset dirs). `stem` = basename minus a trailing ".swf". Rust can't
    // enumerate it reliably on Horizon, so the recursive unlink lives here.
    {
        size_t stemlen = blen;
        if (blen >= 4) {
            const char* e = basename + blen - 4;
            if (e[0] == '.' && (e[1] == 's' || e[1] == 'S') && (e[2] == 'w' || e[2] == 'W')
                && (e[3] == 'f' || e[3] == 'F')) {
                stemlen = blen - 4;
            }
        }
        char filesdir[512];
        const int nl = std::snprintf(filesdir, sizeof(filesdir), "%s%.*s.files",
                                     dir, (int)stemlen, basename);
        if (nl > 0 && (size_t)nl < sizeof(filesdir)) {
            const int n = remove_dir_recursive(filesdir);
            if (n > 0) {
                std::printf("swf_picker_delete_game: removed companion dir %s (%d entries)\n",
                            filesdir, n);
                removed += n;
            }
        }
    }

    std::fflush(stdout);
    return removed;
}

// Multi-file indicator (v1.3.0): count the companion SWFs in a game's
// `<stem>.files/` folder. Returns the count, or 0 if the folder is absent.
// opendir/readdir is Horizon-safe (unlike Rust's read_dir).
extern "C" int swf_picker_count_companions(const char* swf_path) {
    if (!swf_path || !*swf_path) return 0;
    const size_t plen = std::strlen(swf_path);
    size_t base = plen; // strip a trailing ".swf" to get "<dir><stem>"
    if (plen >= 4) {
        const char* e = swf_path + plen - 4;
        if (e[0] == '.' && (e[1] == 's' || e[1] == 'S') && (e[2] == 'w' || e[2] == 'W')
            && (e[3] == 'f' || e[3] == 'F')) {
            base = plen - 4;
        }
    }
    char filesdir[512];
    const int nl = std::snprintf(filesdir, sizeof(filesdir), "%.*s.files", (int)base, swf_path);
    if (nl <= 0 || (size_t)nl >= sizeof(filesdir)) return 0;
    DIR* d = opendir(filesdir);
    if (!d) return 0; // no companion folder for this game
    int count = 0;
    while (struct dirent* ent = readdir(d)) {
        if (ent->d_type == DT_DIR) continue;
        const char* n = ent->d_name;
        const size_t l = std::strlen(n);
        if (l >= 4) {
            const char* e = n + l - 4;
            if (e[0] == '.' && (e[1] == 's' || e[1] == 'S') && (e[2] == 'w' || e[2] == 'W')
                && (e[3] == 'f' || e[3] == 'F')) {
                ++count;
            }
        }
    }
    closedir(d);
    return count;
}
