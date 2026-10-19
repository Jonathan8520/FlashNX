// The only file system SQLite gets in FlashNX (sqlite_flashnx.c), read-only.
// Reads go straight to the file system at the page's offset (fsFileRead),
// which also reads a concatenation file past 4 GB on a FAT32 card. The
// database is declared immutable: SQLite then asks for no journal and no lock.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <switch.h>

#include "sqlite3.h"

// Read-ahead for a walk through the file: a list or a search reads a whole
// index of Happy Wheels' database page after page (4 KB each, on consecutive
// pages), and each read was its own request to the file system: about 80,000
// of them, 33 s for a search by name (#130). When a read starts where the
// previous one ended, this much is read at once and the next pages come from
// memory. A read elsewhere (one level's data) stays a read of its own size.
#define NX_READ_AHEAD (512 * 1024)

typedef struct NxFile {
    sqlite3_file base;
    FsFile file;
    unsigned char* ahead;   // NX_READ_AHEAD bytes, made on the first walk
    s64 ahead_off;          // what `ahead` holds: [ahead_off, ahead_off + ahead_len)
    u64 ahead_len;
    s64 last_end;           // where the previous read ended
} NxFile;

static int nxClose(sqlite3_file* f) {
    NxFile* nf = (NxFile*)f;
    fsFileClose(&nf->file);
    free(nf->ahead);
    nf->ahead = 0;
    return SQLITE_OK;
}

static int nxRead(sqlite3_file* f, void* buf, int amt, sqlite3_int64 off) {
    NxFile* nf = (NxFile*)f;
    const s64 start = (s64)off;
    const s64 end = start + amt;
    // Already read ahead.
    if (nf->ahead && start >= nf->ahead_off && end <= nf->ahead_off + (s64)nf->ahead_len) {
        memcpy(buf, nf->ahead + (start - nf->ahead_off), (size_t)amt);
        nf->last_end = end;
        return SQLITE_OK;
    }
    // A walk: this read follows the previous one.
    if (start == nf->last_end && amt <= NX_READ_AHEAD) {
        if (!nf->ahead) nf->ahead = (unsigned char*)malloc(NX_READ_AHEAD);
        if (nf->ahead) {
            u64 got = 0;
            const Result rc = fsFileRead(&nf->file, start, nf->ahead, NX_READ_AHEAD, FsReadOption_None, &got);
            if (R_SUCCEEDED(rc) && got >= (u64)amt) {
                nf->ahead_off = start;
                nf->ahead_len = got;
                memcpy(buf, nf->ahead, (size_t)amt);
                nf->last_end = end;
                return SQLITE_OK;
            }
            nf->ahead_len = 0;
        }
    }
    u64 got = 0;
    const Result rc = fsFileRead(&nf->file, start, buf, (u64)amt, FsReadOption_None, &got);
    if (R_FAILED(rc)) return SQLITE_IOERR_READ;
    nf->last_end = end;
    if (got < (u64)amt) {
        // SQLite wants the rest zeroed on a short read (past the end).
        memset((char*)buf + got, 0, (size_t)amt - (size_t)got);
        return SQLITE_IOERR_SHORT_READ;
    }
    return SQLITE_OK;
}

static int nxWrite(sqlite3_file* f, const void* buf, int amt, sqlite3_int64 off) {
    (void)f; (void)buf; (void)amt; (void)off;
    return SQLITE_READONLY;
}

static int nxTruncate(sqlite3_file* f, sqlite3_int64 size) {
    (void)f; (void)size;
    return SQLITE_READONLY;
}

static int nxSync(sqlite3_file* f, int flags) {
    (void)f; (void)flags;
    return SQLITE_OK;
}

static int nxFileSize(sqlite3_file* f, sqlite3_int64* size) {
    s64 s = 0;
    if (R_FAILED(fsFileGetSize(&((NxFile*)f)->file, &s))) return SQLITE_IOERR_FSTAT;
    *size = (sqlite3_int64)s;
    return SQLITE_OK;
}

static int nxLock(sqlite3_file* f, int level) { (void)f; (void)level; return SQLITE_OK; }
static int nxUnlock(sqlite3_file* f, int level) { (void)f; (void)level; return SQLITE_OK; }
static int nxCheckReservedLock(sqlite3_file* f, int* out) { (void)f; *out = 0; return SQLITE_OK; }
static int nxFileControl(sqlite3_file* f, int op, void* arg) { (void)f; (void)op; (void)arg; return SQLITE_NOTFOUND; }
static int nxSectorSize(sqlite3_file* f) { (void)f; return 4096; }
static int nxDeviceCharacteristics(sqlite3_file* f) { (void)f; return SQLITE_IOCAP_IMMUTABLE; }

