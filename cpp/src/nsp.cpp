// HOME-menu tiles: build an application out of one game and install it.
//
// A tile is a real installed title, the same kind Tinfoil and Sphaira put on the
// console, not an entry in a homebrew launcher. It is three NCAs:
//
//   program  the forwarder (forwarder/), an ExeFS holding `main` + `main.npdm`
//   control  a RomFS holding `control.nacp` (the name) and an icon (the cover)
//   meta     a PFS0 holding one `.cnmt`, which is the list of the other two
//
// plus an entry in `ns`'s application record list, which is what the HOME menu
// actually reads. Nothing here is copied from another implementation: every
// builder below is written against the public format documentation, because the
// tools that already do this (hacBrewPack, nton, Sphaira) are GPL or carry no
// licence at all and FlashNX is MIT.
//
// ## What this needs from the console, and what it does not
//
// It does NOT need `prod.keys`. Only the NCA header is encrypted, and its key is
// derived at runtime through `spl`, which any process can ask. Sections are
// stored with EncryptionType None and no rights ID, so there is no ticket and
// `es` is never involved.
//
// It DOES need signature patches, and there is no way around that. The first
// NCA signature is RSA-2048 under a Nintendo key nobody has, so it is left zero
// exactly as every homebrew NSP leaves it, and a console without patches (or
// without sys-patch) refuses to mount the file. That is the same requirement
// Tinfoil, Sphaira and PipeNSX carry. It also means a firmware update with stale
// patches stops the tiles until the patches catch up -- the game is still in the
// library, and the tile starts working again on its own.
//
// ## Why the builder can also write a `.nsp` to the card
//
// `nsp_write_file` produces the same three NCAs wrapped in a PFS0. It is not a
// convenience: a tile that installs wrong can sit in the HOME menu in a state
// that neither launches nor uninstalls, so being able to hand the exact bytes to
// `hactool` on a PC, or to install them with a tool that already works, is how
// this gets checked without gambling somebody's home screen.

#include <algorithm>
#include <cstdio>
#include <cstring>
#include <sys/stat.h>
#include <string>
#include <vector>
#include <switch.h>

#include <mbedtls/aes.h>
#include <mbedtls/sha256.h>

extern "C" {
// The forwarder's ExeFS, already a PFS0 (`main` + `main.npdm`), assembled by the
// devkitPro `.nsp` rule and turned into a byte array by bin2s. See cpp/Makefile.
extern const u8 flashnx_forwarder_nsp[];
extern const u8 flashnx_forwarder_nsp_end[];
int swf_picker_write_file(const char* path, const unsigned char* data, unsigned int len);

// Declared up here because installing starts by removing: see the top of
// `nsp_install_tile`.
int nsp_remove_tile(u64 program_id);
int nsp_tile_exists(u64 program_id);
}

