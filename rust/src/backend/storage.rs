//! StorageBackend impl — persists Flash SharedObject data on the SD card.
//!
//! Path: `sdmc:/switch/ruffle/storage/<domain>/<name>.sol`
//! libnx's crt0 already mounts `sdmc:/`, so plain `std::fs` calls work.
