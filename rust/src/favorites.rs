//! Favorited games, persisted to `sdmc:/flashnx/favorites.json`. Keyed by `.swf`
//! basename. Favorites are pinned to the top of the library (see
//! `library::sort_entries`) and flagged with a star on their gallery tile.
//!
//! JSON: a flat array of basenames, e.g. `["mario.swf", "papa.swf"]`. Vec storage
//! (NOT HashMap — `HashMap::new` crashes on Horizon without the stdlib RandomState
//! patch; playtime/covers use Vec for the same reason).

use std::sync::Mutex;

const PATH: &str = "sdmc:/flashnx/favorites.json";

static FAVORITES: Mutex<std::vec::Vec<std::string::String>> = Mutex::new(std::vec::Vec::new());

/// Load the persisted favorites from SD. Best-effort: missing / corrupt → empty.
pub fn load() {
    let Some(bytes) = read_file_bounded(PATH, 256 * 1024) else {
        return;
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    let Some(arr) = json.as_array() else {
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
    let arr: std::vec::Vec<serde_json::Value> = match FAVORITES.lock() {
        Ok(g) => g.iter().map(|k| serde_json::Value::from(k.clone())).collect(),
        Err(_) => return,
    };
    let json = serde_json::Value::Array(arr);
    if let Ok(text) = serde_json::to_string_pretty(&json) {
        if std::fs::write(PATH, text.as_bytes()).is_ok() {
            crate::sd::commit();
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
