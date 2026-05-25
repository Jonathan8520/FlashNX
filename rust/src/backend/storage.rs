//! StorageBackend impl — persists Flash SharedObject data on the SD card.
//!
//! Path: `sdmc:/switch/flash-for-switch/sharedobjects/<name>.sol`
//!
//! libnx's crt0 mounts `sdmc:/` automatically, so plain `std::fs` calls work.
//! This is a direct port of Ruffle's upstream `DiskStorageBackend`
//! (`frontend-utils/src/backends/storage.rs`) — the only Switch-specific bit
//! is the default base path.
//!
//! Bonus: the `.sol` format is standard cross-platform AMF, so a user can
//! drop files from `%APPDATA%\Macromedia\Flash Player\#SharedObjects\...`
//! into the storage dir on SD and they'll just work.

use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use ruffle_core::backend::storage::StorageBackend;

pub struct SwitchStorageBackend {
    shared_objects_path: PathBuf,
}

impl SwitchStorageBackend {
    pub fn new(shared_objects_path: PathBuf) -> Self {
        if !shared_objects_path.exists() {
            if let Err(e) = fs::create_dir_all(&shared_objects_path) {
                tracing::warn!(
                    "SwitchStorageBackend: failed to create {}: {}",
                    shared_objects_path.display(),
                    e
                );
            }
        }
        Self {
            shared_objects_path,
        }
    }

    fn is_path_allowed(path: &Path) -> bool {
        path.components().all(|c| c != Component::ParentDir)
    }

    fn shared_object_path(&self, name: &str) -> PathBuf {
        self.shared_objects_path.join(format!("{name}.sol"))
    }
}

impl StorageBackend for SwitchStorageBackend {
    fn get(&self, name: &str) -> Option<Vec<u8>> {
        let path = self.shared_object_path(name);
        if !Self::is_path_allowed(&path) {
            tracing::warn!("storage.get({}) path not allowed", name);
            return None;
        }
        // We deliberately avoid `std::fs::read` and `read_to_end` here:
        // on Horizon newlib, the `read()` syscall returns ENOMEM (mapped
        // to `OutOfMemory`) when called with a 32+ KB buffer — which is
        // what Rust's `read_to_end` uses as its first growth step. Cause
        // is likely devkitPro fsdev allocating an internal buffer sized
        // to the read request and failing on larger sizes. Chunking at
        // 4 KB avoids the issue. (Note: same family of bug as the
        // `read_dir` filename truncation — see [[reference-horizon-fs-quirks]].)
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                tracing::info!(
                    "storage.get({}) MISS path={} open err={}",
                    name,
                    path.display(),
                    e
                );
                return None;
            }
        };
        let mut data = std::vec::Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => data.extend_from_slice(&buf[..n]),
                Err(e) => {
                    tracing::info!(
                        "storage.get({}) MISS path={} read err={} (after {}B)",
                        name,
                        path.display(),
                        e,
                        data.len(),
                    );
                    return None;
                }
            }
        }
        tracing::info!(
            "storage.get({}) HIT path={} {}B",
            name,
            path.display(),
            data.len()
        );
        Some(data)
    }

    fn put(&mut self, name: &str, value: &[u8]) -> bool {
        let path = self.shared_object_path(name);
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
        let path = self.shared_object_path(name);
        if !Self::is_path_allowed(&path) {
            return;
        }
        let _ = fs::remove_file(&path);
    }
}
