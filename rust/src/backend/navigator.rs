//! Sidecar NavigatorBackend — serves a multi-file game's sibling SWFs off the
//! SD card so `loadMovie` / `GetURL`-into-`_levelN` works without a network.
//!
//! Some Flash games are split across several SWFs. Garfield's Scary Scavenger
//! Hunt's `main.swf` does `loadMovieNum("top.swf", 100)`; the `top.swf` loaded
//! into `_level100` sets `_level0.introend = 2`, which is what unblocks
//! `main.swf`'s frame-4 gate (diagnosed 2026-06-11 by disassembling the AS2:
//! `introend` is read at root frame 4 but only ever *written* by top.swf).
//! Ruffle's default `NullNavigatorBackend` silently drops that load, so the
//! game sits forever on a black frame 4.
//!
//! This backend resolves a relative load against the movie's synthetic base URL
//! (`http://flashforswitch.local/<basename>`) and reads the requested sibling
//! from a per-game sidecar directory on the SD card:
//!   `sdmc:/flashnx/<game-stem>.files/<relative-path>`
//! The fetch future is ready immediately (synchronous SD read); the host pumps
//! the resulting loader futures with a `NullExecutor` once per frame — see
//! `State::executor` and `render_frame_with_dt` in lib.rs. Genuine network
//! access stays disabled: a fetch that doesn't map to a local sidecar file just
//! errors, exactly like the old Null backend did.

use std::borrow::Cow;
use std::path::PathBuf;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use encoding_rs::Encoding;
use indexmap::IndexMap;
use url::{ParseError, Url};

use ruffle_core::backend::navigator::{
    async_return, create_specific_fetch_error, url_from_relative_url, ErrorResponse,
    NavigationMethod, NavigatorBackend, NullSpawner, OwnedFuture, Request, SuccessResponse,
};
use ruffle_core::loader::Error;
use ruffle_core::socket::{ConnectionState, SocketAction, SocketHandle};

// ── Deferred failure delivery (issue #76) ─────────────────────────────────
//
// Our lookups hit the SD card, so a MISS fails in about zero milliseconds and
// its callback is delivered in the SAME host frame as the tick that fired the
// request. Content written against a real network does not expect that. Learn
// to Fly 2 leaves its first frame only through a `gotoAndStop(2)` fired from a
// failed analytics callback; when that lands before the root has advanced past
// frame 1, `run_goto` clamps the target to what is loaded, `goto_frame` has
// already stopped the root, and the movie parks on frame 1 for ever, silently.
// Against a real server the round trip takes ~100 ms, the root is past frame 3,
// and the game's own guard skips the goto -- which is why the same build runs
// everywhere else.
//
// So give a failure the latency a real one would have had. This replaced a much
// blunter fix (forcing `LoadBehavior::Delayed` on every movie under 64 MB),
// which worked but finished EVERY game's preloader instantly and so removed
// every loading screen in the launcher.
//
// NOT implemented by having the future wake itself: `NullExecutor::run()` is
// `LocalPool::run_until_stalled()`, so a self-waking future is polled again
// immediately inside the same frame and the delay would be exactly zero. The
// future parks its waker here instead, and the frame loop calls `wake_due`
// before running the executor.

/// How long a failed fetch pretends to have spent on the wire. Long enough that
/// the root timeline advances several frames first (the thing #76 needs), short
/// enough to stay imperceptible on a load that was going to fail anyway.
const FAIL_LATENCY_MS: u64 = 120;

struct ParkedFailure {
    due_tick: u64,
    waker: core::task::Waker,
}

static PARKED: std::sync::Mutex<std::vec::Vec<ParkedFailure>> =
    std::sync::Mutex::new(std::vec::Vec::new());

extern "C" {
    fn ruffle_tick_now() -> u64;
    fn ruffle_tick_freq() -> u64;
}

fn now_ticks() -> u64 {
    unsafe { ruffle_tick_now() }
}

fn ticks_from_ms(ms: u64) -> u64 {
    let freq = unsafe { ruffle_tick_freq() };
    freq.saturating_mul(ms) / 1000
}

/// Wake every parked failure whose latency has elapsed. Called once per frame
/// from `render_frame_with_dt`, BEFORE the executor runs, so a woken future is
/// polled in the same pass rather than waiting another frame.
pub fn wake_due_failures() {
    let now = now_ticks();
    let mut ready: std::vec::Vec<core::task::Waker> = std::vec::Vec::new();
    if let Ok(mut parked) = PARKED.lock() {
        parked.retain(|p| {
            if now >= p.due_tick {
                ready.push(p.waker.clone());
                false
            } else {
                true
            }
        });
    }
    for w in ready {
        w.wake();
    }
}

/// Drop anything still parked. Called when a movie is torn down so a pending
/// failure from the previous game can never wake into the next one.
pub fn reset_parked_failures() {
    if let Ok(mut parked) = PARKED.lock() {
        parked.clear();
    }
}

/// A fetch error delivered `FAIL_LATENCY_MS` from now instead of instantly.
struct DelayedError {
    due_tick: u64,
    err: Option<ErrorResponse>,
}

impl core::future::Future for DelayedError {
    type Output = Result<Box<dyn SuccessResponse>, ErrorResponse>;

    fn poll(
        mut self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        if now_ticks() >= self.due_tick {
            // `take` leaves None behind; a second poll after completion would be
            // a bug in the executor, and returning Pending for ever is a much
            // kinder failure than unwrapping a None.
            return match self.err.take() {
                Some(e) => core::task::Poll::Ready(Err(e)),
                None => core::task::Poll::Pending,
            };
        }
        if let Ok(mut parked) = PARKED.lock() {
            let waker = cx.waker().clone();
            // Replace our own previous entry rather than piling up: the executor
            // may poll us more than once before the deadline.
            if !parked.iter().any(|p| p.waker.will_wake(&waker)) {
                parked.push(ParkedFailure { due_tick: self.due_tick, waker });
            }
        }
        core::task::Poll::Pending
    }
}

