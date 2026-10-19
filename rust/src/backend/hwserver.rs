//! Happy Wheels' level server, answered on the console from the game's own
//! database (#130).
//!
//! Flashpoint keeps the full Happy Wheels with every level ever published up
//! to 2019: 5.2 million of them and their replays, in an 18 GB SQLite file,
//! `happywheels.sqlite3`, next to two PHP scripts that its own web server runs
//! when the game asks `get_level.hw` or `replay.hw`. FlashNX has no PHP, so
//! this module answers those two requests itself: the same queries on the same
//! database (SQLite is built in, cpp/src/sqlite_flashnx.c), the same answers,
//! byte for byte where the game reads bytes. The scripts are in the GameZIP
//! next to the database (`content/swf.totaljerkface.com/`); what follows
//! mirrors them action by action, and says where it departs from them.
//!
//! A list (the most played, the best rated, a search, a level's replays) has
//! no index that starts with what it filters or sorts on, so it reads a whole
//! index (the ids first, then the page's rows by id): seconds on the console's
//! card, a frame frozen meanwhile. The newest and oldest come straight from the
//! index. Each slow answer is kept in the game's tree (`.flashnx-cache/`), so a
//! list costs that once; the database never changes.

use core::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

extern "C" {
    fn sqlite3_open_v2(name: *const c_char, db: *mut *mut c_void, flags: c_int, vfs: *const c_char) -> c_int;
    fn sqlite3_close(db: *mut c_void) -> c_int;
    fn sqlite3_errmsg(db: *mut c_void) -> *const c_char;
    fn sqlite3_prepare_v2(
        db: *mut c_void,
        sql: *const c_char,
        n: c_int,
        stmt: *mut *mut c_void,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut c_void, idx: c_int, v: i64) -> c_int;
    // The destructor is SQLITE_TRANSIENT (-1): SQLite copies the text.
    fn sqlite3_bind_text(stmt: *mut c_void, idx: c_int, text: *const c_char, n: c_int, destructor: isize) -> c_int;
    fn sqlite3_step(stmt: *mut c_void) -> c_int;
    fn sqlite3_column_count(stmt: *mut c_void) -> c_int;
    fn sqlite3_column_type(stmt: *mut c_void, col: c_int) -> c_int;
    fn sqlite3_column_blob(stmt: *mut c_void, col: c_int) -> *const c_void;
    fn sqlite3_column_text(stmt: *mut c_void, col: c_int) -> *const u8;
    fn sqlite3_column_bytes(stmt: *mut c_void, col: c_int) -> c_int;
    fn sqlite3_finalize(stmt: *mut c_void) -> c_int;
    fn sqlite3_exec(
        db: *mut c_void,
        sql: *const c_char,
        cb: *const c_void,
        arg: *mut c_void,
        err: *mut *mut c_char,
    ) -> c_int;

    fn ruffle_tick_now() -> u64;
    fn ruffle_tick_freq() -> u64;
    fn swf_picker_file_size(path: *const c_char) -> i64;
    fn swf_picker_write_file(path: *const c_char, data: *const u8, len: u32) -> c_int;
}

const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_NULL: c_int = 5;
const SQLITE_BLOB: c_int = 4;
const SQLITE_OPEN_READONLY: c_int = 0x0000_0001;
const SQLITE_TRANSIENT: isize = -1;

/// The database file the scripts open (`new PDO('sqlite:happywheels.sqlite3')`),
/// beside them.
const DB_NAME: &str = "happywheels.sqlite3";

/// Levels per page of a list (`LIMIT 500`), replays per page (`LIMIT 400`).
const LEVELS_PER_PAGE: i64 = 500;
const REPLAYS_PER_PAGE: i64 = 400;

/// The open database, kept between requests: opening reads its schema.
struct Db {
    path: PathBuf,
    handle: *mut c_void,
}

// Only ever used from the game thread, behind the mutex.
unsafe impl Send for Db {}

static DB: Mutex<Option<Db>> = Mutex::new(None);

/// Close the database (the game ends).
pub fn close() {
    if let Ok(mut g) = DB.lock() {
        if let Some(db) = g.take() {
            unsafe { sqlite3_close(db.handle) };
        }
    }
}

fn file_exists(path: &Path) -> bool {
    let Some(c) = path.to_str().and_then(|s| std::ffi::CString::new(s).ok()) else {
        return false;
    };
    unsafe { swf_picker_file_size(c.as_ptr()) >= 0 }
}

