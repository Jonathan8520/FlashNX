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
    /// segments (everything after the host) and join them under `sidecar_dir`.
    /// Empty and `..` segments are dropped so a load can't escape the dir.
    fn local_path(&self, url: &Url) -> PathBuf {
        let mut path = self.sidecar_dir.clone();
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
        path
    }
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
        let path = self.local_path(&resolved);
        match std::fs::read(&path) {
            Ok(bytes) => {
                tracing::info!(
                    "sidecar: served {} ({} bytes) from {}",
                    resolved,
                    bytes.len(),
                    path.display()
                );
                let resp: Box<dyn SuccessResponse> = Box::new(SidecarResponse {
                    url: resolved.to_string(),
                    bytes: Some(bytes),
                });
                async_return(Ok(resp))
            }
            Err(e) => {
                tracing::warn!("sidecar: {} not found at {} ({e})", resolved, path.display());
                async_return(Err(create_specific_fetch_error(
                    "Sidecar file not found",
                    resolved.as_str(),
                    e,
                )))
            }
        }
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