/// Wrap a fetch error so it arrives with a plausible round-trip latency.
fn delayed_err(err: ErrorResponse) -> OwnedFuture<Box<dyn SuccessResponse>, ErrorResponse> {
    Box::pin(DelayedError {
        due_tick: now_ticks().saturating_add(ticks_from_ms(FAIL_LATENCY_MS)),
        err: Some(err),
    })
}

/// A [`SuccessResponse`] backed by bytes already read from the SD card.
struct SidecarResponse {
    url: String,
    /// `Some` until consumed by `body()` / `next_chunk()`.
    bytes: Option<Vec<u8>>,
}

impl SuccessResponse for SidecarResponse {
    fn url(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.url)
    }

    fn set_url(&mut self, url: String) {
        self.url = url;
    }

    fn body(self: Box<Self>) -> OwnedFuture<Vec<u8>, Error> {
        let bytes = self.bytes.unwrap_or_default();
        Box::pin(async move { Ok(bytes) })
    }

    fn text_encoding(&self) -> Option<&'static Encoding> {
        None
    }

    fn status(&self) -> u16 {
        200
    }

    fn redirected(&self) -> bool {
        false
    }

    fn next_chunk(&mut self) -> OwnedFuture<Option<Vec<u8>>, Error> {
        // Hand back the whole body on the first call, then signal EOF (None).
        let chunk = self.bytes.take();
        Box::pin(async move { Ok(chunk) })
    }

    fn expected_length(&self) -> Result<Option<u64>, Error> {
        Ok(self.bytes.as_ref().map(|b| b.len() as u64))
    }
}

/// Serves a game's sibling files from `sidecar_dir`; rejects everything else.
pub struct SidecarNavigator {
    spawner: NullSpawner,
    /// The synthetic base URL relative loads are resolved against, e.g.
    /// `http://flashforswitch.local/foo.swf`. Normally the movie's own URL, but
    /// see `with_document_base` for HTML-wrapped games.
    base_url: String,
    /// The MOVIE's own URL, which `base_url` may diverge from (HTML-wrapped
    /// games resolve against their container page instead). Kept separately so
    /// the entry-SWF check still compares against the movie itself.
    movie_url: String,
    /// `sdmc:/flashnx/<game-stem>.files` — where this game's sibling SWFs live.
    sidecar_dir: PathBuf,
    /// True when the movie runs under its original Flashpoint launchCommand host
    /// (not the synthetic `flashforswitch.local`). A missing sidecar may then be
    /// fetched on demand from the Flashpoint "Legacy htdocs" mirror — see `fetch`.
    /// False for the user's own local SWFs (no network).
    htdocs_proxy: bool,
    /// How many `connectMovie` calls this game has made to the NewgroundsAPI
    /// gateway stub. Bounds an in-game RETRY LOOP, so it belongs to the player,
    /// not to the process: a per-process counter made the stub stall on the 4th
    /// launch of a Newgrounds game in one app session, which left the game stuck
    /// on "Connecting to the Newgrounds API Gateway...". See `fetch`.
    ng_connect_calls: core::sync::atomic::AtomicU32,
    /// Every file in this game's tree, indexed on the first miss so
    /// `tail_match_path` can answer without walking the card again.
    tree_index: core::cell::RefCell<Option<std::sync::Arc<Vec<(Vec<String>, PathBuf)>>>>,
}