fn ms_since(t0: u64) -> u64 {
    let freq = unsafe { ruffle_tick_freq() }.max(1);
    unsafe { ruffle_tick_now() }.saturating_sub(t0) * 1000 / freq
}

/// A value bound to a `?` of a query.
enum Param<'a> {
    Int(i64),
    Text(&'a str),
}

/// One column of a row: its bytes (text or blob), `None` for NULL.
type Row = Vec<Option<Vec<u8>>>;

/// Every row of `sql` with `params`, or `None` when SQLite refused it.
fn query(db: *mut c_void, sql: &str, params: &[Param]) -> Option<Vec<Row>> {
    let sql_c = std::ffi::CString::new(sql).ok()?;
    let mut stmt: *mut c_void = core::ptr::null_mut();
    let rc = unsafe { sqlite3_prepare_v2(db, sql_c.as_ptr(), -1, &mut stmt, core::ptr::null_mut()) };
    if rc != 0 || stmt.is_null() {
        let msg = unsafe { std::ffi::CStr::from_ptr(sqlite3_errmsg(db)) }.to_string_lossy();
        tracing::warn!("hwserver: SQL refused ({rc}: {msg}): {sql}");
        return None;
    }
    let mut texts: Vec<std::ffi::CString> = Vec::new();
    for (i, p) in params.iter().enumerate() {
        let idx = i as c_int + 1;
        match p {
            Param::Int(v) => unsafe {
                sqlite3_bind_int64(stmt, idx, *v);
            },
            Param::Text(t) => {
                let c = std::ffi::CString::new(t.replace('\0', "")).unwrap_or_default();
                unsafe { sqlite3_bind_text(stmt, idx, c.as_ptr(), -1, SQLITE_TRANSIENT) };
                texts.push(c);
            }
        }
    }
    let mut rows = Vec::new();
    loop {
        match unsafe { sqlite3_step(stmt) } {
            SQLITE_ROW => {
                let n = unsafe { sqlite3_column_count(stmt) };
                let mut row = Vec::with_capacity(n as usize);
                for col in 0..n {
                    let kind = unsafe { sqlite3_column_type(stmt, col) };
                    if kind == SQLITE_NULL {
                        row.push(None);
                        continue;
                    }
                    let ptr = if kind == SQLITE_BLOB {
                        unsafe { sqlite3_column_blob(stmt, col) as *const u8 }
                    } else {
                        unsafe { sqlite3_column_text(stmt, col) }
                    };
                    let len = unsafe { sqlite3_column_bytes(stmt, col) }.max(0) as usize;
                    let bytes = if ptr.is_null() || len == 0 {
                        Vec::new()
                    } else {
                        unsafe { core::slice::from_raw_parts(ptr, len) }.to_vec()
                    };
                    row.push(Some(bytes));
                }
                rows.push(row);
            }
            SQLITE_DONE => break,
            rc => {
                let msg = unsafe { std::ffi::CStr::from_ptr(sqlite3_errmsg(db)) }.to_string_lossy();
                tracing::warn!("hwserver: SQL failed ({rc}: {msg}): {sql}");
                unsafe { sqlite3_finalize(stmt) };
                return None;
            }
        }
    }
    unsafe { sqlite3_finalize(stmt) };
    drop(texts);
    Some(rows)
}

/// Run `f` on the database at `path`, opened on first use.
fn with_db<R>(path: &Path, f: impl FnOnce(*mut c_void) -> R) -> Option<R> {
    let mut g = DB.lock().ok()?;
    if g.as_ref().is_some_and(|db| db.path != path) {
        if let Some(db) = g.take() {
            unsafe { sqlite3_close(db.handle) };
        }
    }
    if g.is_none() {
        let name = std::ffi::CString::new(path.to_str()?).ok()?;
        let mut handle: *mut c_void = core::ptr::null_mut();
        let t0 = unsafe { ruffle_tick_now() };
        let rc = unsafe { sqlite3_open_v2(name.as_ptr(), &mut handle, SQLITE_OPEN_READONLY, core::ptr::null()) };
        if rc != 0 {
            tracing::warn!("hwserver: cannot open {} ({rc})", path.display());
            if !handle.is_null() {
                unsafe { sqlite3_close(handle) };
            }
            return None;
        }
        // 32 MB of pages kept: a list's scan reads the whole levels table,
        // and the second look at a level must not go back to the card.
        let pragma = c"PRAGMA cache_size = -32768";
        unsafe {
            sqlite3_exec(handle, pragma.as_ptr(), core::ptr::null(), core::ptr::null_mut(), core::ptr::null_mut())
        };
        tracing::info!("hwserver: opened {} in {} ms", path.display(), ms_since(t0));
        *g = Some(Db { path: path.to_path_buf(), handle });
    }
    let handle = g.as_ref()?.handle;
    Some(f(handle))
}

