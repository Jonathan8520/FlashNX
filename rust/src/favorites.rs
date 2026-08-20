//! Favorited games, persisted to `sdmc:/flashnx/favorites.json`. Keyed by `.swf`
//! basename. Favorites are pinned to the top of the library (see
//! `library::sort_entries`) and flagged with a star on their gallery tile.
//!
//! JSON: a flat array of basenames, e.g. `["mario.swf", "papa.swf"]`. Vec storage
//! (NOT HashMap — `HashMap::new` crashes on Horizon without the stdlib RandomState
//! patch; playtime/covers use Vec for the same reason).

use std::sync::Mutex;

/// File name only: it lives beside the library, wherever that is (#79).
const FILE_NAME: &str = "favorites.json";

/// Read path: the games folder first, the built-in roots after, so an
/// install from before the folder could move still finds its data.
fn read_path() -> std::string::String {
    crate::library::config_read_path(FILE_NAME)
}

/// Write path: always beside the library.
fn write_path() -> std::string::String {
    crate::library::config_write_path(FILE_NAME)
}

static FAVORITES: Mutex<std::vec::Vec<std::string::String>> = Mutex::new(std::vec::Vec::new());

/// True once the table on the card is known — either read successfully, or shown
/// not to exist. False means the in-memory table is not the file's contents, and
/// `save` must not write: `fs::write` truncates, so one star on a table we merely
/// FAILED to read would replace every favourite with that single entry, commit it,
/// and look to the user like their own click erased the lot.
///
/// `save_history_meta` in library.rs already refuses to write for this reason.
/// This module and playtime.rs did not, and they are the two the user notices —
/// the stars, and every timer reading 00:00.
static LOADED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Load the persisted favorites from SD.
///
/// A missing file is a normal first boot and counts as loaded (empty really is
/// the truth). Anything else — unreadable, over the cap, not JSON, not an array —
/// leaves the table sealed rather than quietly empty.
pub fn load() {
    use core::sync::atomic::Ordering::Relaxed;
    match std::fs::metadata(read_path()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            LOADED.store(true, Relaxed);
            return;
        }
        Err(e) => {
            crate::net::log(&std::format!(
                "favorites: {} unreadable ({}) - favorites are frozen this session\n",
                read_path(), e,
            ));
            return;
        }
        Ok(_) => {}
    }
    let Some(bytes) = read_file_bounded(&read_path(), 256 * 1024) else {
        crate::net::log("favorites: read failed or over cap - favorites are frozen this session\n");
        return;
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        crate::net::log("favorites: file is not valid JSON - favorites are frozen this session\n");
        return;
    };
    let Some(arr) = json.as_array() else {
        crate::net::log("favorites: file is not a JSON array - favorites are frozen this session\n");
        return;
    };
    if let Ok(mut g) = FAVORITES.lock() {
        g.clear();
        for v in arr {
            if let Some(s) = v.as_str() {
                if !s.is_empty() && !g.iter().any(|x| x == s) {
                    g.push(s.to_string());
                }
            }
        }
        LOADED.store(true, Relaxed);
    }
}

/// True if `basename` is favorited.
pub fn is_favorite(basename: &str) -> bool {
    FAVORITES
        .lock()
        .ok()
        .is_some_and(|g| g.iter().any(|x| x == basename))
}

/// Toggle `basename`'s favorite state, persist, and return the NEW state.
pub fn toggle(basename: &str) -> bool {
    if basename.is_empty() {
        return false;
    }
    let now_fav = if let Ok(mut g) = FAVORITES.lock() {
        if let Some(pos) = g.iter().position(|x| x == basename) {
            g.remove(pos);
            false
        } else {
            g.push(basename.to_string());
            true
        }
    } else {
        return false;
    };
    save();
    now_fav
}

/// Drop `basename` from favorites (e.g. when its game is deleted). Persists only
/// if it was actually present.
pub fn remove(basename: &str) {
    let changed = if let Ok(mut g) = FAVORITES.lock() {
        if let Some(pos) = g.iter().position(|x| x == basename) {
            g.remove(pos);
            true
        } else {
            false
        }
    } else {
        false
    };
    if changed {
        save();
    }
}

fn save() {
    if !LOADED.load(core::sync::atomic::Ordering::Relaxed) {
        crate::net::log(
            "favorites: NOT saving - the table was never read, writing would replace it\n",
        );
        return;
    }
    let arr: std::vec::Vec<serde_json::Value> = match FAVORITES.lock() {
        Ok(g) => g.iter().map(|k| serde_json::Value::from(k.clone())).collect(),
        Err(_) => return,
    };
    let json = serde_json::Value::Array(arr);
    if let Ok(text) = serde_json::to_string_pretty(&json) {
        match std::fs::write(write_path(), text.as_bytes()) {
            Ok(()) => crate::sd::commit(),
            Err(e) => crate::net::log(&std::format!("favorites: save failed: {}\n", e)),
        }
    }
}

/// Bounded file read — `std::fs::read` can spuriously OOM on Horizon (see
/// `playtime::read_file_bounded`); read in fixed chunks instead.
fn read_file_bounded(path: &str, max: usize) -> Option<std::vec::Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut data: std::vec::Vec<u8> = std::vec::Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                data.extend_from_slice(&buf[..n]);
                if data.len() > max {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    Some(data)
}
