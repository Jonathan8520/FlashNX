//! Per-game playtime + last-played time, persisted to `sdmc:/flashnx/playtime.json`.
//! Drives the "most played" (total seconds) and "last played" (recency) sorts.
//! Keyed by `.swf` basename. Vec storage (NOT HashMap — `HashMap::new` crashes
//! on Horizon without the stdlib RandomState patch; the caches use Vec too).
//!
//! JSON: `{ "<basename>": { "s": <total_secs>, "l": <last_played_epoch> } }`.
//! The legacy flat form (`{ "<basename>": <secs> }`) is still read (l = 0).

use std::sync::Mutex;

const PATH: &str = "sdmc:/flashnx/playtime.json";

/// (basename, total_seconds, last_played_epoch_secs)
static PLAYTIME: Mutex<std::vec::Vec<(std::string::String, u64, u64)>> =
    Mutex::new(std::vec::Vec::new());

/// Load the persisted map from SD. Best-effort: missing / corrupt → stays empty.
pub fn load() {
    let Some(bytes) = read_file_bounded(PATH, 1024 * 1024) else {
        return;
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    let Some(obj) = json.as_object() else {
        return;
    };
    if let Ok(mut g) = PLAYTIME.lock() {
        g.clear();
        for (k, v) in obj {
            let (secs, last) = if let Some(o) = v.as_object() {
                (
                    o.get("s").and_then(|x| x.as_u64()).unwrap_or(0),
                    o.get("l").and_then(|x| x.as_u64()).unwrap_or(0),
                )
            } else if let Some(n) = v.as_u64() {
                (n, 0) // legacy flat format
            } else {
                continue;
            };
            g.push((k.clone(), secs, last));
        }
    }
}

/// Total seconds played for `basename` (0 if never).
pub fn get(basename: &str) -> u64 {
    PLAYTIME
        .lock()
        .ok()
        .and_then(|g| g.iter().find(|(k, _, _)| k == basename).map(|(_, s, _)| *s))
        .unwrap_or(0)
}

/// Last-played epoch seconds for `basename` (0 if never played).
pub fn get_last(basename: &str) -> u64 {
    PLAYTIME
        .lock()
        .ok()
        .and_then(|g| g.iter().find(|(k, _, _)| k == basename).map(|(_, _, l)| *l))
        .unwrap_or(0)
}

/// Add `secs` to the total AND stamp last-played = now. Persists. (A 0-second
/// session still updates last-played — you did open the game.)
pub fn add(basename: &str, secs: u64) {
    if basename.is_empty() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut g) = PLAYTIME.lock() {
        if let Some(e) = g.iter_mut().find(|(k, _, _)| k == basename) {
            e.1 = e.1.saturating_add(secs);
            e.2 = now;
        } else {
            g.push((basename.to_string(), secs, now));
        }
    }
    save();
    crate::net::log(&std::format!(
        "playtime: +{}s for {} (last={})\n",
        secs, basename, now
    ));
}

fn save() {
    let mut obj = serde_json::Map::new();
    if let Ok(g) = PLAYTIME.lock() {
        for (k, s, l) in g.iter() {
            let mut e = serde_json::Map::new();
            e.insert("s".to_string(), serde_json::Value::from(*s));
            e.insert("l".to_string(), serde_json::Value::from(*l));
            obj.insert(k.clone(), serde_json::Value::Object(e));
        }
    }
    let json = serde_json::Value::Object(obj);
    if let Ok(text) = serde_json::to_string_pretty(&json) {
        if std::fs::write(PATH, text.as_bytes()).is_ok() {
            crate::sd::commit();
        }
    }
}

/// Bounded file read — `std::fs::read` can spuriously OOM on Horizon (see
/// `covers::read_file_bounded`); read in fixed chunks instead.
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