/// The POST form the game sends (`application/x-www-form-urlencoded`), as PHP
/// reads it into `$_POST`: `+` is a space, `%XX` a byte.
fn form(body: &[u8]) -> Vec<(String, String)> {
    fn decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < b.len() => {
                    let hex = |c: u8| (c as char).to_digit(16);
                    if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                        continue;
                    }
                    out.push(b'%');
                }
                c => out.push(c),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    String::from_utf8_lossy(body)
        .split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (!k.is_empty()).then(|| (decode(k), decode(v)))
        })
        .collect()
}

fn field<'a>(form: &'a [(String, String)], key: &str) -> &'a str {
    form.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).unwrap_or("")
}

/// PHP's `(int)` of a string: its leading digits, 0 when none.
fn php_int(s: &str) -> i64 {
    let t = s.trim();
    let (neg, digits) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let n: i64 = digits
        .bytes()
        .take_while(|c| c.is_ascii_digit())
        .fold(0i64, |acc, c| acc.saturating_mul(10).saturating_add((c - b'0') as i64));
    if neg { -n } else { n }
}

/// XML text or attribute value, escaped as libxml writes it out.
fn esc(out: &mut Vec<u8>, bytes: &[u8], attr: bool) {
    for &c in bytes {
        match c {
            b'&' => out.extend_from_slice(b"&amp;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'"' if attr => out.extend_from_slice(b"&quot;"),
            b'\n' if attr => out.extend_from_slice(b"&#10;"),
            b'\r' => out.extend_from_slice(b"&#13;"),
            b'\t' if attr => out.extend_from_slice(b"&#9;"),
            // Not allowed in XML 1.0 at all; libxml drops them too.
            0..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F => {}
            _ => out.push(c),
        }
    }
}

/// `<name a="..." ...><uc>...</uc></name>`: one level or replay as the scripts
/// build it (`addChild('uc', htmlspecialchars(...))`, then the attributes in
/// that order). `row` holds the attributes in `names` order, then `uc`.
fn element(out: &mut Vec<u8>, name: &str, names: &[&str], row: &Row) {
    out.push(b'<');
    out.extend_from_slice(name.as_bytes());
    for (i, attr) in names.iter().enumerate() {
        out.push(b' ');
        out.extend_from_slice(attr.as_bytes());
        out.extend_from_slice(b"=\"");
        if let Some(Some(v)) = row.get(i) {
            esc(out, v, true);
        }
        out.push(b'"');
    }
    out.extend_from_slice(b"><uc>");
    if let Some(Some(v)) = row.get(names.len()) {
        esc(out, v, false);
    }
    out.extend_from_slice(b"</uc></");
    out.extend_from_slice(name.as_bytes());
    out.push(b'>');
}

const LEVEL_ATTRS: [&str; 9] = ["id", "ln", "pc", "ps", "rg", "ui", "un", "vs", "dp"];
const LEVEL_COLS: &str = "id, ln, pc, ps, rg, ui, un, vs, dp, uc";
const REPLAY_ATTRS: [&str; 12] = ["ar", "ct", "dc", "id", "li", "pc", "rg", "ui", "un", "vr", "vs", "vw"];
const REPLAY_COLS: &str = "ar, ct, dc, id, li, pc, rg, ui, un, vr, vs, vw, uc";

/// What `SimpleXMLElement::asXML()` writes for a document whose root is `root`
/// (with its attributes) and children `inner`.
fn document(root: &str, root_attrs: &str, inner: &[u8]) -> Vec<u8> {
    let mut out = b"<?xml version=\"1.0\"?>\n<".to_vec();
    out.extend_from_slice(root.as_bytes());
    out.extend_from_slice(root_attrs.as_bytes());
    if inner.is_empty() {
        out.extend_from_slice(b"/>\n");
    } else {
        out.push(b'>');
        out.extend_from_slice(inner);
        out.extend_from_slice(b"</");
        out.extend_from_slice(root.as_bytes());
        out.extend_from_slice(b">\n");
    }
    out
}

fn levels_document(rows: &[Row]) -> Vec<u8> {
    let mut inner = Vec::new();
    for row in rows {
        element(&mut inner, "lv", &LEVEL_ATTRS, row);
    }
    document("lvs", "", &inner)
}

/// `getSQLWhere`: the upload date filter, and its length in days. The levels
/// end in 2019, so anything but `anytime` finds nothing today, as on
/// Flashpoint (its notes suggest changing the system clock).
fn upload_filter(uploaded: &str) -> (&'static str, f64) {
    match uploaded {
        "month" => ("julianday() - julianday(dp) < 30", 30.0),
        "week" => ("julianday() - julianday(dp) < 7", 7.0),
        "today" => ("julianday() - julianday(dp) < 1", 1.0),
        _ => ("", 0.0),
    }
}

/// `getSQLOrder`, on the columns of `levels_index`. The script sorts on
/// `date(dp)`, which no index can serve; `dp` is the same date written
/// `YYYY-MM-DD`, so the same order, read straight from the index. An unknown
/// order made the script fail (`ORDER BY ` and nothing); here it is the most
/// played.
fn level_order(sortby: &str) -> &'static str {
    match sortby {
        "oldest" => "dp ASC",
        "newest" => "dp DESC",
        "rating" => "rg DESC",
        _ => "ps DESC",
    }
}