namespace {

// ---------------------------------------------------------------------------
// Small binary helpers
// ---------------------------------------------------------------------------

void put_le16(u8* p, u16 v) { p[0] = (u8)v; p[1] = (u8)(v >> 8); }

void put_le32(u8* p, u32 v) {
    p[0] = (u8)v; p[1] = (u8)(v >> 8); p[2] = (u8)(v >> 16); p[3] = (u8)(v >> 24);
}

void put_le64(u8* p, u64 v) { put_le32(p, (u32)v); put_le32(p + 4, (u32)(v >> 32)); }

u64 align_up(u64 v, u64 a) { return (v + a - 1) / a * a; }

void sha256(const void* data, size_t len, u8 out[32]) {
    // `_ret` and not `mbedtls_sha256`: in the 2.28 devkitPro ships, the short
    // name is the deprecated one that returns void.
    mbedtls_sha256_ret((const unsigned char*)data, len, out, 0);
}

/// Append `n` zero bytes, then return the offset where the caller may write.
size_t pad_to(std::string& buf, u64 a) {
    const size_t want = (size_t)align_up(buf.size(), a);
    buf.resize(want, '\0');
    return want;
}

// ---------------------------------------------------------------------------
// The NCA header key
// ---------------------------------------------------------------------------

// Public derivation seeds. They are not keys and not secret: they are the inputs
// the console itself hashes against a key that never leaves the security engine.
// `spl` does the derivation, which is why no `prod.keys` file is involved.
const u8 HEADER_KEK_SOURCE[0x10] = {
    0x1F, 0x12, 0x91, 0x3A, 0x4A, 0xCB, 0xF0, 0x0D,
    0x4C, 0xDE, 0x3A, 0xF6, 0xD5, 0x23, 0x88, 0x2A,
};
const u8 HEADER_KEY_SOURCE[0x20] = {
    0x5A, 0x3E, 0xD8, 0x4F, 0xDE, 0xC0, 0xD8, 0x26,
    0x31, 0xF7, 0xE2, 0x5D, 0x19, 0x7B, 0xF5, 0xD0,
    0x1C, 0x9B, 0x7B, 0xFA, 0xF6, 0x28, 0x18, 0x3D,
    0x71, 0xF6, 0x4D, 0x73, 0xF1, 0x50, 0xB9, 0xD2,
};

/// The 32-byte AES-128-XTS key the NCA header is encrypted with: 16 bytes of
/// crypt key followed by 16 bytes of tweak key.
///
/// It is the same on every retail console, but it still has to be asked for
/// rather than shipped, which is the whole reason this works without keys on the
/// card. Returns false when `spl:` refuses, which is what happens if this ever
/// runs somewhere without the service access homebrew normally inherits.
bool derive_header_key(u8 out[0x20]) {
    Result rc = splCryptoInitialize();
    if (R_FAILED(rc)) return false;

    u8 kek[0x10] = {};
    bool ok = R_SUCCEEDED(splCryptoGenerateAesKek(HEADER_KEK_SOURCE, 0, 0, kek));
    // Two halves because the XTS key is twice as long as the AES block key the
    // generation step produces.
    if (ok) ok = R_SUCCEEDED(splCryptoGenerateAesKey(kek, HEADER_KEY_SOURCE, out));
    if (ok) ok = R_SUCCEEDED(splCryptoGenerateAesKey(kek, HEADER_KEY_SOURCE + 0x10, out + 0x10));

    splCryptoExit();
    return ok;
}

/// Encrypt an NCA header in place: AES-128-XTS, 0x200-byte sectors, sector
/// numbers counting from zero.
///
/// The one non-obvious part is the tweak. Standard XTS writes the sector number
/// little-endian into the 16-byte tweak block; the Switch writes it big-endian.
/// Getting this backwards produces a file that is byte-for-byte plausible and
/// that `fs` rejects, so it is worth the four lines of explanation.
void encrypt_header(u8* header, size_t len, const u8 key[0x20]) {
    mbedtls_aes_xts_context ctx;
    mbedtls_aes_xts_init(&ctx);
    // 256 bits = the two 128-bit halves, crypt key first, which is the order
    // `spl` hands them back in.
    mbedtls_aes_xts_setkey_enc(&ctx, key, 256);

    for (size_t sector = 0; sector * 0x200 < len; sector++) {
        u8 tweak[0x10] = {};
        for (int i = 0; i < 8; i++) tweak[15 - i] = (u8)(sector >> (8 * i));
        mbedtls_aes_crypt_xts(&ctx, MBEDTLS_AES_ENCRYPT, 0x200, tweak,
                              header + sector * 0x200, header + sector * 0x200);
    }
    mbedtls_aes_xts_free(&ctx);
}

// ---------------------------------------------------------------------------
// PFS0
// ---------------------------------------------------------------------------

struct PfsFile {
    std::string name;
    const u8* data;
    size_t size;
};

/// Build a PFS0 ("partition filesystem"): a header, a fixed-size entry per file,
/// a string table, then the file bodies.
///
/// The string table is padded so the bodies start on a 0x20 boundary. That
/// padding is not decorative: the offsets in the entry table are relative to the
/// end of the header block, so moving the boundary moves every file.
std::string build_pfs0(const std::vector<PfsFile>& files) {
    const size_t n = files.size();
    size_t string_table_size = 0;
    for (const auto& f : files) string_table_size += f.name.size() + 1;

    const size_t entries_end = 0x10 + 0x18 * n;
    const size_t header_size = align_up(entries_end + string_table_size, 0x20);
    string_table_size = header_size - entries_end;

    std::string out;
    out.assign(header_size, '\0');
    u8* p = (u8*)&out[0];
    std::memcpy(p, "PFS0", 4);
    put_le32(p + 0x04, (u32)n);
    put_le32(p + 0x08, (u32)string_table_size);
    put_le32(p + 0x0C, 0);

    size_t name_off = 0;
    u64 data_off = 0;
    for (size_t i = 0; i < n; i++) {
        u8* e = p + 0x10 + 0x18 * i;
        put_le64(e + 0x00, data_off);
        put_le64(e + 0x08, files[i].size);
        put_le32(e + 0x10, (u32)name_off);
        put_le32(e + 0x14, 0);
        std::memcpy(p + entries_end + name_off, files[i].name.c_str(), files[i].name.size() + 1);
        name_off += files[i].name.size() + 1;
        data_off += files[i].size;
    }
    for (const auto& f : files) out.append((const char*)f.data, f.size);
    return out;
}

// ---------------------------------------------------------------------------
// RomFS, root directory only
// ---------------------------------------------------------------------------

/// The path hash RomFS buckets on. Rotate-right five, xor the byte.
u32 romfs_hash(u32 parent, const std::string& name) {
    u32 hash = parent ^ 123456789;
    for (unsigned char c : name) {
        hash = (hash >> 5) | (hash << 27);
        hash ^= c;
    }
    return hash;
}

/// Bucket count for `n` entries: the first number at or above `n` that is not
/// divisible by any of 2, 3, 5, 7 or 11.
u32 romfs_bucket_count(u32 n) {
    if (n < 3) return 3;
    if (n < 19) return n | 1;
    u32 count = n;
    while (count % 2 == 0 || count % 3 == 0 || count % 5 == 0 ||
           count % 7 == 0 || count % 11 == 0) {
        count++;
    }
    return count;
}

/// Build a RomFS image with every file in the root and no subdirectories.
///
/// That restriction is what keeps this short: a general builder has to walk a
/// tree and thread parent and sibling links through it, and a control RomFS only
/// ever holds a NACP and a handful of icons.
std::string build_romfs(const std::vector<PfsFile>& files) {
    const u32 n = (u32)files.size();
    const u32 dir_buckets = romfs_bucket_count(1);
    const u32 file_buckets = romfs_bucket_count(n);
    const u32 NONE = 0xFFFFFFFFu;

    // One root directory entry, no name.
    const u32 dir_meta_size = 0x18;

    std::vector<u32> file_entry_off(n);
    u32 file_meta_size = 0;
    for (u32 i = 0; i < n; i++) {
        file_entry_off[i] = file_meta_size;
        file_meta_size += (u32)align_up(0x20 + files[i].name.size(), 4);
    }

    const u64 dir_hash_off  = 0x50;
    const u64 dir_hash_size = (u64)dir_buckets * 4;
    const u64 dir_meta_off  = align_up(dir_hash_off + dir_hash_size, 4);
    const u64 file_hash_off = align_up(dir_meta_off + dir_meta_size, 4);
    const u64 file_hash_size = (u64)file_buckets * 4;
    const u64 file_meta_off = align_up(file_hash_off + file_hash_size, 4);
    const u64 file_data_off = align_up(file_meta_off + file_meta_size, 0x10);

    std::string out;
    out.assign((size_t)file_data_off, '\0');
    u8* p = (u8*)&out[0];

    put_le64(p + 0x00, 0x50);
    put_le64(p + 0x08, dir_hash_off);
    put_le64(p + 0x10, dir_hash_size);
    put_le64(p + 0x18, dir_meta_off);
    put_le64(p + 0x20, dir_meta_size);
    put_le64(p + 0x28, file_hash_off);
    put_le64(p + 0x30, file_hash_size);
    put_le64(p + 0x38, file_meta_off);
    put_le64(p + 0x40, file_meta_size);
    put_le64(p + 0x48, file_data_off);

    for (u32 i = 0; i < dir_buckets; i++) put_le32(p + dir_hash_off + 4 * i, NONE);
    for (u32 i = 0; i < file_buckets; i++) put_le32(p + file_hash_off + 4 * i, NONE);

    // The root: no parent but itself, no sibling, no child directory, and the
    // first file if there is one.
    u8* root = p + dir_meta_off;
    put_le32(root + 0x00, 0);
    put_le32(root + 0x04, NONE);
    put_le32(root + 0x08, NONE);
    put_le32(root + 0x0C, n ? 0 : NONE);
    put_le32(root + 0x10, NONE);
    put_le32(root + 0x14, 0);
    // The root goes in its own hash bucket, hashed on an empty name.
    put_le32(p + dir_hash_off + 4 * (romfs_hash(0, "") % dir_buckets), 0);

    u64 data_off = 0;
    for (u32 i = 0; i < n; i++) {
        u8* e = p + file_meta_off + file_entry_off[i];
        put_le32(e + 0x00, 0);                                        // parent: root
        put_le32(e + 0x04, i + 1 < n ? file_entry_off[i + 1] : NONE); // next sibling
        put_le64(e + 0x08, data_off);
        put_le64(e + 0x10, files[i].size);
        const u32 bucket = romfs_hash(0, files[i].name) % file_buckets;
        // Push onto the bucket's chain: this entry points at whoever was there.
        put_le32(e + 0x18, *(const u32*)(p + file_hash_off + 4 * bucket));
        put_le32(e + 0x1C, (u32)files[i].name.size());
        std::memcpy(e + 0x20, files[i].name.data(), files[i].name.size());
        put_le32(p + file_hash_off + 4 * bucket, file_entry_off[i]);
        data_off += align_up(files[i].size, 0x10);
    }

    for (u32 i = 0; i < n; i++) {
        pad_to(out, 0x10);
        out.append((const char*)files[i].data, files[i].size);
    }
    pad_to(out, 0x10);
    return out;
}

// ---------------------------------------------------------------------------
// control.nacp
// ---------------------------------------------------------------------------

constexpr size_t NACP_SIZE = 0x4000;
constexpr size_t NACP_LANG_COUNT = 16;
constexpr size_t NACP_LANG_STRIDE = 0x300;
constexpr size_t NACP_NAME_CAP = 0x200;
constexpr size_t NACP_AUTHOR_CAP = 0x100;

/// Fill the control NACP for a tile.
///
/// The name goes into all sixteen language slots, as in the `.nro` writer this
/// replaces: the HOME menu reads the slot matching the console language, so
/// filling one would leave the tile blank on every other system setting.
std::string build_nacp(const char* title, u64 program_id) {
    std::string nacp(NACP_SIZE, '\0');
    u8* p = (u8*)&nacp[0];

    for (size_t i = 0; i < NACP_LANG_COUNT; i++) {
        u8* e = p + i * NACP_LANG_STRIDE;
        std::snprintf((char*)e, NACP_NAME_CAP, "%s", title);
        std::snprintf((char*)e + NACP_NAME_CAP, NACP_AUTHOR_CAP, "%s", "FlashNX");
    }

    p[0x3025] = 0;                      // startup_user_account: none, no profile prompt
    p[0x3027] = 1;                      // add_on_content_registration_type
    put_le32(p + 0x3028, 0);            // attribute_flag
    // 0xBFF, not 0xFFFF: there are fifteen languages, and the sixteenth bit
    // claims one that does not exist.
    put_le32(p + 0x302C, 0xBFF);        // supported_language_flag
    put_le32(p + 0x3030, 0);            // parental_control_flag
    p[0x3034] = 0;                      // screenshot: allowed
    p[0x3035] = 2;                      // video_capture: enabled
    p[0x3036] = 0;                      // data_loss_confirmation
    p[0x3037] = 0;                      // play_log_policy
    put_le64(p + 0x3038, program_id);   // presence_group_id
    // An age rating per region, copied from a forwarder the console accepts.
    //
    // 0xFF means "not rated", and filling all thirty-two with it looked like the
    // neutral choice. It is not: a title that is rated nowhere is a title the
    // parental-control check has nothing to compare against, and it is refused
    // before the process is ever created -- which is indistinguishable, from the
    // menu, from a tile that simply does nothing.
    static const u8 RATING_AGE[32] = {
        0x0C, 0xFF, 0xFF, 0x0A, 0xFF, 0x0C, 0x0C, 0x0C,
        0x0C, 0x0C, 0x0D, 0x0D, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    };
    std::memcpy(p + 0x3040, RATING_AGE, sizeof(RATING_AGE));
    // The player's version, not the game's: it says which FlashNX made this
    // tile, which is the useful fact when one stops working.
    std::snprintf((char*)p + 0x3060, 0x10, "%s", FLASHNX_VERSION);
    put_le64(p + 0x3070, program_id + 0x1000); // add_on_content_base_id
    put_le64(p + 0x3078, program_id);          // save_data_owner_id
    std::snprintf((char*)p + 0x30A8, 8, "%s", "flashnx"); // application_error_code_category
    // local_communication_id: the title's own ID in all eight slots, as the
    // reference does. Left at zero it claims to be usable for local play with a
    // title that does not exist.
    for (int i = 0; i < 8; i++) put_le64(p + 0x30B0 + 8 * i, program_id);
    p[0x30F0] = 2;                      // logo_type
    // Auto, not Manual. There is no logo partition here and Manual looked like
    // the honest answer, but the forwarder that works says Auto with no logo
    // partition either, so Auto is what the system expects.
    p[0x30F1] = 0;                      // logo_handling: auto
    put_le64(p + 0x30F8, program_id);   // seed_for_pseudo_device_id
    return nacp;
}

// ---------------------------------------------------------------------------
// NCA
// ---------------------------------------------------------------------------

// NCA header ContentType. Note this is NOT `NcmContentType`: the two
// enumerations disagree (Program is 0 here and 1 there), and mixing them
// produces an NCA the console files under the wrong kind.
enum NcaContentType : u8 {
    NcaContentType_Program = 0,
    NcaContentType_Meta    = 1,
    NcaContentType_Control = 2,
};

constexpr size_t NCA_HEADER_SIZE = 0xC00;
constexpr u64 NCA_SECTION_ALIGN = 0x200;
constexpr u64 IVFC_BLOCK = 0x4000;

struct NcaSection {
    std::string body;      ///< what the section holds, hash layers included
    std::string fs_header; ///< the 0x200 FsHeader describing it
};

/// A PartitionFS section: the PFS0 preceded by a table of SHA-256 over each
/// 0x1000 block of it, and a master hash over that table.
NcaSection make_pfs0_section(const std::string& pfs0) {
    constexpr u32 BLOCK = 0x1000;
    const size_t block_count = (pfs0.size() + BLOCK - 1) / BLOCK;

    std::string hash_table(block_count * 32, '\0');
    for (size_t i = 0; i < block_count; i++) {
        const size_t off = i * BLOCK;
        const size_t len = (off + BLOCK <= pfs0.size()) ? BLOCK : pfs0.size() - off;
        sha256(pfs0.data() + off, len, (u8*)&hash_table[i * 32]);
    }

    u8 master[32];
    sha256(hash_table.data(), hash_table.size(), master);

    NcaSection s;
    s.body = hash_table;
    const u64 pfs0_off = pad_to(s.body, 0x200);
    s.body.append(pfs0);

    s.fs_header.assign(0x200, '\0');
    u8* h = (u8*)&s.fs_header[0];
    put_le16(h + 0x00, 2);  // version
    h[0x02] = 1;            // FsType: PartitionFs
    h[0x03] = 2;            // HashType: HierarchicalSha256
    h[0x04] = 1;            // EncryptionType: None
    u8* hd = h + 0x08;      // HashData: HierarchicalSha256Data
    std::memcpy(hd + 0x00, master, 32);
    put_le32(hd + 0x20, BLOCK);
    put_le32(hd + 0x24, 2); // two layers: the hash table, then the data
    put_le64(hd + 0x28, 0);
    put_le64(hd + 0x30, hash_table.size());
    put_le64(hd + 0x38, pfs0_off);
    put_le64(hd + 0x40, pfs0.size());
    return s;
}

/// A RomFS section, with the six hash layers the format wants.
///
/// Layer five is the image itself; each layer above holds SHA-256 over the
/// 0x4000-byte blocks of the one below, and the master hash covers layer zero.
/// Small images make the upper layers nearly empty, which is expected -- the
/// count is fixed at six whatever the size.
NcaSection make_romfs_section(const std::string& romfs) {
    std::string level[6];
    level[5] = romfs;
    for (int i = 4; i >= 0; i--) {
        const std::string& below = level[i + 1];
        const size_t padded = (size_t)align_up(below.size(), IVFC_BLOCK);
        const size_t blocks = padded / IVFC_BLOCK;
        level[i].assign(blocks * 32, '\0');
        // One 16 KB scratch block, reused: a fresh one per iteration would sit
        // on the stack of a thread with better uses for the room.
        std::vector<u8> block((size_t)IVFC_BLOCK);
        for (size_t b = 0; b < blocks; b++) {
            // Hash the whole block, zero padding included: the layer below is
            // stored padded, so that is what a reader will hash back.
            const size_t off = b * (size_t)IVFC_BLOCK;
            const size_t len = off < below.size()
                ? std::min((size_t)IVFC_BLOCK, below.size() - off) : 0;
            std::memset(block.data(), 0, (size_t)IVFC_BLOCK);
            if (len) std::memcpy(block.data(), below.data() + off, len);
            sha256(block.data(), (size_t)IVFC_BLOCK, (u8*)&level[i][b * 32]);
        }
    }

    // A hash layer is DECLARED at its padded size, not at the size of the hashes
    // it actually holds, and the master hash covers that whole padded block.
    //
    // The bytes are identical either way -- one hash followed by zeros -- so a
    // layer declared at 0x20 looks right to anything that reads it the way it
    // was written. The console does not: it reads a layer a block at a time. Get
    // this wrong and the section is structurally perfect and still unusable.
    // Taken from a forwarder the console accepts, not from reasoning.
    u64 declared[6];
    for (int i = 0; i < 5; i++) declared[i] = align_up(level[i].size(), IVFC_BLOCK);
    declared[5] = level[5].size();

    std::string level0_padded = level[0];
    level0_padded.resize((size_t)declared[0], '\0');
    u8 master[32];
    sha256(level0_padded.data(), level0_padded.size(), master);

    NcaSection s;
    u64 logical[6];
    for (int i = 0; i < 6; i++) {
        logical[i] = pad_to(s.body, IVFC_BLOCK);
        s.body.append(level[i]);
    }
    pad_to(s.body, IVFC_BLOCK);

    s.fs_header.assign(0x200, '\0');
    u8* h = (u8*)&s.fs_header[0];
    put_le16(h + 0x00, 2);  // version
    h[0x02] = 0;            // FsType: RomFs
    // 3, not 1. The HashType values are Auto=0, HierarchicalSha256=2,
    // HierarchicalIntegrity=3 -- there is no 1 in the middle, and 1 is None. A
    // RomFS section claiming None is read from its first byte, which is where
    // the IVFC hash layers live, not the RomFS header: the control data becomes
    // unreadable while the file still looks perfectly well formed.
    h[0x03] = 3;            // HashType: HierarchicalIntegrity
    h[0x04] = 1;            // EncryptionType: None
    u8* hd = h + 0x08;      // HashData: IntegrityMetaInfo
    std::memcpy(hd + 0x00, "IVFC", 4);
    put_le32(hd + 0x04, 0x20000); // version
    put_le32(hd + 0x08, 0x20);    // master hash size
    put_le32(hd + 0x0C, 7);       // max layers, one more than the six described
    for (int i = 0; i < 6; i++) {
        u8* lv = hd + 0x10 + 0x18 * i;
        put_le64(lv + 0x00, logical[i]);
        put_le64(lv + 0x08, declared[i]);
        put_le32(lv + 0x10, 14); // log2(0x4000)
        put_le32(lv + 0x14, 0);
    }
    std::memcpy(hd + 0xC0, master, 32);
    return s;
}

struct BuiltNca {
    std::string data;
    u8 content_id[16];
    u8 hash[32];
};

/// Wrap one section into a finished NCA.
///
/// Both signatures stay zero. The first has no private key outside Nintendo, and
/// the second is checked against a public key in the NPDM that we would have to
/// sign the NPDM with -- and a self-signed NPDM fails the same check the first
/// signature fails, on the same consoles, so it would buy nothing. This is why
/// the file needs signature patches to mount, and it is what every homebrew NSP
/// does.
BuiltNca build_nca(u64 program_id, NcaContentType type,
                   const std::vector<NcaSection>& sections,
                   const u8 header_key[0x20]) {
    std::string out;
    out.assign(NCA_HEADER_SIZE, '\0');

    // Where each section landed, in bytes. The table in the header wants these
    // in 0x200 blocks, which is also why every section starts on one.
    u64 start[4] = {}, end[4] = {};
    for (size_t i = 0; i < sections.size(); i++) {
        start[i] = pad_to(out, NCA_SECTION_ALIGN);
        out.append(sections[i].body);
        end[i] = align_up(out.size(), NCA_SECTION_ALIGN);
        out.resize((size_t)end[i], '\0');
    }

    u8* h = (u8*)&out[0];
    u8* hdr = h + 0x200;
    std::memcpy(hdr + 0x00, "NCA3", 4);
    hdr[0x04] = 0;    // DistributionType: Download
    hdr[0x05] = type;
    hdr[0x06] = 0;    // KeyGenerationOld: 1.0.0
    hdr[0x07] = 0;    // KeyAreaEncryptionKeyIndex: Application
    put_le64(hdr + 0x08, out.size());
    put_le64(hdr + 0x10, program_id);
    put_le32(hdr + 0x18, 0);          // ContentIndex
    put_le32(hdr + 0x1C, 0x000C1100); // SdkAddonVersion
    hdr[0x20] = 0;                    // KeyGeneration
    // RightsId stays zero: no ticket, no title key, nothing for `es` to check.

    // One entry, one FsHeader and one hash per section. The hash table at 0x280
    // is what ties each FsHeader to the signed part of the header.
    for (size_t i = 0; i < sections.size(); i++) {
        u8* entry = hdr + 0x40 + 0x10 * i;
        put_le32(entry + 0x00, (u32)(start[i] / NCA_SECTION_ALIGN));
        put_le32(entry + 0x04, (u32)(end[i] / NCA_SECTION_ALIGN));
        put_le32(entry + 0x08, 1); // enabled
        std::memcpy(h + 0x400 + 0x200 * i, sections[i].fs_header.data(), 0x200);
        sha256(sections[i].fs_header.data(), 0x200, hdr + 0x80 + 0x20 * i);
    }

    encrypt_header(h, NCA_HEADER_SIZE, header_key);

    BuiltNca nca;
    nca.data = std::move(out);
    sha256(nca.data.data(), nca.data.size(), nca.hash);
    // An NCA is named after the first half of its own hash, which is how the
    // console finds it again without an index.
    std::memcpy(nca.content_id, nca.hash, 16);
    return nca;
}

/// `<32 lowercase hex>.nca`, the name every content file carries.
std::string nca_filename(const u8 id[16], bool meta) {
    char name[64];
    char* w = name;
    for (int i = 0; i < 16; i++) w += std::sprintf(w, "%02x", id[i]);
    std::sprintf(w, meta ? ".cnmt.nca" : ".nca");
    return std::string(name);
}

// ---------------------------------------------------------------------------
// The forwarder's ExeFS, with this tile's identity written into it
// ---------------------------------------------------------------------------

/// Rewrite the program ID in the embedded ExeFS's `main.npdm`.
///
/// The NPDM names the program three times: once in the ACI0, which is what the
/// process gets, and twice in the ACID as the range it is allowed to claim. They
/// are found by following the offsets in the META header rather than by fixed
/// position, because a rebuild of the forwarder moves them.
///
/// The ACID signature does not survive this, and is not meant to: see the
/// comment on `build_nca`.
bool patch_exefs_program_id(std::string& exefs, u64 program_id) {
    u8* p = (u8*)&exefs[0];
    const size_t n = exefs.size();

    // Find `main.npdm` inside the PFS0 rather than assuming it is second.
    if (n < 0x10 || std::memcmp(p, "PFS0", 4) != 0) return false;
    const u32 count = *(const u32*)(p + 0x04);
    const u32 string_table_size = *(const u32*)(p + 0x08);
    const size_t entries_end = 0x10 + 0x18 * (size_t)count;
    const size_t data_start = entries_end + string_table_size;
    if (data_start > n) return false;

    size_t npdm_off = 0, npdm_size = 0;
    for (u32 i = 0; i < count; i++) {
        const u8* e = p + 0x10 + 0x18 * i;
        const u32 name_off = *(const u32*)(e + 0x10);
        if (entries_end + name_off >= data_start) return false;
        if (std::strcmp((const char*)p + entries_end + name_off, "main.npdm") == 0) {
            npdm_off = data_start + *(const u64*)(e + 0x00);
            npdm_size = (size_t)*(const u64*)(e + 0x08);
            break;
        }
    }
    if (!npdm_size || npdm_off + npdm_size > n) return false;

    u8* meta = p + npdm_off;
    if (npdm_size < 0x80 || std::memcmp(meta, "META", 4) != 0) return false;
    const u32 aci_off  = *(const u32*)(meta + 0x70);
    const u32 aci_size = *(const u32*)(meta + 0x74);
    const u32 acid_off = *(const u32*)(meta + 0x78);
    const u32 acid_size = *(const u32*)(meta + 0x7C);
    if (aci_off + aci_size > npdm_size || acid_off + acid_size > npdm_size) return false;

    u8* aci = meta + aci_off;
    u8* acid = meta + acid_off;
    if (std::memcmp(aci, "ACI0", 4) != 0 || std::memcmp(acid + 0x200, "ACID", 4) != 0) return false;

    put_le64(aci + 0x10, program_id);   // the program's own ID
    put_le64(acid + 0x210, program_id); // range min
    put_le64(acid + 0x218, program_id); // range max
    return true;
}

// ---------------------------------------------------------------------------
// The content metadata (CNMT)
// ---------------------------------------------------------------------------

/// Build `Application_<id>.cnmt`, the file that goes inside the meta NCA.
///
/// This is the PACKAGED form: a 0x20 header carrying the title's identity, and
/// one record per content WITH its hash. It lists the program and the control
/// only -- a meta NCA does not list itself. The database wants a different
/// shape entirely, which is `build_cnmt_db` below; using one where the other
/// belongs is the classic way to install a title the HOME menu shows as
/// corrupt.
std::string build_cnmt_file(u64 program_id, const BuiltNca& program, const BuiltNca& control) {
    // ContentMetaHeader, then NcmApplicationMetaExtendedHeader, then the
    // packaged content records. The meta NCA is not listed in its own list.
    std::string out(0x20 + 0x10, '\0');
    u8* p = (u8*)&out[0];
    put_le64(p + 0x00, program_id);
    put_le32(p + 0x08, 0);              // version
    p[0x0C] = NcmContentMetaType_Application;
    p[0x0D] = 0;                        // reserved
    put_le16(p + 0x0E, 0x10);           // extended header size
    put_le16(p + 0x10, 2);              // content count: program + control
    put_le16(p + 0x12, 0);              // content meta count
    p[0x14] = NcmContentMetaAttribute_None;
    p[0x15] = NcmStorageId_None;
    p[0x16] = NcmContentInstallType_Full;
    p[0x17] = 0;
    put_le32(p + 0x18, 0);              // required download system version
    put_le32(p + 0x1C, 0);

    u8* ext = p + 0x20;
    put_le64(ext + 0x00, program_id + 0x800); // patch ID, by convention
    put_le32(ext + 0x08, 0);                  // required system version
    put_le32(ext + 0x0C, 0);                  // required application version

    auto append_content = [&out](const BuiltNca& nca, u8 type) {
        const size_t at = out.size();
        out.resize(at + 0x38, '\0');
        u8* c = (u8*)&out[at];
        std::memcpy(c + 0x00, nca.hash, 32);
        std::memcpy(c + 0x20, nca.content_id, 16);
        // A PackagedContentInfo is a 0x20 hash followed by an NcmContentInfo, so
        // these five fields are at 0x20 + the offsets ncm_types.h gives. Getting
        // `attr` and `content_type` the wrong way round leaves every content
        // typed Meta and folds the attribute byte into the size, which reads back
        // as a terabyte. It is invisible in the database form, whose layout is
        // one byte tighter.
        put_le32(c + 0x30, (u32)nca.data.size()); // size_low
        c[0x34] = (u8)(nca.data.size() >> 32);    // size_high
        c[0x35] = 0;                              // attr
        c[0x36] = type;                           // content_type
        c[0x37] = 0;                              // id_offset
    };
    append_content(program, NcmContentType_Program);
    append_content(control, NcmContentType_Control);

    // The digest closes the file. Nothing reads it back on install, but a
    // truncated CNMT is one some tools refuse to look at.
    const size_t at = out.size();
    out.resize(at + 32, 0);
    sha256(out.data(), at, (u8*)&out[at]);
    return out;
}

/// Build what `ncmContentMetaDatabaseSet` expects, which is NOT the file above.
///
/// Three differences, all of them load-bearing: the header is 0x8 bytes and
/// carries no title ID (that lives in the key passed alongside), the content
/// records drop their hashes, and the meta NCA DOES appear in the list -- the
/// database is what the console reads to find every file belonging to the title,
/// including the one holding this list.
std::string build_cnmt_db(const BuiltNca& program, const BuiltNca& control,
                          const BuiltNca& meta, u64 program_id) {
    std::string out(0x8 + 0x10, 0);
    u8* p = (u8*)&out[0];
    put_le16(p + 0x00, 0x10); // extended header size
    put_le16(p + 0x02, 3);    // program + control + meta
    put_le16(p + 0x04, 0);    // content meta count
    p[0x06] = NcmContentMetaAttribute_None;
    p[0x07] = NcmStorageId_None; // ncm fills this in from the storage it is on

    u8* ext = p + 0x08;
    put_le64(ext + 0x00, program_id + 0x800);
    put_le32(ext + 0x08, 0);
    put_le32(ext + 0x0C, 0);

    auto append = [&out](const BuiltNca& nca, u8 type) {
        const size_t at = out.size();
        out.resize(at + 0x18, 0);
        u8* c = (u8*)&out[at];
        std::memcpy(c + 0x00, nca.content_id, 16);
        put_le32(c + 0x10, (u32)nca.data.size());
        c[0x14] = (u8)(nca.data.size() >> 32);
        c[0x15] = 0; // attr
        c[0x16] = type;
        c[0x17] = 0; // id offset
    };
    append(program, NcmContentType_Program);
    append(control, NcmContentType_Control);
    append(meta, NcmContentType_Meta);
    return out;
}

// ---------------------------------------------------------------------------
// The whole tile
// ---------------------------------------------------------------------------

struct Tile {
    BuiltNca program, control, meta;
    std::string cnmt_db; ///< the database form, for ncmContentMetaDatabaseSet
    u64 program_id = 0;
};

bool build_tile(Tile& out, u64 program_id, const char* title, const char* swf_path,
                const u8* icon, u32 icon_len, char* err, size_t err_cap) {
    auto fail = [&](const char* why) {
        std::snprintf(err, err_cap, "%s", why);
        return false;
    };

    u8 header_key[0x20];
    if (!derive_header_key(header_key)) return fail("spl: header key");

    const size_t forwarder_len = (size_t)(flashnx_forwarder_nsp_end - flashnx_forwarder_nsp);
    if (forwarder_len < 0x100) return fail("no forwarder in this build");

    std::string exefs((const char*)flashnx_forwarder_nsp, forwarder_len);
    if (!patch_exefs_program_id(exefs, program_id)) return fail("npdm patch");

    out.program_id = program_id;
    // ExeFS *and* a RomFS. The RomFS is not optional in practice: both working
    // forwarders on hand carry one in their program NCA (Sphaira's has ExeFS +
    // RomFS, hacBrewPack's has ExeFS + RomFS + logo), and a program NCA with an
    // ExeFS alone installs perfectly, shows its tile, and is then refused at
    // launch with no process ever created and no crash report to explain it.
    //
    // What goes in it is the game's path. The forwarder does not read it today
    // -- it uses the `.cfg` on the card, which keeps the binary identical for
    // every tile -- but it makes each tile self-describing, which is worth the
    // dozen bytes it costs.
    std::vector<PfsFile> program_files;
    program_files.push_back({"target.txt", (const u8*)swf_path, std::strlen(swf_path)});
    std::vector<NcaSection> program_sections;
    program_sections.push_back(make_pfs0_section(exefs));
    program_sections.push_back(make_romfs_section(build_romfs(program_files)));
    out.program = build_nca(program_id, NcaContentType_Program, program_sections, header_key);

    const std::string nacp = build_nacp(title, program_id);
    std::vector<PfsFile> control_files;
    control_files.push_back({"control.nacp", (const u8*)nacp.data(), nacp.size()});
    // One icon, for American English. The HOME menu falls back to it for any
    // language whose own icon is missing, so a game with a cover shows it
    // everywhere and a game without one shows the console's placeholder.
    if (icon && icon_len) {
        control_files.push_back({"icon_AmericanEnglish.dat", icon, icon_len});
    }
    out.control = build_nca(program_id, NcaContentType_Control,
                            {make_romfs_section(build_romfs(control_files))}, header_key);

    const std::string cnmt_file = build_cnmt_file(program_id, out.program, out.control);
    char cnmt_name[64];
    std::snprintf(cnmt_name, sizeof(cnmt_name), "Application_%016lx.cnmt", program_id);
    std::vector<PfsFile> meta_files;
    meta_files.push_back({cnmt_name, (const u8*)cnmt_file.data(), cnmt_file.size()});
    out.meta = build_nca(program_id, NcaContentType_Meta,
                         {make_pfs0_section(build_pfs0(meta_files))}, header_key);

    // Last, because it has to name the meta NCA that was just built from it.
    out.cnmt_db = build_cnmt_db(out.program, out.control, out.meta, program_id);
    return true;
}

// ---------------------------------------------------------------------------
// Installing
// ---------------------------------------------------------------------------

/// `ns`'s PushApplicationRecord, which libnx does not wrap.
///
/// This is the call that makes a tile appear. Everything before it only puts
/// files where the console can find them; the HOME menu reads this list.
Result push_application_record(u64 program_id, const void* records, size_t size) {
    Service srv;
    Result rc = nsGetApplicationManagerInterface(&srv);
    if (R_FAILED(rc)) return rc;

    const struct {
        u8 last_modified_event;
        u8 pad[7];
        u64 application_id;
    } in = {3, {}, program_id};

    rc = serviceDispatchIn(&srv, 16, in,
        .buffer_attrs = { SfBufferAttr_HipcMapAlias | SfBufferAttr_In },
        .buffers = { { records, size } },
    );
    serviceClose(&srv);
    return rc;
}

/// Write one NCA into content storage, through a placeholder.
///
/// The placeholder is the console's own staging area: content only becomes
/// visible when `Register` renames it, so a write that dies halfway leaves
/// garbage that `CleanupAllPlaceHolder` sweeps rather than a half-installed
/// title.
Result write_content(NcmContentStorage* cs, const BuiltNca& nca) {
    NcmContentId content_id;
    std::memcpy(content_id.c, nca.content_id, 16);

    NcmPlaceHolderId ph;
    Result rc = ncmContentStorageGeneratePlaceHolderId(cs, &ph);
    if (R_FAILED(rc)) return rc;

    // An earlier attempt may have left one behind; its presence is not an error.
    ncmContentStorageDeletePlaceHolder(cs, &ph);

    rc = ncmContentStorageCreatePlaceHolder(cs, &content_id, &ph, (s64)nca.data.size());
    if (R_FAILED(rc)) return rc;

    constexpr size_t CHUNK = 0x100000;
    for (size_t off = 0; off < nca.data.size(); off += CHUNK) {
        const size_t len = std::min(CHUNK, nca.data.size() - off);
        rc = ncmContentStorageWritePlaceHolder(cs, &ph, off, nca.data.data() + off, len);
        if (R_FAILED(rc)) {
            ncmContentStorageDeletePlaceHolder(cs, &ph);
            return rc;
        }
    }

    // Registering over an existing copy fails, so drop the old one first. This
    // is what makes re-installing the same tile an update rather than an error.
    ncmContentStorageDelete(cs, &content_id);
    rc = ncmContentStorageRegister(cs, &content_id, &ph);
    if (R_FAILED(rc)) ncmContentStorageDeletePlaceHolder(cs, &ph);
    return rc;
}

} // namespace

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// The program ID for a game, derived from its path.
///
/// Derived and not random, so that installing the same game twice replaces its
/// tile instead of adding a second one, and so that removing a tile does not
/// need a file to remember which ID it had.
///
/// The 0x05 prefix is what Sphaira uses today (`owo.cpp`, where 0x01 survives
/// only as `old_tid`, to find and remove tiles made by older versions). A
/// forwarder it installed on this very console carries 055F3F0CFAC09000, so the
/// range is not the thing that stops a tile from launching -- worth writing down,
/// because DBI warns that such an ID "belongs to an add-on" and it is tempting to
/// believe it. Forty-four bits of SHA-256 is what keeps it off a real game.
///
/// The low twelve bits stay clear because the console uses them for patch and
/// add-on IDs derived from this one.
extern "C" u64 nsp_program_id_for(const char* swf_path) {
    u8 digest[32];
    sha256(swf_path, std::strlen(swf_path), digest);
    u64 id = 0;
    for (int i = 0; i < 8; i++) id = (id << 8) | digest[i];
    return 0x0500000000000000ull | (id & 0x00FFFFFFFFFFF000ull);
}

