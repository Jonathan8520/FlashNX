// Shortcuts: write one `.nro` per game into `sdmc:/switch/`, so a game gets its
// own entry in hbmenu and in Sphaira instead of hiding behind
// "FlashNX | angry-birds.swf".
//
// The file is 144 KB and carries NO engine. It hands the `.swf` path back to the
// installed FlashNX and exits (stub/source/main.c). That is the point: updating
// FlashNX updates every shortcut at once.
//
// A fatter variant was considered and dropped -- a copy of the whole player with
// the game's path written in, needing nothing else installed. It works, but it is
// 16.8 MB per game and its engine is frozen at the moment it was made, so a
// player who picked it for feeling safer would end up with ten copies of a build
// that never gets a fix again. Shipping a game that way is a developer's job and
// has its own answer: `cpp/romfs/` at build time (issue #106).
//
// ## Why not a tile on the console's own HOME menu
//
// That needs an NSP forwarder, i.e. building an NCA. Atmosphere validates its
// ACID signature (`IsEnabledProgramVerification` starts true and its setter only
// obeys a development flag no retail unit has), so such a tile only launches on
// a console whose sigpatches match its firmware, and stops when the firmware
// moves. And every working builder is copyleft or carries no licence at all:
// Sphaira's says in its own header that it is "based on hacbrewpack ... and
// yati", so its author could not grant an exception even if asked. Nothing here
// touches `ncm`, `ns`, crypto or sigpatches. A shortcut is a file, and deleting
// one is deleting a file.

#include <cstdio>
#include <cstring>
#include <string>
#include <switch.h>

// The stub, assembled in by bin2s from `stub/flashnx_stub.nro` (see cpp/Makefile).
extern "C" {
extern const u8 flashnx_stub_nro[];
extern const u8 flashnx_stub_nro_end[];
int swf_picker_write_file(const char* path, const unsigned char* data, unsigned int len);
}

namespace {

// Must match `TARGET_MARKER` in stub/source/main.c. The stub holds a 1024-byte
// array beginning with this string; the path goes in the remainder.
constexpr const char MARKER[] = "FLASHNX_SHORTCUT_TARGET_V1:";
constexpr size_t MARKER_LEN = sizeof(MARKER) - 1;
constexpr size_t TARGET_CAP = 1024;

// Asset section appended after the NRO proper, exactly as elf2nro writes it:
// magic, version, then three {offset,size} pairs whose offsets are relative to
// the START OF THIS HEADER, not to the file (checked against switch-tools'
// elf2nro.c, which does `fseek(out, file_off + asset_hdr.icon.offset, ...)`).
constexpr u32 ASET_MAGIC = 0x54455341; // 'ASET'
constexpr size_t ASET_HEADER_SIZE = 0x38;
constexpr size_t NACP_SIZE = 0x4000;
// NacpLanguageEntry lang[16], each { char name[0x200]; char author[0x100]; }.
constexpr size_t NACP_LANG_COUNT = 16;
constexpr size_t NACP_LANG_STRIDE = 0x300;
constexpr size_t NACP_NAME_CAP = 0x200;
constexpr size_t NACP_AUTHOR_CAP = 0x100;

void put_le32(u8* p, u32 v) {
    p[0] = (u8)(v);
    p[1] = (u8)(v >> 8);
    p[2] = (u8)(v >> 16);
    p[3] = (u8)(v >> 24);
}

void put_le64(u8* p, u64 v) {
    put_le32(p, (u32)v);
    put_le32(p + 4, (u32)(v >> 32));
}

} // namespace