/// `buildQuery` + the page, for the ids only: `SELECT id FROM levels WHERE a
/// AND b ORDER BY o LIMIT 500 OFFSET n`, the empty conditions left out.
///
/// Read from `levels_index` (dp, ln, ps, rg, ui, un, and the id with every
/// entry): it holds every column a list filters or sorts on, so SQLite walks
/// the index alone, without the levels' descriptions, a few times less to
/// read off the card than the table. The script's `SELECT *` over the table
/// took the whole table each time.
fn level_ids_sql(conditions: &[&str], order: &str, page: i64) -> String {
    let conds: Vec<&str> = conditions.iter().copied().filter(|c| !c.is_empty()).collect();
    let mut sql = std::string::String::from("SELECT id FROM levels INDEXED BY levels_index");
    if !conds.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conds.join(" AND "));
    }
    let offset = LEVELS_PER_PAGE * (page.max(1) - 1);
    sql.push_str(&std::format!(" ORDER BY {order} LIMIT {LEVELS_PER_PAGE} OFFSET {offset}"));
    sql
}

/// The rows of `table` whose ids are `ids` (first column of each), with the
/// columns `cols`, the id being column `id_col` of those, in the order of `ids`.
fn rows_by_id(db: *mut c_void, table: &str, cols: &str, id_col: usize, ids: &[Row]) -> Option<Vec<Row>> {
    let id_of = |r: &Row, col: usize| {
        r.get(col).cloned().flatten().map(|v| php_int(&String::from_utf8_lossy(&v))).unwrap_or(0)
    };
    let ids: Vec<i64> = ids.iter().map(|r| id_of(r, 0)).collect();
    if ids.is_empty() {
        return Some(Vec::new());
    }
    let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
    let sql = std::format!("SELECT {cols} FROM {table} WHERE id IN ({})", list.join(","));
    let mut rows = query(db, &sql, &[])?;
    rows.sort_by_key(|r| {
        let id = id_of(r, id_col);
        ids.iter().position(|&i| i == id).unwrap_or(usize::MAX)
    });
    Some(rows)
}

/// The levels `ids`, whole, in that order.
fn levels_by_id(db: *mut c_void, ids: &[Row]) -> Option<Vec<Row>> {
    rows_by_id(db, "levels", LEVEL_COLS, 0, ids)
}

/// Whether any level is recent enough for an upload filter of `days`: the
/// newest date comes from the index at once, where the filter itself reads
/// every level to find none.
fn any_level_within(db: *mut c_void, days: f64) -> bool {
    let rows = query(db, "SELECT julianday() - julianday(max(dp)) FROM levels", &[]).unwrap_or_default();
    let age = rows
        .first()
        .and_then(|r| r.first().cloned().flatten())
        .and_then(|v| String::from_utf8_lossy(&v).trim().parse::<f64>().ok());
    age.is_some_and(|a| a < days)
}