static const sqlite3_io_methods nx_io = {
    1,
    nxClose, nxRead, nxWrite, nxTruncate, nxSync, nxFileSize,
    nxLock, nxUnlock, nxCheckReservedLock, nxFileControl,
    nxSectorSize, nxDeviceCharacteristics,
    0, 0, 0, 0, 0, 0,
};

static int nxOpen(sqlite3_vfs* vfs, const char* name, sqlite3_file* f, int flags, int* out_flags) {
    (void)vfs;
    NxFile* nf = (NxFile*)f;
    nf->base.pMethods = 0;
    nf->ahead = 0;
    nf->ahead_off = 0;
    nf->ahead_len = 0;
    nf->last_end = -1;
    // Only the database itself: read-only and immutable, it never needs a
    // journal, and temporary storage is in memory (SQLITE_TEMP_STORE 3).
    if (!name || !(flags & SQLITE_OPEN_MAIN_DB) || (flags & (SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE))) {
        return SQLITE_CANTOPEN;
    }
    FsFileSystem* fs = 0;
    char fs_path[FS_MAX_PATH];
    if (fsdevTranslatePath(name, &fs, fs_path) == -1) return SQLITE_CANTOPEN;
    if (R_FAILED(fsFsOpenFile(fs, fs_path, FsOpenMode_Read, &nf->file))) return SQLITE_CANTOPEN;
    nf->base.pMethods = &nx_io;
    if (out_flags) *out_flags = SQLITE_OPEN_READONLY;
    return SQLITE_OK;
}

static int nxDelete(sqlite3_vfs* vfs, const char* name, int sync) {
    (void)vfs; (void)name; (void)sync;
    return SQLITE_IOERR_DELETE;
}

static int nxAccess(sqlite3_vfs* vfs, const char* name, int flags, int* out) {
    (void)vfs; (void)flags;
    struct stat st;
    *out = (stat(name, &st) == 0);
    return SQLITE_OK;
}

static int nxFullPathname(sqlite3_vfs* vfs, const char* name, int n, char* out) {
    (void)vfs;
    sqlite3_snprintf(n, out, "%s", name);
    return SQLITE_OK;
}

static int nxRandomness(sqlite3_vfs* vfs, int n, char* out) {
    (void)vfs;
    u64 x = armGetSystemTick() | 1;
    for (int i = 0; i < n; i++) {
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        out[i] = (char)x;
    }
    return n;
}

static int nxSleep(sqlite3_vfs* vfs, int us) {
    (void)vfs;
    svcSleepThread((s64)us * 1000);
    return us;
}

static int nxCurrentTimeInt64(sqlite3_vfs* vfs, sqlite3_int64* now) {
    (void)vfs;
    // Julian day in milliseconds: the Unix epoch is day 2440587.5.
    *now = (sqlite3_int64)time(0) * 1000 + 210866760000000LL;
    return SQLITE_OK;
}

static int nxCurrentTime(sqlite3_vfs* vfs, double* now) {
    sqlite3_int64 ms = 0;
    nxCurrentTimeInt64(vfs, &ms);
    *now = ms / 86400000.0;
    return SQLITE_OK;
}

static int nxGetLastError(sqlite3_vfs* vfs, int n, char* out) {
    (void)vfs; (void)n; (void)out;
    return 0;
}

static sqlite3_vfs nx_vfs = {
    2,                  // iVersion
    sizeof(NxFile),     // szOsFile
    FS_MAX_PATH,        // mxPathname
    0,                  // pNext
    "flashnx",          // zName
    0,                  // pAppData
    nxOpen, nxDelete, nxAccess, nxFullPathname,
    0, 0, 0, 0,         // no dynamic libraries
    nxRandomness, nxSleep, nxCurrentTime, nxGetLastError,
    nxCurrentTimeInt64,
    0, 0, 0,
};

// Called by sqlite3_initialize (SQLITE_OS_OTHER: SQLite has no file system of
// its own).
int sqlite3_os_init(void) {
    return sqlite3_vfs_register(&nx_vfs, 1);
}

int sqlite3_os_end(void) {
    return SQLITE_OK;
}