/// Write the tile to the card as a `.nsp`, declared here so installing can also
/// dump. Defined at the bottom of the file.
extern "C" int nsp_write_file(u64 program_id, const char* title, const char* swf_path,
                              const unsigned char* icon, unsigned int icon_len,
                              const char* out_path, char* err, unsigned int err_cap);

/// Build a tile and install it. 1 on success, 0 on failure with `err` set.
///
/// `err` is filled with something specific on every path, because the failures
/// here are not interchangeable: a console without signature patches, a full SD
/// card and a build without a forwarder all look identical from the menu.
extern "C" int nsp_install_tile(u64 program_id, const char* title, const char* swf_path,
                                const unsigned char* icon, unsigned int icon_len,
                                char* err, unsigned int err_cap) {
    if (err_cap) err[0] = 0;

    Tile tile;
    if (!build_tile(tile, program_id, title, swf_path, icon, icon_len, err, err_cap)) return 0;

    // Debug marker, the same shape as `noalloc.on` and `trace.on`: when
    // `sdmc:/switch/FlashNX/nsp_dump.on` is present, the tile is ALSO written out
    // as an installable `.nsp` before anything touches the console's own storage.
    //
    // Before, deliberately. That file is what gets handed to `hactool` when a
    // tile misbehaves, and it installs with any other tool -- so it has to exist
    // even when the install that follows is what goes wrong.
    {
        struct stat st;
        if (::stat("sdmc:/switch/FlashNX/nsp_dump.on", &st) == 0) {
            char path[128];
            std::snprintf(path, sizeof(path),
                          "sdmc:/switch/FlashNX/tiles/%016lX.nsp", program_id);
            char dump_err[96] = {};
            const int ok = nsp_write_file(program_id, title, swf_path, icon, icon_len, path,
                                          dump_err, sizeof(dump_err));
            std::printf("nsp: dump %s -> %s\n", ok ? "ok" : dump_err, path);
            std::fflush(stdout);
        }
    }

    // Installing over a tile that is already there fails at the record push, and
    // re-installing has to work: it is how a renamed game or a new cover reaches
    // the home screen. So the old one goes first, completely -- record, content
    // and save slot -- and what follows starts from nothing.
    if (nsp_tile_exists(program_id)) nsp_remove_tile(program_id);

    Result rc = ncmInitialize();
    if (R_FAILED(rc)) {
        std::snprintf(err, err_cap, "ncm init 0x%x", rc);
        return 0;
    }

    NcmContentStorage cs = {};
    NcmContentMetaDatabase db = {};
    // The card, not the console's own storage: the game it launches is on the
    // card anyway, so a tile that outlived it would only be a dead icon.
    const NcmStorageId storage = NcmStorageId_SdCard;

    auto cleanup = [&]() {
        ncmContentMetaDatabaseClose(&db);
        ncmContentStorageClose(&cs);
        ncmExit();
    };

    rc = ncmOpenContentStorage(&cs, storage);
    if (R_FAILED(rc)) {
        std::snprintf(err, err_cap, "open storage 0x%x", rc);
        ncmExit();
        return 0;
    }
    rc = ncmOpenContentMetaDatabase(&db, storage);
    if (R_FAILED(rc)) {
        std::snprintf(err, err_cap, "open meta db 0x%x", rc);
        ncmContentStorageClose(&cs);
        ncmExit();
        return 0;
    }

    NcmContentMetaKey key = {};
    key.id = program_id;
    key.version = 0;
    key.type = NcmContentMetaType_Application;
    key.install_type = NcmContentInstallType_Full;

    // Everything registered so far, so a failure half-way can put the console
    // back the way it was found.
    //
    // This is not tidiness. Content registered with no metadata pointing at it is
    // invisible from the HOME menu but real to `ncm`, and it is what people are
    // chasing when they hit `0x805 Ncm` -- at which point the advice they find
    // tells them to delete `/atmosphere/contents/`, taking every sysmodule and
    // LayeredFS mod they own with it. An install that fails has to leave nothing.
    std::vector<NcmContentId> registered;
    bool meta_committed = false;

    auto rollback = [&]() {
        if (meta_committed) {
            ncmContentMetaDatabaseRemove(&db, &key);
            ncmContentMetaDatabaseCommit(&db);
        }
        for (const NcmContentId& id : registered) {
            ncmContentStorageDelete(&cs, &id);
        }
    };

    for (const BuiltNca* nca : {&tile.program, &tile.control, &tile.meta}) {
        rc = write_content(&cs, *nca);
        if (R_FAILED(rc)) {
            std::snprintf(err, err_cap, "write content 0x%x", rc);
            rollback();
            cleanup();
            return 0;
        }
        NcmContentId id;
        std::memcpy(id.c, nca->content_id, 16);
        registered.push_back(id);
    }

    rc = ncmContentMetaDatabaseSet(&db, &key, tile.cnmt_db.data(), tile.cnmt_db.size());
    if (R_SUCCEEDED(rc)) rc = ncmContentMetaDatabaseCommit(&db);
    if (R_FAILED(rc)) {
        std::snprintf(err, err_cap, "meta db set 0x%x", rc);
        rollback();
        cleanup();
        return 0;
    }
    meta_committed = true;

    // The storage handles stay open past this point on purpose: if the record
    // push fails, `rollback` still needs them.
    //
    // One storage record per content meta, which is what ties the record to the
    // card it was installed on.
    const struct {
        NcmContentMetaKey key;
        u8 storage_id;
        u8 pad[7];
    } record = {key, (u8)storage, {}};

    rc = nsInitialize();
    if (R_SUCCEEDED(rc)) {
        rc = push_application_record(program_id, &record, sizeof(record));
        nsExit();
    } else {
        std::snprintf(err, err_cap, "ns init 0x%x", rc);
    }

    if (R_FAILED(rc)) {
        if (err[0] == 0) std::snprintf(err, err_cap, "push record 0x%x", rc);
        // The record is the step that makes a tile visible, so failing here is
        // the one case where everything written above is already orphaned.
        rollback();
        cleanup();
        return 0;
    }
    cleanup();
    std::snprintf(err, err_cap, "ok");
    return 1;
}

