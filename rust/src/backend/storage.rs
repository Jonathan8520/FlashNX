//! StorageBackend impl — persists Flash SharedObject data on the SD card.
//!
//! **Layout**: saves live FLAT alongside the `.swf` they belong to, keyed by the
//! game's filename ON THE SD CARD:
//!
//!     sdmc:/flashnx/<sd_basename>.<hash>.<sol_name>.sol
//!
//! The key used to come from Ruffle's SharedObject name, whose penultimate
//! component is the movie's own filename. For a Flashpoint game that is the entry
//! SWF's name ON ITS ORIGINAL HOST, never the name on the card — so two unrelated
//! games whose entry file happens to be called the same thing shared one save,
//! and each overwrote the other. Measured against the Flashpoint catalogue, 5.3%
//! of entries share a leaf filename and `game.swf` alone appears 279 times.
//! Deleting a game did not remove those saves either: the delete sweeps
//! `<sd_basename>*`, which never matched a file named after the original host.
//!
//! Keying on the SD basename fixes both at once, the second one for free — the
//! new name starts with `<sd_basename>.`, so the existing delete sweep matches it
//! with no change on the C++ side. `<hash>` is 8 hex of the FULL SharedObject
//! name, so a game that stores several objects, or the same object under
//! different paths (`getLocal(n)` vs `getLocal(n, "/")`), keeps them apart.
//!
//! Mirrors the `.keymap.json` / `.meta.json` sidecar convention. Easier to
//! manage manually (one folder, no nested host/movie dirs) and matches
//! what a user expects when they open the SD card.
//!
//! **Backward compatibility**: two older shapes are still READ as fallbacks — the
//! movie-keyed flat name, and the nested `<base>/saves/<host>/<basename>/<n>.sol`.
//! Writes go only to the new path, so a game migrates the first time it saves.
//! No one-shot migration walk: it could fail halfway and there is no safe way to
//! tell whose save a movie-keyed file was.
//!
//! Note what the movie-keyed fallback implies, since it is a real trade-off and
//! not an oversight: until a game has saved once under the new key, a same-leaf
//! collision can still resolve to the other game's file. That is the behaviour
//! that already shipped, it now logs a WARN when it happens, and it self-heals as
//! soon as each game writes. The alternative — ignoring the old files — would
//! silently discard every save on the card.
//!
//! libnx's crt0 mounts `sdmc:/` automatically, so plain `std::fs` calls
//! work. AMF format is cross-platform — users can drop in `.sol` files
//! from desktop Ruffle / Flash Player `%APPDATA%` and they just work.

use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use ruffle_core::backend::storage::StorageBackend;

pub struct SwitchStorageBackend {
    /// Flat root where new saves are written, e.g. `sdmc:/flashnx/`.
    flat_root: PathBuf,
    /// Legacy nested root checked on `get` only. e.g. `sdmc:/ruffle/saves/`.
    /// `None` if no legacy data exists on this device — skips the
    /// fallback check entirely. Cheap to set up: we just test `exists()`
    /// once at construction.
    legacy_root: Option<PathBuf>,
}

impl SwitchStorageBackend {
    /// `flat_root` = base dir where new saves go (typically
    /// `sdmc:/flashnx/`). `legacy_root` = old nested-tree base
    /// (typically `sdmc:/ruffle/saves/` or `sdmc:/flashnx/saves/`); pass
    /// the path even if it doesn't exist — we test and remember.
    pub fn new(flat_root: PathBuf, legacy_root: PathBuf) -> Self {
        if !flat_root.exists() {
            if let Err(e) = fs::create_dir_all(&flat_root) {
                tracing::warn!(
                    "SwitchStorageBackend: failed to create {}: {}",
                    flat_root.display(),
                    e
                );
            }
        }
        let legacy_root = if legacy_root.exists() {
            Some(legacy_root)
        } else {
            None
        };
        Self {
            flat_root,
            legacy_root,
        }
    }

    fn is_path_allowed(path: &Path) -> bool {
        path.components().all(|c| c != Component::ParentDir)
    }

    /// Split Ruffle's SharedObject `name` (e.g.
    /// "flashforswitch.local/Super_Mario_63_2010.swf/marionowe") into
    /// (basename, sol_name) by `/`. The last component is the SO name;
    /// the penultimate is the SWF basename. If there's only one
    /// component, basename is None (we don't expect this in practice,
    /// but we handle it).
    fn split_name(name: &str) -> (Option<&str>, &str) {
        let parts: std::vec::Vec<&str> = name.split('/').collect();
        match parts.len() {
            0 => (None, name),
            1 => (None, parts[0]),
            _ => (Some(parts[parts.len() - 2]), parts[parts.len() - 1]),
        }
    }

