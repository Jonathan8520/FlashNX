//! FlashNX updating itself, without waiting for the Homebrew App Store.
//!
//! Once per launch, as soon as the library is on screen and only when the
//! console has a network, GitHub is asked for the latest release. A newer one
//! is offered once per version, in a window over the Play grid; after that it
//! stays a dot on the Settings tab and a row in À propos. Nothing is
//! downloaded or replaced before the player says so with A.
//!
//! Installing downloads `FlashNX.nro` beside the running one, checks it against
//! the SHA-256 GitHub gives for it, and only then puts it in place, keeping the
//! old file aside until the new one has its name (cpp/src/updater.cpp). The
//! update is used from the next start, which the player can ask for at once.

use core::ffi::{c_char, c_int};

/// GitHub's answer for the newest release that is neither a draft nor a
/// pre-release.
const LATEST_URL: &str = "https://api.github.com/repos/Jonathan8520/FlashNX/releases/latest";
/// The release file that is FlashNX itself.
const ASSET: &str = "FlashNX.nro";
/// The last version a window was shown for, so it is offered once.
const ANNOUNCED_PATH: &str = "sdmc:/switch/FlashNX/update_seen.txt";
/// Debug marker: when present, the running version reads as 0.0.0, so the
/// current release is offered and can be installed over a test build.
const TEST_MARKER: &str = "sdmc:/switch/FlashNX/update_test.on";
/// Kept free on top of the new file (net.cpp `SPACE_MARGIN`).
const SPACE_MARGIN: u64 = 8 * 1024 * 1024;
/// A check that got no answer is asked again this much later, once.
const RETRY_SECS: u64 = 60;

extern "C" {
    fn flashnx_update_check_start(url: *const c_char) -> c_int;
    fn flashnx_update_check_poll(out: *mut c_char, cap: c_int) -> c_int;
    fn flashnx_update_check_busy() -> c_int;
    fn flashnx_sha256_file(path: *const c_char, out: *mut u8) -> c_int;
    fn flashnx_replace_file(fresh: *const c_char, target: *const c_char) -> c_int;
    fn flashnx_self_nro(out: *mut c_char, cap: c_int) -> c_int;
    fn flashnx_restart_supported() -> c_int;
    fn flashnx_request_restart() -> c_int;
    fn swf_picker_write_file(path: *const c_char, data: *const u8, len: u32) -> c_int;
    fn ruffle_is_applet_mode() -> c_int;
    fn ruffle_tick_now() -> u64;
    fn ruffle_tick_freq() -> u64;
}

/// A release newer than the one running.
#[derive(Clone)]
pub(crate) struct Release {
    /// "2.1.0", without the tag's "v".
    pub version: std::string::String,
    url: std::string::String,
    pub size: u64,
    sha256: [u8; 32],
}

/// Where the check stands this session.
#[derive(Clone)]
pub(crate) enum Check {
    NotYet,
    Running,
    Available(Release),
    UpToDate,
    /// No answer (no network, GitHub refused or down).
    Failed,
}

/// Where an install stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Install {
    Idle,
    Downloading,
    /// Downloaded: checked and put in place on the next frame, so the page says
    /// so for one frame first.
    Verify,
    Done,
}

/// What `tick_install` says to the page.
pub(crate) enum Progress {
    /// Still going (the page reads the bytes from `net::download_progress`).
    Pending,
    /// In place: used from the next start.
    Done,
    Failed(std::string::String),
}

struct State {
    check: Check,
    /// Ticks after which the first check, or the one retry, may start. 0 until
    /// the library first asks.
    next_try: u64,
    retried: bool,
    install: Install,
    /// The release being installed.
    target: Option<Release>,
    /// Whether the window was decided for this session (offered, or already
    /// offered at an earlier launch): read from the card once, not per frame.
    offer_decided: bool,
}

static STATE: std::sync::Mutex<State> = std::sync::Mutex::new(State {
    check: Check::NotYet,
    next_try: 0,
    retried: false,
    install: Install::Idle,
    target: None,
    offer_decided: false,
});

