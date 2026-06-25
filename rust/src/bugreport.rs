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

/// Relay endpoint that creates the GitHub issue. **Deploy the Worker in
/// `tools/bug-report-worker/` and paste its URL here** (e.g.
/// `https://flashnx-bug-report.<your-subdomain>.workers.dev/report`). Left as a
/// placeholder on purpose — the secret GitHub token lives in the Worker, never
/// in this binary.
pub const BUG_REPORT_ENDPOINT: &str =
    "https://flashnx.j-levy228.workers.dev/report";

/// App version reported alongside each bug (the library/release line). Bump
/// with each release so issues are attributable to a build.
pub const APP_VERSION: &str = "1.4.0";

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
    let body = serde_json::to_string(report)
        .map_err(|e| std::format!("encode failed: {}", e))?;
    net::log(&std::format!(
        "bugreport: POST {} bytes -> {}\n",
        body.len(),
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
