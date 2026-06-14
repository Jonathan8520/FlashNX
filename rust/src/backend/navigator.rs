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
    /// The movie's synthetic base URL, e.g. `http://flashforswitch.local/foo.swf`.
    /// Relative loads are resolved against this.
    base_url: String,
    /// `sdmc:/flashnx/<game-stem>.files` — where this game's sibling SWFs live.
    sidecar_dir: PathBuf,
}

impl SidecarNavigator {
    pub fn new(spawner: NullSpawner, base_url: String, sidecar_dir: PathBuf) -> Self {
        Self {
            spawner,
            base_url,
            sidecar_dir,
        }
    }

    /// Map a resolved request URL to a local sidecar path: take the URL's path
    /// Host-mirrored path: `<sidecar_dir>/<host>/<path-segments>`. Matches the
    /// layout written by `gamezip::extract_gamezip_tree` (the GameZIP's full
    /// `content/<host>/<path>` tree), so a game that loads an asset by its
    /// original absolute URL (e.g. `http://www.fliplineads.com/serve/data/x.xml`)
    /// resolves to the bundled stub. Empty and `..` segments are dropped so a
    /// load can't escape the dir.
    fn local_path(&self, url: &Url) -> PathBuf {
        let mut path = self.sidecar_dir.clone();
        if let Some(host) = url.host_str() {
            path.push(host);
        }
        self.push_segments(&mut path, url);
        path
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
    const DEAD: &[&str] = &["mochiads.com", "mochibot.com", "mochimedia.com"];
    DEAD.iter()
        .any(|d| host == *d || host.ends_with(&std::format!(".{d}")))
}

extern "C" {
    // C++/libnx sidecar reads (cpp/src/swf_picker.cpp). Rust's std::fs::read
    // returns ENOENT for some files that ARE on disk on Horizon (verified
    // 2026-06-14: C++ re-reads every extracted file fine; Rust's std::fs misses
    // a few — same newlib-glue unreliability as the read_dir/metadata bugs).
    fn swf_picker_file_size(path: *const core::ffi::c_char) -> i64;
    fn swf_picker_read_file(path: *const core::ffi::c_char, buf: *mut u8, cap: u64) -> i64;
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
        // Defunct ad/analytics hosts: mimic an unreachable server (hang) rather
        // than erroring. Preloaders that gate on MochiAd & co. fail-open via
        // their own timeout when the ad server is unreachable; our usual
        // immediate "not found" error instead breaks that path and leaves the
        // game stuck on the sponsor / "update Flash" screen (observed on
        // Papa Louie 2's MochiAds preloader). We still never touch the network.
        if resolved.host_str().is_some_and(is_dead_ad_host) {
            tracing::info!("sidecar: stalling dead ad host {resolved} (mimic unreachable)");
            return Box::pin(std::future::pending::<
                Result<Box<dyn SuccessResponse>, ErrorResponse>,
            >());
        }
        // Try the host-mirrored layout first (GameZIP `content/<host>/<path>`
        // tree), then fall back to the flat layout (htdocs-fetched companions
        // sitting directly in `<game>.files/`).
        let host_path = self.local_path(&resolved);
        let (bytes, from) = match read_sidecar_file(&host_path) {
            Some(b) => (b, host_path),
            None => {
                let flat = self.flat_path(&resolved);
                match read_sidecar_file(&flat) {
                    Some(b) => (b, flat),
                    None => {
                        tracing::warn!(
                            "sidecar: {} not found ({}, {})",
                            resolved,
                            host_path.display(),
                            flat.display()
                        );
                        let e = std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "sidecar file not found",
                        );
                        return async_return(Err(create_specific_fetch_error(
                            "Sidecar file not found",
                            resolved.as_str(),
                            e,
                        )));
                    }
                }
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
