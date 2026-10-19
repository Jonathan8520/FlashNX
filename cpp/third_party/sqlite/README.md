# SQLite

The [SQLite](https://sqlite.org) amalgamation, version 3.53.4 (2026-07-24),
`sqlite3.c` and `sqlite3.h` as published in `sqlite-amalgamation-3530400.zip`.
SQLite is in the public domain.

It is not compiled on its own: `cpp/src/sqlite_flashnx.c` sets the build
options and includes `sqlite3.c`, and `cpp/src/sqlite_vfs.c` is the only file
system SQLite gets in FlashNX, read-only, reading the database straight from
the SD card (a concatenation file past 4 GB included).

FlashNX uses it to answer the server requests of Flashpoint games that ship
their server's data as a SQLite database, such as Happy Wheels' level database
(`rust/src/backend/hwserver.rs`).

To update: replace both files with those of a newer amalgamation and change the
version above.