/// Write a shortcut for `swf_path`, shown as `display_name`, to `out_path`.
///
/// Returns 1 on success, or a negative code the caller turns into a message:
/// -1 no stub in this build, -2 the path does not fit, -3 the stub has no
/// marker (it was built wrong), -4 the file could not be written.
extern "C" int shortcut_write(const char* swf_path,
                              const char* display_name,
                              const char* out_path) {
    if (!swf_path || !display_name || !out_path) return -4;

    const size_t stub_len = (size_t)(flashnx_stub_nro_end - flashnx_stub_nro);
    if (stub_len < 0x20) return -1;

    const size_t path_len = std::strlen(swf_path);
    // The remainder of the stub's array, minus the terminator it must keep.
    if (path_len + 1 > TARGET_CAP - MARKER_LEN) return -2;

    // One buffer: the stub, then the asset header, then the NACP. The stub is
    // built with no asset section of its own, so this only ever appends.
    std::string out;
    out.assign((const char*)flashnx_stub_nro, stub_len);
    out.append(ASET_HEADER_SIZE + NACP_SIZE, '\0');
    u8* buf = (u8*)&out[0];

    // Searched, never a fixed offset: the offset moves with every rebuild of the
    // stub, and a stale constant would write the path into unrelated code.
    size_t at = (size_t)-1;
    for (size_t i = 0; i + MARKER_LEN <= stub_len; i++) {
        if (std::memcmp(buf + i, MARKER, MARKER_LEN) == 0) {
            at = i;
            break;
        }
    }
    if (at == (size_t)-1) return -3;
    u8* target = buf + at + MARKER_LEN;
    std::memset(target, 0, TARGET_CAP - MARKER_LEN);
    std::memcpy(target, swf_path, path_len);

    u8* aset = buf + stub_len;
    put_le32(aset + 0x00, ASET_MAGIC);
    put_le32(aset + 0x04, 0);
    put_le64(aset + 0x08, 0); // icon offset
    put_le64(aset + 0x10, 0); // icon size: none, see below
    put_le64(aset + 0x18, ASET_HEADER_SIZE);
    put_le64(aset + 0x20, NACP_SIZE);
    put_le64(aset + 0x28, 0); // no romfs
    put_le64(aset + 0x30, 0);

    // No icon, deliberately. A `.nro` icon is expected to be JPEG 256x256; our
    // box art is PNG and we ship a decoder but no encoder. A PNG in this slot was
    // tried on hardware (2026-08-26) and it does display, badly: non-square art,
    // white bands, wrong crop. An absent icon is valid and the launcher falls
    // back to its default, which is honest rather than wrong.

    // The name goes into ALL sixteen language slots, which is what nacptool
    // does: a launcher reads the slot for the console's own language, so filling
    // one would leave the entry blank on every other system setting.
    u8* nacp = aset + ASET_HEADER_SIZE;
    for (size_t i = 0; i < NACP_LANG_COUNT; i++) {
        u8* e = nacp + i * NACP_LANG_STRIDE;
        std::snprintf((char*)e, NACP_NAME_CAP, "%s", display_name);
        std::snprintf((char*)e + NACP_NAME_CAP, NACP_AUTHOR_CAP, "%s", "FlashNX");
    }

    // Through the C++ writer, like every other file this app puts on the card:
    // Rust's `std::fs::write` reports success on Horizon and the file then reads
    // back ENOENT on some writes (see navigator.rs / gamezip.rs).
    if (swf_picker_write_file(out_path, (const unsigned char*)out.data(),
                              (unsigned int)out.size()) != 1) {
        return -4;
    }
    return 1;
}

/// Delete the shortcut at `path`, but ONLY if it is one of ours.
///
/// Guarded on purpose. The file lives in `sdmc:/switch/`, which is everyone's
/// homebrew folder, and its name comes from a game title: someone with a game
/// called "Moonlight" would otherwise lose their Moonlight client the day they
/// delete that game from FlashNX. So the marker is checked first, and a file
/// without it is left exactly where it is.
///
/// Returns 1 when a shortcut was removed, 0 when the file was absent or not
/// ours, negative on a read error.
extern "C" int shortcut_delete(const char* path) {
    if (!path || !*path) return -1;
    FILE* f = std::fopen(path, "rb");
    if (!f) return 0; // nothing there: deleting a game with no shortcut is normal
    // The marker sits in the stub's rodata, well inside the first 256 KB, and the
    // whole stub is 144 KB. Reading the head is enough and bounds the cost.
    static char buf[256 * 1024];
    const size_t n = std::fread(buf, 1, sizeof(buf), f);
    std::fclose(f);
    if (n == 0) return -1;

    // Assembled rather than written as one literal, for the same reason as in
    // the writer: this file ends up inside the binary, and a contiguous copy
    // would make FlashNX itself look like a shortcut to any future scan.
    char marker[64];
    std::snprintf(marker, sizeof(marker), "%s%s", "FLASHNX_SHORTCUT_", "TARGET_V1:");
    const size_t mlen = std::strlen(marker);
    bool ours = false;
    for (size_t i = 0; i + mlen <= n; i++) {
        if (std::memcmp(buf + i, marker, mlen) == 0) {
            ours = true;
            break;
        }
    }
    if (!ours) {
        std::printf("shortcut: %s is not ours, left alone\n", path);
        std::fflush(stdout);
        return 0;
    }
    return std::remove(path) == 0 ? 1 : -1;
}