impl SidecarNavigator {
    pub fn new(spawner: NullSpawner, base_url: String, sidecar_dir: PathBuf) -> Self {
        // Flashpoint games carry a real host (from their launchCommand `.base`
        // sidecar); the synthetic `flashforswitch.local` host marks a direct /
        // user-supplied SWF, for which we never touch the network.
        let htdocs_proxy = Url::parse(&base_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h != "flashforswitch.local"))
            .unwrap_or(false);
        Self {
            spawner,
            movie_url: base_url.clone(),
            base_url,
            sidecar_dir,
            htdocs_proxy,
            ng_connect_calls: core::sync::atomic::AtomicU32::new(0),
            tree_index: core::cell::RefCell::new(None),
        }
    }

    /// Resolve relative loads against the container PAGE's directory instead of
    /// the movie's own location.
    ///
    /// Flash resolves a relative URL against the HTML document embedding the
    /// movie, not against the SWF file. It only matters when the two differ,
    /// which is exactly the Flashpoint HTML-wrapped layout: Dragon City's page
    /// is at `.../dragoncity/` while its movie lives in `.../dragoncity/flash/`,
    /// so its `assets/...` requests were resolving to `.../dragoncity/flash/assets/...`
    /// and missing all 2901 asset files. Only called when a container page was
    /// actually found, so plain SWFs keep resolving against the movie.
    pub fn with_document_base(mut self, page_base: String) -> Self {
        self.base_url = page_base;
        self
    }

    /// On-demand fetch of a missing sidecar from the Flashpoint "Legacy htdocs"
    /// mirror, caching it to `cache_path` (the host-mirrored sidecar layout) so a
    /// replay is offline and a delete cleans it up. The request URL's path is
    /// already percent-encoded by the `url` crate; the query is dropped (the
    /// mirror serves static files). Returns the bytes, or None on any miss so the
    /// caller falls through to the usual not-found error. CONTENT-NEUTRAL: same
    /// mirror the GameZIP came from, only for the game the user downloaded.
    /// The hosts to try on the mirror, in order: the one asked for, then its
    /// `www` sibling and the bare domain.
    ///
    /// Deliberately generic — no site is named here. A capture keeps a file
    /// under the host it was fetched from, and a game may well request it from
    /// another of the site's own hosts; asking the same path of `www.<domain>`
    /// costs one request, only ever after a miss, and covers that.
    fn host_fallbacks_impl(host: &str) -> std::vec::Vec<std::string::String> {
        let mut out = std::vec![std::string::String::from(host)];
        let labels: std::vec::Vec<&str> = host.split('.').collect();
        if labels.len() > 2 {
            let domain = labels[labels.len() - 2..].join(".");
            for candidate in [std::format!("www.{domain}"), domain] {
                if !out.contains(&candidate) {
                    out.push(candidate);
                }
            }
        }
        out
    }

    fn fetch_from_mirror(
        &self,
        resolved: &Url,
        cache_path: &std::path::Path,
    ) -> Option<std::vec::Vec<u8>> {
        let host = resolved.host_str()?;
        const CAP: usize = 16 * 1024 * 1024;
        // The same path under a few sibling hosts, because the archive keeps a
        // file under the host it was CAPTURED from, which is not always the host
        // a game asks it from. Hasee Bounce (#86) requests its Neopets game
        // system from `images.neopets.com`, where the mirror answers 404, while
        // the very same file sits under `images50.neopets.com` and
        // `www.neopets.com`. Only tried after the requested host has failed, so
        // a game that asks correctly costs nothing.
        for candidate in Self::host_fallbacks_impl(host) {
            let mirror = std::format!(
                "https://infinity.unstable.life/Flashpoint/Legacy/htdocs/{}{}",
                candidate,
                resolved.path()
            );
            match crate::net::http_get(&mirror, CAP) {
                Ok(bytes) => {
                    if candidate != host {
                        tracing::info!(
                            "sidecar: {} served from mirror host {} ({} bytes)",
                            resolved,
                            candidate,
                            bytes.len()
                        );
                    } else {
                        tracing::info!(
                            "sidecar: fetched {} from mirror ({} bytes)",
                            resolved,
                            bytes.len()
                        );
                    }
                    if let Some(p) = cache_path.to_str() {
                        crate::sources::gamezip::write_sidecar_abs(p, &bytes);
                    }
                    return Some(bytes);
                }
                Err(e) => {
                    tracing::warn!("sidecar: mirror miss {} ({}): {}", resolved, mirror, e);
                }
            }
        }
        None
    }

    /// Map a resolved request URL to a local sidecar path: take the URL's path
    /// Host-mirrored path: `<sidecar_dir>/<host>/<path-segments>`. Matches the
    /// layout written by `gamezip::extract_gamezip_tree` (the GameZIP's full
    /// `content/<host>/<path>` tree), so a game that loads an asset by its
    /// original absolute URL (e.g. `http://www.fliplineads.com/serve/data/x.xml`)
    /// resolves to the bundled stub. Empty and `..` segments are dropped so a
    /// load can't escape the dir.
    /// Where an archived answer to a DYNAMIC request lives, when the archive
    /// stored it under a path built from that request's parameters.
    ///
    /// Flashpoint cannot keep a PHP endpoint alive, so it keeps its ANSWER as a
    /// file — and the file's path is the query, rearranged. The Neopets game
    /// system asks its translations from
    /// `.../gettranslationxml.phtml?lang=en&type_id=4&item_id=1157`, and the
    /// capture holds `www.neopets.com/transcontent/4/1157/en.xml` (#85). Every
    /// Neopets game of that era makes this call at boot and stops on its NeoBIOS
    /// screen when it fails, so the rewrite is worth naming explicitly rather
    /// than hoping a generic search stumbles onto the right file.
    fn archived_dynamic_path(&self, url: &Url, body: Option<&str>) -> Option<std::string::String> {
        if !url.path().ends_with("gettranslationxml.phtml") {
            return None;
        }
        let mut lang = None;
        let mut type_id = None;
        let mut item_id = None;
        // The parameters come in the query string on the first call and in a
        // POST body on the second — same endpoint, same answer, and the game
        // makes both before it will leave its BIOS screen.
        let mut take = |k: &str, v: std::string::String| match k {
            "lang" => lang = Some(v),
            "type_id" => type_id = Some(v),
            "item_id" => item_id = Some(v),
            _ => {}
        };
        for (k, v) in url.query_pairs() {
            take(k.as_ref(), v.into_owned());
        }
        if let Some(body) = body {
            for pair in body.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    // The KEY needs decoding too, not just the value: this game
                    // posts `type%5Fid=4&item%5Fid=1157`, where %5F is the
                    // underscore. Comparing the raw key found nothing, which is
                    // why the second call still failed with the body in hand.
                    take(
                        &crate::sources::gamezip::percent_decode(k),
                        crate::sources::gamezip::percent_decode(v),
                    );
                }
            }
        }
        let Some(((lang, type_id), item_id)) = lang.zip(type_id).zip(item_id) else {
            // No parameters anywhere — logged rather than guessed, because the
            // game calls this endpoint a second time bare and what it expects
            // then is not something the archive shows.
            tracing::warn!(
                "sidecar: {} asked with no lang/type_id/item_id — body: {:?}",
                url,
                body.map(|b| &b[..b.len().min(200)]).unwrap_or(""),
            );
            return None;
        };
        // Lowercased: the same game asks with `lang=en` in the query and
        // `lang=EN` in the body, and the archive holds `en.xml`. The local
        // lookup compares without case, but the mirror does not.
        let lang = lang.to_ascii_lowercase();
        Some(std::format!("transcontent/{type_id}/{item_id}/{lang}.xml"))
    }

    /// The archived answer, if this game's tree holds it.
    fn archived_dynamic_local(&self, rel: &str) -> Option<PathBuf> {
        let index = self.tree_index();
        let wanted: std::vec::Vec<std::string::String> =
            rel.split('/').map(|s| s.to_ascii_lowercase()).collect();
        index
            .iter()
            .find(|(segments, _)| {
                segments.len() >= wanted.len()
                    && segments[segments.len() - wanted.len()..] == wanted[..]
            })
            .map(|(_, path)| path.clone())
    }

    fn local_path(&self, url: &Url) -> PathBuf {
        let mut path = self.sidecar_dir.clone();
        if let Some(host) = url.host_str() {
            path.push(host);
        }
        self.push_segments(&mut path, url);
        path
    }

    /// Locate a SHARED Disney container asset in a SIBLING game's tree.
    ///
    /// `/v1/game_container/**` is Disney's common minigame runtime (the API SWF
    /// and its XML config), identical for every one of their titles. Some
    /// Flashpoint packages bundle it (Agent P Strikes Back ships
    /// `as3MinigameApi_2_5_6.swf` + `minigameAPIConfig.xml`), others don't (Tron
    /// Uprising: Escape from Argon City), and the Legacy htdocs mirror 404s on
    /// those paths under EVERY host. A game whose package omits them can't load
    /// its API and its preloader waits forever.
    ///
    /// So: look for the same `/v1/game_container/...` path in the other
    /// `<game>.files/` trees on the card. The host directory deliberately does
    /// NOT have to match — Agent P files them under `play.lol.disney.com` while
    /// Tron asks for `img.lum.dolimg.com`, and it is the same asset either way.
    ///
    /// This only ever reads files the user already downloaded, and only for this
    /// one path prefix. It does mean a game can depend on another being
    /// installed, which is why the hit is logged explicitly.
    fn shared_container_path(&self, url: &Url) -> Option<PathBuf> {
        let mut want = PathBuf::new();
        self.push_segments(&mut want, url);
        // Guard: only Disney's shared container tree, never arbitrary assets.
        let rel = want.to_string_lossy().replace('\\', "/");
        if !rel.contains("v1/game_container/") {
            return None;
        }
        // NO directory reading: `std::fs::read_dir` corrupts entry names on
        // Horizon (see the SWF_CANDIDATES note in lib.rs — a 23-char name came
        // back missing its first 2 bytes), which is exactly why the first
        // attempt at this silently found nothing. Both the trees to search and
        // the host folder inside each are therefore derived from data we already
        // hold: the scanned game paths, and each game's `.base` sidecar (its
        // original launchCommand, whose host names the folder).
        let mut looked = 0usize;
        for game in crate::library::scanned_game_paths() {
            // Same helper the rest of the app uses: the tree REPLACES the `.swf`
            // extension (`<game>.files`), it does not append to it.
            let tree = crate::sidecar_dir_for(Some(&game));
            if tree == self.sidecar_dir {
                continue;
            }
            // Hosts to try inside that tree: the one the sibling was published
            // under, and the one WE are asking for (Agent P's package carries
            // files under both `play.lol.disney.com` and `img.lum.dolimg.com`).
            let base = read_sidecar_file(&PathBuf::from(std::format!("{}.base", game)))
                .and_then(|b| std::string::String::from_utf8(b).ok())
                .and_then(|u| Url::parse(u.trim()).ok())
                .and_then(|u| u.host_str().map(|h| h.to_string()));
            for host in [base.as_deref(), url.host_str()].into_iter().flatten() {
                let candidate = tree.join(host).join(&want);
                looked += 1;
                if let Some(bytes) = read_sidecar_file(&candidate) {
                    tracing::info!(
                        "shared container: {} found in {} ({} bytes)",
                        rel,
                        candidate.display(),
                        bytes.len(),
                    );
                    return Some(candidate);
                }
            }
        }
        tracing::warn!(
            "shared container: {} in no other game's tree ({} candidate(s) tried)",
            rel,
            looked,
        );
        None
    }

    /// Flat path: `<sidecar_dir>/<path-segments>` (NO host). The legacy layout
    /// for companions pulled by `fetch_siblings` from the htdocs mirror, which
    /// land directly in `<game>.files/<leaf>.swf` (e.g. Garfield's `top.swf`).
    /// Used as a fallback when the host-mirrored path isn't present.
    fn flat_path(&self, url: &Url) -> PathBuf {
        let mut path = self.sidecar_dir.clone();
        self.push_segments(&mut path, url);
        path
    }

    /// Leaf-only path: `<sidecar_dir>/<last-segment>`. Companions fetched flat
    /// from the htdocs mirror land directly in `<game>.files/<leaf>.swf`
    /// (Garfield's top.swf, books.swf, ...). Needed because the movie runs under
    /// its original launchCommand base URL, so a relative load ("top.swf")
    /// resolves WITH the host path — `flat_path` (all segments) then misses the
    /// flat companion, while this matches it.
    fn leaf_path(&self, url: &Url) -> PathBuf {
        let mut path = self.sidecar_dir.clone();
        if let Some(leaf) = url
            .path_segments()
            .and_then(|segs| segs.filter(|s| !s.is_empty() && *s != "..").last())
        {
            path.push(leaf);
        }
        path
    }

    /// Last local layer: any file in the game's tree whose path ENDS WITH the
    /// requested one.
    ///
    /// The three layers above all assume the archived tree mirrors the URL the
    /// movie asks with, and that is regularly untrue. Angry Birds Cheetos (#88)
    /// asks for `<host>/external_assets/AngryBirdsCredits.swf` while its own
    /// GameZIP stores that file under `<host>/flash/external_assets/` — one
    /// directory deeper, because the SWF resolves against its containing page
    /// rather than against itself. The same archive carries `flash/flash/...`
    /// paths, so the mismatch runs both ways. The file was there all along; the
    /// game's loader treated the miss as fatal and never left its loading screen.
    ///
    /// Matching on the tail keeps this specific: `external_assets/Credits.swf`
    /// will not match some other `Credits.swf` elsewhere in the tree unless the
    /// whole tail agrees. On a tie, the shallowest path wins, so the result does
    /// not depend on directory iteration order. The walk happens once per game,
    /// on the first miss, and is then reused.
    fn tail_match_path(&self, url: &Url) -> Option<PathBuf> {
        let wanted: Vec<String> = url
            .path_segments()?
            .filter(|s| !s.is_empty() && *s != "..")
            // The tree holds literal names; the URL is encoded ("%20" for the
            // space in a file name), so compare on the decoded form.
            .map(|s| crate::sources::gamezip::percent_decode(s).to_ascii_lowercase())
            .collect();
        if wanted.is_empty() {
            return None;
        }
        let index = self.tree_index();
        // A GameZIP often carries the same asset under several hosts — this
        // game ships an `-ru` twin of every file — so the tail alone can match
        // twice. Rank the URL's own host first, then the shallowest path, so the
        // answer is both right and stable.
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let mut best: Option<((u8, usize), PathBuf)> = None;
        for (segments, path) in index.iter() {
            if segments.len() < wanted.len() {
                continue;
            }
            if segments[segments.len() - wanted.len()..] != wanted[..] {
                continue;
            }
            let same_host = segments.first().is_some_and(|s| *s == host);
            let score = (u8::from(!same_host), segments.len());
            if best.as_ref().is_none_or(|(s, _)| score < *s) {
                best = Some((score, path.clone()));
            }
        }
        best.map(|(_, p)| p)
    }

    /// Every file under the game's tree, as lowercased segment lists. Built once.
    fn tree_index(&self) -> std::sync::Arc<Vec<(Vec<String>, PathBuf)>> {
        if let Some(index) = self.tree_index.borrow().as_ref() {
            return index.clone();
        }
        // Walked in C++, never with `std::fs::read_dir`: that one corrupts entry
        // names on Horizon, and a Rust walk of this very tree reported it EMPTY
        // while the files sat there on the card (#88). Same reason the library
        // scan, the extraction and the sidecar reads all go through C++.
        let mut root = self.sidecar_dir.to_string_lossy().into_owned().into_bytes();
        root.push(0);
        let mut buf = std::vec![0u8; 256 * 1024];
        let n = unsafe {
            swf_picker_list_tree(
                root.as_ptr() as *const core::ffi::c_char,
                buf.as_mut_ptr() as *mut core::ffi::c_char,
                buf.len() as core::ffi::c_int,
            )
        };
        let mut out: Vec<(Vec<String>, PathBuf)> = Vec::new();
        if n > 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            let listing = std::string::String::from_utf8_lossy(&buf[..end]).into_owned();
            for rel in listing.lines().filter(|l| !l.is_empty()) {
                let segments: Vec<String> = rel
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_ascii_lowercase())
                    .collect();
                if segments.is_empty() {
                    continue;
                }
                let mut path = self.sidecar_dir.clone();
                for seg in rel.split('/').filter(|s| !s.is_empty()) {
                    path.push(seg);
                }
                out.push((segments, path));
            }
        }
        let index = std::sync::Arc::new(out);
        *self.tree_index.borrow_mut() = Some(index.clone());
        index
    }

    /// True when `resolved` points at the movie's OWN entry SWF (its base URL).
    /// That file is stored flat as the library `<game>.swf` (not in the tree —
    /// see `gamezip::extract_gamezip_tree`), so `fetch`'s layer 0 serves it from
    /// there if the running movie ever re-requests its own URL (e.g. a restart
    /// that reloads the root SWF). Compared by host + path, ignoring the query.
    ///
    /// The path compare is case-INSENSITIVE on purpose: extraction decides which
    /// entry to keep flat with `eq_ignore_ascii_case` too (see
    /// `gamezip::extract_gamezip_tree`). A byte-exact compare here would leave a
    /// GameZIP whose zip entry casing differs from its launchCommand (`Game.SWF`
    /// vs `game.swf`) with NO reachable copy of its entry SWF — skipped at
    /// extraction, then not matched here.
    fn is_movie_entry(&self, resolved: &Url) -> bool {
        Url::parse(&self.movie_url).ok().is_some_and(|b| {
            b.host_str() == resolved.host_str()
                && b.path().eq_ignore_ascii_case(resolved.path())
        })
    }

    fn push_segments(&self, path: &mut PathBuf, url: &Url) {
        if let Some(segs) = url.path_segments() {
            for seg in segs {
                if seg.is_empty() || seg == ".." {
                    continue;
                }
                // NOTE: segments are still percent-encoded; sibling files with
                // spaces/special chars would need decoding here. top.swf and the
                // common case have none, so we keep it simple for now.
                path.push(seg);
            }
        }
    }
}

