//! In-app bug reporting (RÉGLAGES → SIGNALER UN BUG).
//!
//! Lets a player flag a game that renders/plays wrong WITHOUT any account or
//! login: they pick the broken `.swf`, optionally type a short description, and
//! press send. The app POSTs a small JSON payload over HTTPS to a relay
//! endpoint that opens a GitHub issue on the FlashNX repo.
//!
//! ## Why a relay and not "create the issue directly"
//!
//! GitHub's API requires a credential to create an issue. We CANNOT embed a
//! GitHub token in the `.nro`: it's a public homebrew binary, anyone could
//! extract the token and spam/abuse the repo, and GitHub's secret-scanning
//! auto-revokes tokens it finds in published artifacts. So the token has to
//! live server-side. The relay (a tiny Cloudflare Worker — see
//! `tools/bug-report-worker/`) holds a fine-grained token scoped to
//! "Issues: write" on the one repo, and turns our anonymous POST into a real
//! issue. From the player's side it stays a single "send" button, no login.
//!
//! `BUG_REPORT_ENDPOINT` must point at the deployed Worker. Until it does
//! (placeholder URL), `submit` returns a clear "endpoint not configured" error
//! instead of a confusing network failure.

use crate::net;
use core::ffi::{c_char, c_int};

extern "C" {
    /// Last few KB of the log ring (see `ruffle_log_tail` in ruffle_bridge.cpp).
    fn ruffle_log_tail(out: *mut c_char, cap: c_int) -> c_int;
}

/// How much of the log tail travels with a report. The relay fences it into a
/// collapsed block on the issue, and GitHub caps an issue body at 65536 chars,
/// so this stays well clear even after JSON escaping doubles the newlines.
///
/// 6 KB was too small to be worth much: on a real report it covered ~560 lines
/// of session, nearly all of it telemetry, and on a bug noticed mid-session it
/// would have held only the walk back to the menu. The relay caps it again on
/// its own side, so raising this can never produce an issue GitHub refuses.
const LOG_TAIL_CAP: usize = 24 * 1024;

/// The tail of this session's log, for `Report::log_tail`.
fn log_tail() -> std::string::String {
    let mut buf = std::vec![0u8; LOG_TAIL_CAP];
    let n = unsafe { ruffle_log_tail(buf.as_mut_ptr() as *mut c_char, LOG_TAIL_CAP as c_int) };
    if n <= 0 {
        return std::string::String::new();
    }
    buf.truncate(n as usize);
    // Lossy: the log carries game titles, which are not guaranteed UTF-8 once
    // they come from a SWF header, and a report must never fail over that.
    std::string::String::from_utf8_lossy(&buf).into_owned()
}

/// Relay endpoint that creates the GitHub issue. **Deploy the Worker in
/// `tools/bug-report-worker/` and paste its URL here** (e.g.
/// `https://flashnx-bug-report.<your-subdomain>.workers.dev/report`). Left as a
/// placeholder on purpose — the secret GitHub token lives in the Worker, never
/// in this binary.
pub const BUG_REPORT_ENDPOINT: &str =
    "https://flashnx.j-levy228.workers.dev/report";

/// App version reported alongside each bug (the library/release line). Bump
/// with each release so issues are attributable to a build.
pub const APP_VERSION: &str = "1.7.0";

/// Everything we send about a reported game. The relay formats the GitHub
/// issue title/body from these fields, so the client stays format-agnostic.
#[derive(serde::Serialize)]
pub struct Report {
    /// "bug" or "suggestion" — the relay labels the GitHub issue accordingly.
    /// Suggestions carry no game metadata (the game fields are left empty).
    pub kind: &'static str,
    /// Display name of the game (sidecar override or basename). Empty for a
    /// suggestion.
    pub game: std::string::String,
    /// On-SD `.swf` filename — lets the dev match an exact file.
    pub file: std::string::String,
    /// Source URL the game was imported from (recovered by matching the import
    /// history). Empty for Flashpoint downloads (identifiable by their title)
    /// and hand-copied files. The key clue when a URL import has an arbitrary
    /// name like `7k7k7k.swf`.
    pub source_url: std::string::String,
    /// SWF `file_length` header field (bytes).
    pub size: u64,
    pub swf_version: u8,
    /// "FWS" / "CWS" / "ZWS".
    pub compression: std::string::String,
    /// AVM2 (ActionScript 3) movie — the riskier engine.
    pub as3: bool,
    pub app_version: &'static str,
    /// UI language code ("fr"/"en"/"es"/"ru").
    pub lang: &'static str,
    /// True when running in the small applet-memory pool.
    pub applet: bool,
    /// Free-text problem description typed by the player (may be empty).
    pub description: std::string::String,
}

/// True once `BUG_REPORT_ENDPOINT` has been pointed at a real deployment.
fn endpoint_configured() -> bool {
    !BUG_REPORT_ENDPOINT.is_empty() && !BUG_REPORT_ENDPOINT.contains("example")
}

/// Serialize + POST the report to the relay. Blocks for the duration of the
/// HTTPS call (a couple seconds) — call it hoisted out of the LIBRARY lock,
/// like the other synchronous HTTPS flows. Returns `Ok(())` on a 2xx from the
/// relay, `Err(message)` otherwise (message is already localized / human-readable).
pub fn submit(report: &Report) -> Result<(), std::string::String> {
    if !endpoint_configured() {
        // Friendlier than a raw DNS error: the build just hasn't had the relay
        // URL filled in yet. Surfaced on the result screen.
        return Err(std::string::String::from(
            "Bug report endpoint not configured in this build.",
        ));
    }
    // The log tail rides along as an extra field rather than living in `Report`
    // itself: it is gathered at SEND time (so it is as fresh as possible) and
    // only a bug carries it. A suggestion is about a feature, not a failure,
    // and has no reason to publish anything about the player's session.
    #[derive(serde::Serialize)]
    struct Wire<'a> {
        #[serde(flatten)]
        report: &'a Report,
        #[serde(skip_serializing_if = "std::string::String::is_empty")]
        log_tail: std::string::String,
    }
    let wire = Wire {
        report,
        log_tail: if report.kind == "bug" { log_tail() } else { std::string::String::new() },
    };
    let body = serde_json::to_string(&wire)
        .map_err(|e| std::format!("encode failed: {}", e))?;
    net::log(&std::format!(
        "bugreport: POST {} bytes ({} of log) -> {}\n",
        body.len(),
        wire.log_tail.len(),
        BUG_REPORT_ENDPOINT,
    ));
    // 16 KB response cap — the relay only echoes a short JSON (issue URL).
    match net::post_json(BUG_REPORT_ENDPOINT, &body, 16 * 1024) {
        Ok(_) => {
            net::log("bugreport: relay accepted the report\n");
            Ok(())
        }
        Err(e) => {
            net::log(&std::format!("bugreport: relay failed: {}\n", e));
            Err(e)
        }
    }
}
