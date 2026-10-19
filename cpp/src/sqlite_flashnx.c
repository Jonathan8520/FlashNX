// SQLite for FlashNX: the amalgamation (cpp/third_party/sqlite, public domain)
// built with the options below. Its only file system, read-only, is in
// sqlite_vfs.c: SQLite's own `u64` and libnx's are different types, so the
// two cannot share a file.
//
// Why it is here (#130): a few Flashpoint games answer their server's requests
// from a SQLite database shipped in their GameZIP, through PHP scripts that
// Flashpoint's own web server runs. Happy Wheels is one: 5.2 million levels in
// an 18 GB `happywheels.sqlite3`, asked through `get_level.hw` and `replay.hw`.
// FlashNX answers those requests itself (rust/src/backend/hwserver.rs), with
// the same queries, through this.
//
// The database is opened read-only and declared immutable: no journal, no
// locks, no temporary files (sorting and temporary tables live in memory).

#define SQLITE_OS_OTHER 1
#define SQLITE_THREADSAFE 0
#define SQLITE_TEMP_STORE 3
#define SQLITE_DEFAULT_MEMSTATUS 0
#define SQLITE_DQS 0
#define SQLITE_MAX_MMAP_SIZE 0
// Only options SQLite documents as safe with the amalgamation.
#define SQLITE_OMIT_LOAD_EXTENSION 1
#define SQLITE_OMIT_SHARED_CACHE 1
#define SQLITE_OMIT_DEPRECATED 1
#define SQLITE_OMIT_PROGRESS_CALLBACK 1
#define SQLITE_OMIT_JSON 1

// A few internal helpers only the omitted JSON functions call.
#pragma GCC diagnostic ignored "-Wunused-function"
#include "sqlite3.c"