/// Hosts of defunct ad/analytics networks (MochiAds shut down in 2014, etc.).
/// Games that gate their preloader on `MochiAd.showPreGameAd` expect these to be
/// UNREACHABLE — the connection hangs, then the preloader's own ~3 s ad_timeout
/// fires and fail-opens into the game. We let such requests hang for exactly
/// that reason (see `fetch`), instead of erroring immediately.
fn is_dead_ad_host(host: &str) -> bool {
    // mochibot.com is NOT in here. It is a hit counter, not an ad server: a game
    // fires it and forgets it, so there is nothing to fail open FROM, and hanging
    // it forever leaves a loader pending for the rest of the session. The reason
    // this list exists -- a preloader that gates the game on an ad and needs the
    // server to look unreachable rather than absent -- applies to the ad hosts
    // only. Learn to Fly 2 goes silent immediately after its MochiBot ping and
    // never runs another line of script; that is the case this split is meant to
    // answer, and it is a HYPOTHESIS until the console says otherwise.
    const DEAD: &[&str] = &["mochiads.com", "mochimedia.com"];
    DEAD.iter()
        .any(|d| host == *d || host.ends_with(&std::format!(".{d}")))
}

/// Hit counters: answered with a normal "not found" straight away rather than
/// hung, so whatever the game does next is not waiting on us.
fn is_tracker_host(host: &str) -> bool {
    const TRACKERS: &[&str] = &["mochibot.com"];
    TRACKERS
        .iter()
        .any(|d| host == *d || host.ends_with(&std::format!(".{d}")))
}