fn log(s: &str) {
    crate::net::log(s);
}

fn secs(n: u64) -> u64 {
    unsafe { ruffle_tick_freq() }.saturating_mul(n)
}

/// FlashNX's own `.nro`, `sdmc:/...`; None when it cannot be updated in place
/// (not started from a file on the card, or a self-contained build).
fn self_nro() -> Option<std::string::String> {
    let mut buf = [0u8; 512];
    let n = unsafe { flashnx_self_nro(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int) };
    if n <= 0 {
        return None;
    }
    core::str::from_utf8(&buf[..n as usize]).ok().map(str::to_string)
}

fn marker(path: &str) -> bool {
    crate::sources::gamezip::read_file_bounded(path, 4).is_some()
}

/// (major, minor, patch) of "2.1.0" or "v2.1.0"; a suffix after a number
/// ("2.1.0-beta") is dropped.
fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let mut it = s.split('.').map(|p| {
        let digits: std::string::String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse::<u32>().ok()
    });
    let major = it.next()??;
    let minor = it.next().flatten().unwrap_or(0);
    let patch = it.next().flatten().unwrap_or(0);
    Some((major, minor, patch))
}

fn running_version() -> (u32, u32, u32) {
    if marker(TEST_MARKER) {
        log("update: update_test.on present -> the running version reads as 0.0.0\n");
        return (0, 0, 0);
    }
    parse_version(crate::bugreport::APP_VERSION).unwrap_or((0, 0, 0))
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let nib = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (nib(b[2 * i])? << 4) | nib(b[2 * i + 1])?;
    }
    Some(out)
}

/// The release GitHub answered with, when it is newer than the one running
/// and carries a FlashNX.nro with its SHA-256.
fn parse_release(buf: &[u8]) -> Result<Option<Release>, ()> {
    let v: serde_json::Value = serde_json::from_slice(buf).map_err(|_| ())?;
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return Ok(None);
    }
    let tag = v["tag_name"].as_str().ok_or(())?;
    let latest = parse_version(tag).ok_or(())?;
    if latest <= running_version() {
        log(&std::format!("update: {} is the latest, nothing newer\n", tag));
        return Ok(None);
    }
    let asset = v["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"].as_str() == Some(ASSET)))
        .ok_or(())?;
    let url = asset["browser_download_url"].as_str().ok_or(())?;
    let size = asset["size"].as_u64().ok_or(())?;
    // Without its digest the file could not be checked, so it is not offered.
    let sha256 = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).and_then(hex32).ok_or(())?;
    let version = std::format!("{}.{}.{}", latest.0, latest.1, latest.2);
    log(&std::format!("update: {} available ({} bytes)\n", version, size));
    Ok(Some(Release { version, url: url.to_string(), size, sha256 }))
}

fn start_check(st: &mut State) {
    let url = std::format!("{}\0", LATEST_URL);
    if unsafe { flashnx_update_check_start(url.as_ptr() as *const c_char) } == 0 {
        log("update: asking GitHub for the latest release\n");
        st.check = Check::Running;
    } else {
        st.check = Check::Failed;
    }
}