    /// 8 hex chars of `name`, so one game's several SharedObjects — and the same
    /// object reached by different paths — stay in separate files. FNV-1a: it only
    /// has to be stable and short, not cryptographic.
    fn name_hash(name: &str) -> std::string::String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in name.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        std::format!("{:08x}", (h ^ (h >> 32)) as u32)
    }

    /// Where this game's saves go: `<flat_root>/<sd_basename>.<hash>.<sol>.sol`.
    ///
    /// Keyed on the game the user launched, which the app already knows, rather
    /// than on anything derived from the movie's URL. Returns None when no game is
    /// active (the library UI, or the embedded fallback movie), in which case the
    /// caller falls back to the movie-keyed name — there is nothing better to key
    /// on, and nothing is playing that could collide.
    fn game_path(&self, name: &str) -> Option<PathBuf> {
        let basename = crate::keymap::active_game_basename()?;
        let (_, sol_name) = Self::split_name(name);
        Some(self.flat_root.join(std::format!(
            "{}.{}.{}.sol",
            basename,
            Self::name_hash(name),
            sol_name,
        )))
    }

    /// The OLD flat path, `<flat_root>/<movie_basename>.<sol_name>.sol`. Read-only
    /// now: this is the shape that let two games share one save.
    fn flat_path(&self, name: &str) -> PathBuf {
        let (basename, sol_name) = Self::split_name(name);
        let filename = match basename {
            Some(b) => std::format!("{}.{}.sol", b, sol_name),
            None => std::format!("{}.sol", sol_name),
        };
        self.flat_root.join(filename)
    }

    /// Legacy nested path:
    /// `<legacy_root>/<host>/<basename>/<sol_name>.sol` (mirrors
    /// `<legacy_root>/<full_name>.sol` since `name` already contains
    /// the slashes).
    fn legacy_path(&self, name: &str) -> Option<PathBuf> {
        self.legacy_root
            .as_ref()
            .map(|root| root.join(std::format!("{name}.sol")))
    }

    fn read_chunked(path: &Path) -> Option<std::vec::Vec<u8>> {
        // 4 KB chunked read — avoids the Horizon newlib ENOMEM @ 32+ KB
        // bug that bites `std::fs::read` / `read_to_end`'s default
        // growth step. Same workaround as in keymap.rs.
        let mut file = File::open(path).ok()?;
        let mut data = std::vec::Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => data.extend_from_slice(&buf[..n]),
                Err(_) => return None,
            }
        }
        Some(data)
    }
}

impl StorageBackend for SwitchStorageBackend {
    fn get(&self, name: &str) -> Option<Vec<u8>> {
        // The game-keyed path first: the only one that cannot belong to another game.
        if let Some(path) = self.game_path(name) {
            if Self::is_path_allowed(&path) {
                if let Some(data) = Self::read_chunked(&path) {
                    tracing::info!(
                        "storage.get({}) HIT game path={} {}B",
                        name,
                        path.display(),
                        data.len()
                    );
                    return Some(data);
                }
            }
        }
        // Then the movie-keyed shape. A hit here is not necessarily THIS game's
        // save — that is the bug being retired — so say so.
        let flat = self.flat_path(name);
        if !Self::is_path_allowed(&flat) {
            tracing::warn!("storage.get({}) path not allowed", name);
            return None;
        }
        if let Some(data) = Self::read_chunked(&flat) {
            tracing::warn!(
                "storage.get({}) HIT LEGACY movie-keyed path={} {}B — this file is \
                 keyed on the movie's own filename, so it may belong to another \
                 game with the same entry name; the next save migrates it",
                name,
                flat.display(),
                data.len()
            );
            return Some(data);
        }
        // Fall back to the legacy nested layout. Helps users who already
        // have saves from before the flat-layout refactor.
        if let Some(legacy) = self.legacy_path(name) {
            if !Self::is_path_allowed(&legacy) {
                return None;
            }
            if let Some(data) = Self::read_chunked(&legacy) {
                tracing::info!(
                    "storage.get({}) HIT legacy path={} {}B (next put will move to flat)",
                    name,
                    legacy.display(),
                    data.len()
                );
                return Some(data);
            }
        }
        tracing::info!("storage.get({}) MISS flat={}", name, flat.display());
        None
    }

    fn put(&mut self, name: &str, value: &[u8]) -> bool {
        // Always the game-keyed path when a game is active. The movie-keyed name
        // is only ever read now, never written.
        let path = self.game_path(name).unwrap_or_else(|| self.flat_path(name));
        if !Self::is_path_allowed(&path) {
            tracing::warn!("storage.put({}) path not allowed", name);
            return false;
        }
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = fs::create_dir_all(parent) {
                    tracing::warn!("storage.put: mkdir failed: {}", e);
                    return false;
                }
            }
        }
        match File::create(&path) {
            Ok(mut f) => match f.write_all(value) {
                Ok(()) => {
                    // Flush so the save survives a mode switch / abrupt exit
                    // (libnx fsdev buffers writes — see crate::sd).
                    crate::sd::commit();
                    tracing::info!(
                        "storage.put({}) OK path={} {}B",
                        name,
                        path.display(),
                        value.len()
                    );
                    true
                }
                Err(e) => {
                    tracing::warn!("storage.put({}) write failed: {}", name, e);
                    false
                }
            },
            Err(e) => {
                tracing::warn!("storage.put({}) create failed: {}", name, e);
                false
            }
        }
    }

    fn remove_key(&mut self, name: &str) {
        // Remove both the new flat path AND the legacy nested path so a
        // delete really wipes the save (otherwise the legacy read-
        // fallback would resurrect a "removed" save next session).
        let flat = self.flat_path(name);
        if Self::is_path_allowed(&flat) {
            let _ = fs::remove_file(&flat);
        }
        if let Some(legacy) = self.legacy_path(name) {
            if Self::is_path_allowed(&legacy) {
                let _ = fs::remove_file(&legacy);
            }
        }
        // Make the deletion durable too, so a removed save doesn't reappear
        // after a mode switch with the uncommitted file still on the card.
        crate::sd::commit();
    }
}