/// The bytes of the first column of the first row, empty when there is none
/// (PHP echoes `$result[0]['data']` of nothing: an empty answer).
fn first_blob(rows: Option<Vec<Row>>) -> Vec<u8> {
    rows.and_then(|r| r.into_iter().next())
        .and_then(|row| row.into_iter().next())
        .flatten()
        .unwrap_or_default()
}

/// `get_level.hw`.
fn get_level(db: *mut c_void, dir: &Path, form: &[(String, String)]) -> Option<Vec<u8>> {
    let action = field(form, "action");
    match action {
        // The featured list is a file the script echoes.
        "get_featured" => super::navigator::read_sidecar_file(&dir.join("feat.xml")),
        "get_record" => {
            let id = php_int(field(form, "level_id"));
            Some(first_blob(query(db, "SELECT data FROM levels_data WHERE id = ?", &[Param::Int(id)])))
        }
        "get_level" => {
            let id = php_int(field(form, "level_id"));
            let rows = query(db, &std::format!("SELECT {LEVEL_COLS} FROM levels WHERE id = ?"), &[Param::Int(id)])?;
            if rows.is_empty() {
                // Echoed as written in the script, not through SimpleXML.
                Some(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><lvs/>".to_vec())
            } else {
                Some(levels_document(&rows))
            }
        }
        "get_all" | "get_pub_by_user" | "search_by_name" | "search_by_user" => {
            let page = php_int(field(form, "page"));
            let order = level_order(field(form, "sortby"));
            let (when, days) = upload_filter(field(form, "uploaded"));
            if !when.is_empty() && !any_level_within(db, days) {
                return Some(levels_document(&[]));
            }
            let sterm = std::format!("%{}%", field(form, "sterm"));
            let user = php_int(field(form, "user_id"));
            let (cond, param) = match action {
                "get_pub_by_user" => ("ui = ?", Some(Param::Int(user))),
                "search_by_name" => ("ln like ?", Some(Param::Text(&sterm))),
                "search_by_user" => ("un like ?", Some(Param::Text(&sterm))),
                _ => ("", None),
            };
            let sql = level_ids_sql(&[cond, when], order, page);
            let params: Vec<Param> = param.into_iter().collect();
            let ids = query(db, &sql, &params)?;
            Some(levels_document(&levels_by_id(db, &ids)?))
        }
        _ => None,
    }
}

/// `replay.hw`.
fn replay(db: *mut c_void, form: &[(String, String)]) -> Option<Vec<u8>> {
    match field(form, "action") {
        "get_combined" => {
            let id = php_int(field(form, "replay_id"));
            let rows = query(db, &std::format!("SELECT {REPLAY_COLS} FROM replays WHERE id = ?"), &[Param::Int(id)])?;
            if rows.is_empty() {
                return Some(b"<combined_data />".to_vec());
            }
            let mut inner = Vec::new();
            for row in &rows {
                element(&mut inner, "rp", &REPLAY_ATTRS, row);
            }
            // The level of the first replay (`$result[0]['li']`, column 4).
            let li = rows[0]
                .get(4)
                .and_then(|v| v.as_ref())
                .map(|v| php_int(&String::from_utf8_lossy(v)))
                .unwrap_or(0);
            let levels = query(db, &std::format!("SELECT {LEVEL_COLS} FROM levels WHERE id = ?"), &[Param::Int(li)])?;
            for row in &levels {
                element(&mut inner, "lv", &LEVEL_ATTRS, row);
            }
            Some(document("combined_data", "", &inner))
        }
        "get_all_by_level" => {
            let page_raw = field(form, "page");
            let page = php_int(page_raw);
            // On the columns of `replays_index` (ct, dc, li, rg), `dc` for the
            // script's `date(dc)` as `dp` above. An unknown order made the
            // script fail; here, the best rated.
            let order = match field(form, "sortby") {
                "newest" => "dc DESC",
                "oldest" => "dc ASC",
                "completion_time" => "ct ASC",
                _ => "rg DESC",
            };
            let offset = REPLAYS_PER_PAGE * (page - 1);
            let li = php_int(field(form, "level_id"));
            // No index starts with the level, so the ids come from the whole
            // of `replays_index`, which holds li and every order (and the id):
            // smaller to read than the replays themselves. Then the page's
            // replays by id, in that order.
            let sql = std::format!(
                "SELECT id FROM replays INDEXED BY replays_index WHERE li = ? ORDER BY {order} LIMIT {REPLAYS_PER_PAGE} OFFSET ?"
            );
            let ids = query(db, &sql, &[Param::Int(li), Param::Int(offset)])?;
            let rows = rows_by_id(db, "replays", REPLAY_COLS, 3, &ids)?;
            let mut inner = Vec::new();
            for row in &rows {
                element(&mut inner, "rp", &REPLAY_ATTRS, row);
            }
            let mut attrs = std::format!(" pg=\"");
            let mut escaped = Vec::new();
            esc(&mut escaped, page_raw.as_bytes(), true);
            attrs.push_str(&String::from_utf8_lossy(&escaped));
            attrs.push_str(&std::format!("\" pp=\"{REPLAYS_PER_PAGE}\""));
            Some(document("rps", &attrs, &inner))
        }
        "get_cmb_records" => {
            let replay_id = php_int(field(form, "replay_id"));
            let level_id = php_int(field(form, "level_id"));
            let replay = first_blob(query(db, "SELECT data FROM replays_data WHERE id = ?", &[Param::Int(replay_id)]));
            let level = first_blob(query(db, "SELECT data FROM levels_data WHERE id = ?", &[Param::Int(level_id)]));
            // `strrev(pack("L", $length))`: the length, big-endian, first.
            let mut out = (replay.len() as u32).to_be_bytes().to_vec();
            out.extend_from_slice(&replay);
            out.extend_from_slice(&level);
            Some(out)
        }
        _ => None,
    }
}

/// Whether an action's answer is a list worth keeping: one that scans a table.
fn cacheable(script: &str, action: &str) -> bool {
    matches!(
        (script, action),
        ("get_level.hw", "get_all" | "get_pub_by_user" | "search_by_name" | "search_by_user")
            | ("replay.hw", "get_all_by_level")
    )
}

/// Where a list's answer is kept: `<script dir>/.flashnx-cache/<hash>`, hashed
/// from the script and the fields the queries read (FNV-1a, 64 bits). Only
/// those: a field the scripts ignore, such as a counter against a browser's
/// cache, would make every request a new one.
fn cache_path(dir: &Path, script: &str, form: &[(String, String)]) -> PathBuf {
    const READ: [&str; 7] = ["action", "page", "sortby", "uploaded", "sterm", "user_id", "level_id"];
    let pairs: Vec<(&str, &str)> = READ.iter().map(|k| (*k, field(form, k))).collect();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: &[u8]| {
        for &c in b {
            h ^= c as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    eat(script.as_bytes());
    for (k, v) in &pairs {
        eat(k.as_bytes());
        eat(v.as_bytes());
    }
    dir.join(".flashnx-cache").join(std::format!("{h:016x}.xml"))
}

/// The answer to a request for `script_path` (the `.hw` script's place in the
/// game's tree) with the POST `body`, when the database the script reads is
/// there; `None` otherwise, and the request goes its usual way.
pub fn answer(script_path: &Path, body: Option<&[u8]>) -> Option<Vec<u8>> {
    let script = script_path.file_name()?.to_str()?;
    if script != "get_level.hw" && script != "replay.hw" {
        return None;
    }
    let dir = script_path.parent()?;
    let db_path = dir.join(DB_NAME);
    if !file_exists(&db_path) {
        return None;
    }
    let form = form(body.unwrap_or_default());
    let action = field(&form, "action").to_string();
    let cached = cacheable(script, &action).then(|| cache_path(dir, script, &form));
    if let Some(path) = &cached {
        if let Some(bytes) = super::navigator::read_sidecar_file(path) {
            tracing::info!("hwserver: {script} {action} from the kept answer ({} bytes)", bytes.len());
            return Some(bytes);
        }
    }
    let t0 = unsafe { ruffle_tick_now() };
    let out = with_db(&db_path, |db| match script {
        "get_level.hw" => get_level(db, dir, &form),
        _ => replay(db, &form),
    })
    .flatten();
    let ms = ms_since(t0);
    match &out {
        Some(bytes) => {
            tracing::info!("hwserver: {script} {action} {:?} -> {} bytes in {ms} ms", form, bytes.len());
            if let (Some(path), true) = (&cached, ms >= 500) {
                if let Some(c) = path.to_str().and_then(|s| std::ffi::CString::new(s).ok()) {
                    unsafe { swf_picker_write_file(c.as_ptr(), bytes.as_ptr(), bytes.len() as u32) };
                    crate::sd::commit();
                }
            }
        }
        None => tracing::warn!("hwserver: {script} {action} {:?} unanswered after {ms} ms", form),
    }
    out
}