/// Once per library frame: starts the check when it is time, and reads its
/// answer. Cheap when there is nothing to do.
pub(crate) fn drive() {
    let Ok(mut st) = STATE.lock() else { return };
    let now = unsafe { ruffle_tick_now() };
    match st.check {
        Check::NotYet | Check::Failed => {
            if st.next_try == 0 {
                // The applet's small pool, and a build that cannot be replaced,
                // never ask.
                let Some(target) = self_nro().filter(|_| unsafe { ruffle_is_applet_mode() } == 0) else {
                    st.check = Check::UpToDate;
                    return;
                };
                // What an update cut short (switched off mid-download, or
                // between the two renames) left beside FlashNX.
                let _ = std::fs::remove_file(part_path(&target));
                let _ = std::fs::remove_file(std::format!("{}.old", target));
                // Asked on the next frame: the request has a thread of its own
                // and the library is already on screen (a 3 s wait, meant to
                // let the shelves have the network first, only made the window
                // come late; Jonathan, 2026-10-18).
                st.next_try = now.max(1);
                return;
            }
            if now < st.next_try || crate::net::offline() {
                return;
            }
            if matches!(st.check, Check::Failed) {
                if st.retried {
                    return;
                }
                st.retried = true;
            }
            start_check(&mut st);
        }
        Check::Running => {
            if unsafe { flashnx_update_check_busy() } == 1 {
                return;
            }
            let mut buf = std::vec![0u8; 256 * 1024];
            let n = unsafe { flashnx_update_check_poll(buf.as_mut_ptr() as *mut c_char, buf.len() as c_int) };
            if n == 0 {
                return;
            }
            if n < 0 {
                st.check = Check::Failed;
                st.next_try = now + secs(RETRY_SECS);
                return;
            }
            buf.truncate(n as usize);
            st.check = match parse_release(&buf) {
                Ok(Some(r)) => Check::Available(r),
                Ok(None) => Check::UpToDate,
                Err(()) => {
                    log("update: GitHub's answer did not read as a release\n");
                    Check::Failed
                }
            };
            if matches!(st.check, Check::Failed) {
                st.next_try = now + secs(RETRY_SECS);
            }
        }
        Check::Available(_) | Check::UpToDate => {}
    }
}

/// À propos asked: check now rather than wait (the row's A when nothing is
/// known to be newer). False when it cannot be asked (no network, or a build
/// that is not updated in place).
pub(crate) fn check_now() -> bool {
    let Ok(mut st) = STATE.lock() else { return false };
    if matches!(st.check, Check::Running) {
        return true;
    }
    if self_nro().is_none() || crate::net::offline() {
        return false;
    }
    st.retried = true;
    start_check(&mut st);
    true
}

pub(crate) fn status() -> Check {
    STATE.lock().map(|s| s.check.clone()).unwrap_or(Check::NotYet)
}

/// A newer version is known: the dot on the Settings tab and on À propos.
pub(crate) fn badge() -> bool {
    STATE.lock().map(|s| matches!(s.check, Check::Available(_)) && s.install != Install::Done).unwrap_or(false)
}

pub(crate) fn available() -> Option<Release> {
    STATE.lock().ok().and_then(|s| match &s.check {
        Check::Available(r) => Some(r.clone()),
        _ => None,
    })
}