// Hosts of the legacy NewgroundsAPI v2 gateway we answer with a synthetic
// success instead of erroring, so its games leave their preloader (Ruffle #896).
fn is_newgrounds_gateway_host(host: &str) -> bool {
    const NG: &[&str] = &["ngads.com", "newgrounds.com", "ungrounded.net"];
    NG.iter()
        .any(|d| host == *d || host.ends_with(&std::format!(".{d}")))
}

extern "C" {
    // C++/libnx sidecar reads (cpp/src/swf_picker.cpp). Rust's std::fs::read
    // returns ENOENT for some files that ARE on disk on Horizon (verified
    // 2026-06-14: C++ re-reads every extracted file fine; Rust's std::fs misses
    // a few — same newlib-glue unreliability as the read_dir/metadata bugs).
    fn swf_picker_file_size(path: *const core::ffi::c_char) -> i64;
    fn swf_picker_read_file(path: *const core::ffi::c_char, buf: *mut u8, cap: u64) -> i64;
    /// Every file under `root`, one relative path per line. Same reason as the
    /// two above: Rust cannot list a directory on Horizon without corrupting
    /// the names.
    fn swf_picker_list_tree(
        root: *const core::ffi::c_char,
        out: *mut core::ffi::c_char,
        cap: core::ffi::c_int,
    ) -> core::ffi::c_int;
}