/// Remove a tile: its record and its content. 1 when something was removed.
extern "C" int nsp_remove_tile(u64 program_id) {
    Result rc = nsInitialize();
    if (R_FAILED(rc)) return 0;
    // Completely: the record, the contents, and the save data slot the console
    // opened for it. Leaving any of the three behind is what produces a tile
    // that will neither launch nor uninstall.
    rc = nsDeleteApplicationCompletely(program_id);
    nsExit();
    return R_SUCCEEDED(rc) ? 1 : 0;
}

/// Is a tile for this program ID installed? 1 yes, 0 no.
extern "C" int nsp_tile_exists(u64 program_id) {
    if (R_FAILED(nsInitialize())) return 0;

    int found = 0;
    NsApplicationRecord records[32];
    s32 offset = 0;
    while (true) {
        s32 count = 0;
        if (R_FAILED(nsListApplicationRecord(records, 32, offset, &count)) || count <= 0) break;
        for (s32 i = 0; i < count; i++) {
            if (records[i].application_id == program_id) {
                found = 1;
                break;
            }
        }
        if (found || count < 32) break;
        offset += count;
    }
    nsExit();
    return found;
}

/// Write the tile to the card as a `.nsp` instead of installing it.
///
/// Same three NCAs, wrapped in a PFS0. This exists so the output can be checked
/// against a tool that already reads the format, and so a console that cannot
/// install still has something it can hand to one that can.
extern "C" int nsp_write_file(u64 program_id, const char* title, const char* swf_path,
                              const unsigned char* icon, unsigned int icon_len,
                              const char* out_path, char* err, unsigned int err_cap) {
    Tile tile;
    if (!build_tile(tile, program_id, title, swf_path, icon, icon_len, err, err_cap)) return 0;

    const std::string program_name = nca_filename(tile.program.content_id, false);
    const std::string control_name = nca_filename(tile.control.content_id, false);
    const std::string meta_name = nca_filename(tile.meta.content_id, true);

    std::vector<PfsFile> files;
    files.push_back({program_name, (const u8*)tile.program.data.data(), tile.program.data.size()});
    files.push_back({control_name, (const u8*)tile.control.data.data(), tile.control.data.size()});
    files.push_back({meta_name, (const u8*)tile.meta.data.data(), tile.meta.data.size()});
    const std::string nsp = build_pfs0(files);

    // Through the C++ writer like every other file this app puts on the card:
    // Rust's `std::fs::write` reports success on Horizon and the file then reads
    // back ENOENT on some writes.
    if (swf_picker_write_file(out_path, (const unsigned char*)nsp.data(),
                              (unsigned int)nsp.size()) != 1) {
        std::snprintf(err, err_cap, "write %s", out_path);
        return 0;
    }
    std::snprintf(err, err_cap, "ok");
    return 1;
}