/// The release to offer in a window, once per version: Some the first time a
/// version is seen (and remembered on the card at once, so "later" holds even
/// if the console is switched off), None after.
pub(crate) fn take_offer() -> Option<Release> {
    let r = available()?;
    {
        let Ok(mut st) = STATE.lock() else { return None };
        if st.offer_decided || st.install != Install::Idle {
            return None;
        }
        st.offer_decided = true;
    }
    let seen = crate::sources::gamezip::read_file_bounded(ANNOUNCED_PATH, 64)
        .map(|b| std::string::String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();
    if seen == r.version {
        return None;
    }
    let path = std::format!("{}\0", ANNOUNCED_PATH);
    unsafe { swf_picker_write_file(path.as_ptr() as *const c_char, r.version.as_ptr(), r.version.len() as u32) };
    crate::sd::commit();
    Some(r)
}

fn part_path(target: &str) -> std::string::String {
    std::format!("{}.part", target)
}

/// The first `n` bytes of a file, fewer when it is shorter or unreadable.
fn file_head(path: &str, n: usize) -> std::vec::Vec<u8> {
    use std::io::Read;
    let mut buf = std::vec![0u8; n];
    let got = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    buf.truncate(got);
    buf
}

/// A on Installer: start downloading the new version beside the running one.
pub(crate) fn start_install() -> Result<(), std::string::String> {
    let lc = crate::loc::s();
    let Some(r) = available() else { return Err(lc.upd_err_none.to_string()) };
    let Some(target) = self_nro() else { return Err(lc.upd_err_none.to_string()) };
    // Known before anything is fetched: say it now, not after a download.
    if let Some(free) = crate::net::sd_free_bytes() {
        let need = r.size + SPACE_MARGIN;
        if need > free {
            return Err(crate::loc::err_sd_space(r.size, need, free));
        }
    }
    let part = part_path(&target);
    crate::net::start_download(&r.url, &part)?;
    log(&std::format!("update: downloading {} -> {}\n", r.version, part));
    if let Ok(mut st) = STATE.lock() {
        st.install = Install::Downloading;
        st.target = Some(r);
    }
    Ok(())
}

fn fail(st: &mut State, part: &str, msg: std::string::String) -> Progress {
    let _ = std::fs::remove_file(part);
    st.install = Install::Idle;
    st.target = None;
    Progress::Failed(msg)
}

/// Once per frame on the update page: the download, then the check and the
/// swap.
pub(crate) fn tick_install() -> Progress {
    let Ok(mut st) = STATE.lock() else { return Progress::Pending };
    let Some(target) = self_nro() else {
        st.install = Install::Idle;
        return Progress::Failed(crate::loc::s().upd_err_none.to_string());
    };
    let part = part_path(&target);
    match st.install {
        Install::Idle => Progress::Failed(crate::loc::s().upd_err_none.to_string()),
        Install::Done => Progress::Done,
        Install::Downloading => match crate::net::tick_download() {
            Ok(false) => Progress::Pending,
            Ok(true) => {
                st.install = Install::Verify;
                Progress::Pending
            }
            Err(f) => fail(&mut st, &part, f.msg),
        },
        Install::Verify => {
            let Some(r) = st.target.clone() else {
                return fail(&mut st, &part, crate::loc::s().upd_err_none.to_string());
            };
            let path_c = std::format!("{}\0", part);
            let mut digest = [0u8; 32];
            let hashed = unsafe { flashnx_sha256_file(path_c.as_ptr() as *const c_char, digest.as_mut_ptr()) } == 1;
            let is_nro = file_head(&part, 0x14).get(0x10..0x14) == Some(&b"NRO0"[..]);
            if !hashed || digest != r.sha256 || !is_nro {
                log(&std::format!(
                    "update: {} does not check out (hashed {}, match {}, NRO {})\n",
                    part,
                    hashed,
                    digest == r.sha256,
                    is_nro
                ));
                return fail(&mut st, &part, crate::loc::s().upd_err_corrupt.to_string());
            }
            let target_c = std::format!("{}\0", target);
            if unsafe { flashnx_replace_file(path_c.as_ptr() as *const c_char, target_c.as_ptr() as *const c_char) } != 1
            {
                return fail(&mut st, &part, crate::loc::s().err_sd_write.to_string());
            }
            crate::sd::commit();
            log(&std::format!("update: FlashNX {} is in place at {}\n", r.version, target));
            st.install = Install::Done;
            Progress::Done
        }
    }
}

/// B on the update page: the download stops and its file goes.
pub(crate) fn cancel_install() {
    let Ok(mut st) = STATE.lock() else { return };
    if st.install == Install::Downloading {
        crate::net::cancel_download();
    }
    if let Some(target) = self_nro() {
        let _ = std::fs::remove_file(part_path(&target));
    }
    st.install = Install::Idle;
    st.target = None;
}

/// Downloaded, and being checked and put in place: the page says so.
pub(crate) fn verifying() -> bool {
    STATE.lock().map(|s| s.install == Install::Verify).unwrap_or(false)
}

pub(crate) fn installed() -> Option<std::string::String> {
    STATE.lock().ok().and_then(|s| {
        if s.install == Install::Done {
            s.target.as_ref().map(|r| r.version.clone())
        } else {
            None
        }
    })
}

/// FlashNX can start itself again on the way out (hbmenu, a tile).
pub(crate) fn restart_supported() -> bool {
    unsafe { flashnx_restart_supported() == 1 }
}

/// Ask for FlashNX to start again once it exits. True when the loader took it.
pub(crate) fn request_restart() -> bool {
    unsafe { flashnx_request_restart() == 1 }
}