/// Read a sidecar file via C++/libnx (reliable on Horizon, unlike std::fs::read).
/// Returns None if the file is absent or unreadable.
fn read_sidecar_file(path: &std::path::Path) -> Option<std::vec::Vec<u8>> {
    let s = path.to_str()?;
    let c = std::ffi::CString::new(s).ok()?;
    let sz = unsafe { swf_picker_file_size(c.as_ptr()) };
    if sz < 0 {
        return None;
    }
    let sz = sz as usize;
    let mut buf = std::vec![0u8; sz];
    let n = unsafe { swf_picker_read_file(c.as_ptr(), buf.as_mut_ptr(), sz as u64) };
    if n < 0 || n as usize != sz {
        return None;
    }
    Some(buf)
}

impl NavigatorBackend for SidecarNavigator {
    fn navigate_to_url(
        &self,
        _url: &str,
        _target: &str,
        _vars_method: Option<(NavigationMethod, IndexMap<String, String>)>,
    ) {
        // No browser to hand off to on a console.
    }

    fn fetch(&self, request: Request) -> OwnedFuture<Box<dyn SuccessResponse>, ErrorResponse> {
        let raw = request.url().to_string();
        // Resolve relative loads (e.g. "top.swf") against the movie's base URL.
        // Ruffle's AVM1 loadMovie path hands us the raw relative string without
        // calling resolve_url first, so we resolve here.
        let resolved = match url_from_relative_url(&self.base_url, &raw) {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!("sidecar: can't resolve URL {raw}: {e}");
                return async_return(Err(create_specific_fetch_error(
                    "Unresolvable URL",
                    &raw,
                    e,
                )));
            }
        };
        // NewgroundsAPI v2 gateway (Newgrounds Rumble & co., Ruffle #896): the
        // gateway host is dead offline. Returning a FetchError makes the AS2
        // `connectMovie` fire its failure path ("Could not contact the API
        // Gateway") and, crucially, never calls `reportAssetLoaded()`, so the
        // API's `getPercentLoaded()` caps at 80% (bytes are 80%, the connect
        // preload item is the other 20%) and the game's custom preloader keeps
        // its "WAIT" button forever. Answer 200 with the minimal JSON the API's
        // `onData` expects (`com.Newgrounds.JSON.decode`, routed by `command_id`
        // which `getCommandName` returns verbatim): `success` truthy takes the
        // `doEvent` path, and the connectMovie case only reads OPTIONAL fields
        // before `reportAssetLoaded()` fires -> preload hits 100% -> "PLAY".
        // Echo the requested command from the POST form so any later command
        // (getMedals, postScore, ...) routes to its own success case too. We
        // still never touch the network. Validated hw with Newgrounds Rumble.
        let is_ng_gateway = resolved.host_str().is_some_and(is_newgrounds_gateway_host)
            || resolved.path().ends_with("gateway_v2.php");
        if is_ng_gateway {
            let cmd = request
                .body()
                .as_ref()
                .and_then(|(b, _)| {
                    String::from_utf8_lossy(b)
                        .split('&')
                        .find_map(|kv| kv.strip_prefix("command_id=").map(str::to_string))
                })
                .unwrap_or_else(|| "connectMovie".to_string());
            // A synthetic success is enough to get most NG API v2 games past
            // their preloader, but not all: haunt-the-house re-issues
            // `connectMovie` forever because our reply carries no movie identity
            // ("Movie identified as \"undefined\"" in its own trace). That spins
            // the AVM at 100%, so even the pause menu stops responding. The real
            // v2 response schema is undocumented (the API is long retired), so
            // rather than guess field names we break the LOOP: after a few
            // attempts the request simply hangs, exactly as we already do for
            // dead ad hosts. The game either fails open on its own timeout or at
            // minimum stops burning the CPU, leaving the app usable and quittable.
            // The budget is PER PLAYER (`self`), not per process: it bounds one
            // game's retry loop, and a new launch or RESTART is a new loop. As a
            // process-wide static it summed across every game played in the app
            // session, so the 4th Newgrounds launch stalled on its very first
            // call and the game sat forever on "Connecting to the Newgrounds API
            // Gateway...".
            const NG_CONNECT_ATTEMPTS_BEFORE_STALL: u32 = 3;
            if cmd == "connectMovie" {
                let n = self
                    .ng_connect_calls
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                if n >= NG_CONNECT_ATTEMPTS_BEFORE_STALL {
                    if n == NG_CONNECT_ATTEMPTS_BEFORE_STALL {
                        tracing::warn!(
                            "sidecar: NewgroundsAPI connectMovie looping ({n} calls), stalling further ones"
                        );
                    }
                    return Box::pin(std::future::pending::<
                        Result<Box<dyn SuccessResponse>, ErrorResponse>,
                    >());
                }
            }
            // `connectMovie` is the API handshake and the game reads a movie
            // IDENTITY back from it; a bare success makes it log
            // `Movie identified as "undefined"` and retry forever (43447 calls in
            // one session). The v2 schema is undocumented, so these field names
            // are a best guess at what the AS class reads — harmless if wrong,
            // since the loop-breaker above still bounds the damage. Before this
            // stub existed the request simply ERRORED and such games coped, so a
            // wrong-shaped success is worse than no answer: never widen this stub
            // without checking a game that used to work.
            // The version we hand back is compared against the movie's OWN
            // declared version, and a mismatch makes the API cover the running
            // game with its "a new version of this movie is available" panel
            // (Infiltrating the Airship: its trace read `Current version:` empty
            // against our hardcoded `Newest version: 1`). So ECHO whatever
            // version the request carried: that always means "you are up to
            // date", instead of asserting a number we cannot know. The v2
            // parameter name is undocumented, hence matching any `*version*`
            // key; absent = empty, which also matches a movie that declares none.
            let movie_version = request
                .body()
                .as_ref()
                .and_then(|(b, _)| {
                    String::from_utf8_lossy(b).split('&').find_map(|kv| {
                        let (k, v) = kv.split_once('=')?;
                        k.to_ascii_lowercase()
                            .contains("version")
                            .then(|| v.to_string())
                    })
                })
                // Keep it JSON-safe: a version is digits/dots, never quotes.
                .map(|v| {
                    v.chars()
                        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                        .collect::<String>()
                })
                .unwrap_or_default();
            let body = if cmd == "connectMovie" {
                std::format!(
                    "{{\"success\":true,\"command_id\":\"{cmd}\",\"movie_id\":1,                     \"movie_name\":\"FlashNX\",\"movie_version\":\"{movie_version}\",                     \"ad_url\":\"\",\"deny_host\":false,\"session_id\":\"flashnx\"}}"
                )
                .into_bytes()
            } else {
                std::format!("{{\"success\":true,\"command_id\":\"{cmd}\"}}").into_bytes()
            };
            tracing::info!("sidecar: NewgroundsAPI gateway stubbed (cmd={cmd}) for {resolved}");
            let resp: Box<dyn SuccessResponse> = Box::new(SidecarResponse {
                url: resolved.to_string(),
                bytes: Some(body),
            });
            return async_return(Ok(resp));
        }
        // Defunct ad/analytics hosts: mimic an unreachable server (hang) rather
        // than erroring. Preloaders that gate on MochiAd & co. fail-open via
        // their own timeout when the ad server is unreachable; our usual
        // immediate "not found" error instead breaks that path and leaves the
        // game stuck on the sponsor / "update Flash" screen (observed on
        // Papa Louie 2's MochiAds preloader). We still never touch the network.
        if resolved.host_str().is_some_and(is_tracker_host) {
            tracing::info!("sidecar: tracker {resolved} refused after a round-trip delay");
            let err = Error::FetchError(std::format!("tracker not served: {resolved}"));
            // Refused, but NOT in zero milliseconds: this is the exact call whose
            // instant failure strands Learn to Fly 2 on frame 1 (see #76 and
            // `delayed_err`). A tracker is fire-and-forget for the game, so the
            // only thing the wait costs is nothing.
            return delayed_err(ErrorResponse { url: resolved.to_string(), error: err });
        }
        if resolved.host_str().is_some_and(is_dead_ad_host) {
            tracing::info!("sidecar: stalling dead ad host {resolved} (mimic unreachable)");
            return Box::pin(std::future::pending::<
                Result<Box<dyn SuccessResponse>, ErrorResponse>,
            >());
        }
        // Resolve in three layers (first hit wins):
        //   1. host-mirrored  `<dir>/<host>/<path>`  (full GameZIP tree, e.g.
        //      Super Brawl 2's extracted assets).
        //   2. flat-with-path `<dir>/<path>`         (legacy, no host).
        //   3. leaf-only       `<dir>/<leaf>`        (htdocs-fetched companions
        //      land directly in `<game>.files/<leaf>.swf`, e.g. Garfield's
        //      top.swf). This layer is REQUIRED since the movie now runs under
        //      its original launchCommand base URL, so a relative load like
        //      "top.swf" resolves WITH the host path and layers 1-2 miss the
        //      flat companion. (2026-06-14 regression fix.)
        // Layer 0: the movie's own entry SWF is NOT kept in the tree (it's the
        // flat library `<game>.swf`). Serve it from there if re-requested by URL.
        let entry_flat = self
            .is_movie_entry(&resolved)
            .then(|| self.sidecar_dir.with_extension("swf"));
        let host_path = self.local_path(&resolved);
        let flat = self.flat_path(&resolved);
        let leaf = self.leaf_path(&resolved);
        // Layer 4: the same file, deeper in the tree than the URL says (#88), or
        // the archived answer to a dynamic request (#85). Both are tried before
        // the network: a file we already have on the card beats a round trip to
        // a mirror that will probably 404 too.
        let post_body = request
            .body()
            .as_ref()
            .and_then(|(bytes, _)| std::str::from_utf8(bytes).ok())
            .map(std::string::String::from);
        // The archived answer to a dynamic request (#85): a relative path built
        // from the query, looked for in this game's tree and — since a game can
        // have no tree at all, like Hasee Bounce (#86) — on the mirror too.
        let archived_rel = self.archived_dynamic_path(&resolved, post_body.as_deref());
        let tail = archived_rel
            .as_deref()
            .and_then(|rel| self.archived_dynamic_local(rel))
            .or_else(|| self.tail_match_path(&resolved));
        let found = entry_flat
            .as_ref()
            .and_then(|p| read_sidecar_file(p).map(|b| (b, p)))
            .or_else(|| read_sidecar_file(&host_path).map(|b| (b, &host_path)))
            .or_else(|| read_sidecar_file(&flat).map(|b| (b, &flat)))
            .or_else(|| read_sidecar_file(&leaf).map(|b| (b, &leaf)))
            .or_else(|| {
                tail.as_ref().and_then(|p| {
                    read_sidecar_file(p).map(|b| {
                        tracing::info!(
                            "sidecar: {} served from {} (matched by path tail)",
                            resolved,
                            p.display(),
                        );
                        (b, p)
                    })
                })
            });
        let (bytes, from) = match found {
            Some((b, p)) => (b, p.clone()),
            None => {
                // Not on the SD card. For a Flashpoint game (running under its
                // original launchCommand host) the asset may be on the Legacy
                // htdocs mirror. Many games build asset paths dynamically in
                // ActionScript (Racing is Magic loads `xml/config.xml` at
                // runtime), so the static download-time prefetch can't know about
                // them — fetch on demand, cache, and serve.
                // The rewritten path first: the mirror answers it (verified for
                // both Neopets games) while it 404s the PHP endpoint the game
                // actually asked for.
                if self.htdocs_proxy {
                    if let Some(rel) = archived_rel.as_deref() {
                        if let Ok(rewritten) =
                            Url::parse(&std::format!("http://{}/{}", resolved.host_str().unwrap_or("www.neopets.com"), rel))
                        {
                            if let Some(b) = self.fetch_from_mirror(&rewritten, &host_path) {
                                tracing::info!(
                                    "sidecar: {} served from mirror as {}",
                                    resolved,
                                    rel
                                );
                                let resp: Box<dyn SuccessResponse> = Box::new(SidecarResponse {
                                    url: resolved.to_string(),
                                    bytes: Some(b),
                                });
                                return async_return(Ok(resp));
                            }
                        }
                    }
                }
                if self.htdocs_proxy {
                    if let Some(b) = self.fetch_from_mirror(&resolved, &host_path) {
                        let resp: Box<dyn SuccessResponse> = Box::new(SidecarResponse {
                            url: resolved.to_string(),
                            bytes: Some(b),
                        });
                        return async_return(Ok(resp));
                    }
                }
                // Last resort for Disney's shared minigame runtime: a sibling
                // game's tree may carry the very same file (see
                // `shared_container_path`).
                if let Some(shared) = self.shared_container_path(&resolved) {
                    if let Some(b) = read_sidecar_file(&shared) {
                        tracing::info!(
                            "sidecar: served {} ({} bytes) from ANOTHER game's tree {}",
                            resolved,
                            b.len(),
                            shared.display(),
                        );
                        let resp: Box<dyn SuccessResponse> = Box::new(SidecarResponse {
                            url: resolved.to_string(),
                            bytes: Some(b),
                        });
                        return async_return(Ok(resp));
                    }
                }
                // Say what the tree actually holds, not just which four paths we
                // tried. "Not found" reads the same whether the file is one
                // directory away or the tree was never extracted at all, and
                // those two call for opposite fixes (#88).
                let index = self.tree_index();
                let sample: std::vec::Vec<std::string::String> = index
                    .iter()
                    .take(6)
                    .map(|(segs, _)| segs.join("/"))
                    .collect();
                tracing::warn!(
                    "sidecar: {} not found ({}, {}, {}); tree {} holds {} file(s): {}",
                    resolved,
                    host_path.display(),
                    flat.display(),
                    leaf.display(),
                    self.sidecar_dir.display(),
                    index.len(),
                    if sample.is_empty() {
                        std::string::String::from("(empty)")
                    } else {
                        sample.join(", ")
                    },
                );
                let e = std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "sidecar file not found",
                );
                // Delayed like the tracker refusal above: a missing companion is
                // this backend's equivalent of a 404, and content that reacts to
                // one expects it to arrive after a round trip, not inside the
                // frame that asked. See `delayed_err` and #76.
                return delayed_err(create_specific_fetch_error(
                    "Sidecar file not found",
                    resolved.as_str(),
                    e,
                ));
            }
        };
        tracing::info!("sidecar: served {} ({} bytes) from {}", resolved, bytes.len(), from.display());
        let resp: Box<dyn SuccessResponse> = Box::new(SidecarResponse {
            url: resolved.to_string(),
            bytes: Some(bytes),
        });
        async_return(Ok(resp))
    }

    fn resolve_url(&self, url: &str) -> Result<Url, ParseError> {
        url_from_relative_url(&self.base_url, url)
    }

    fn spawn_future(&mut self, future: OwnedFuture<(), Error>) {
        self.spawner.spawn_local(future);
    }

    fn pre_process_url(&self, url: Url) -> Url {
        url
    }

    fn connect_socket(
        &mut self,
        _host: String,
        _port: u16,
        _timeout: Duration,
        handle: SocketHandle,
        _receiver: Receiver<Vec<u8>>,
        sender: Sender<SocketAction>,
    ) {
        // Sockets are unsupported; tell the AVM the connection failed, mirroring
        // NullNavigatorBackend so AS code that probes a socket gets a clean
        // failure instead of hanging.
        let _ = sender.try_send(SocketAction::Connect(handle, ConnectionState::Failed));
    }
}
