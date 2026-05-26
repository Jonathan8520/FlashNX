//! flash-for-switch — Ruffle Flash player port to Nintendo Switch.
//!
//! Phase 0:    glClear red.
//! Phase 0.5:  GLSL triangle.
//! Phase 1.1:  stdlib via -Z build-std + 2 patches.
//! Phase 1.2:  ruffle_core links.
//! Phase 1.3:  full SwitchRenderBackend (shapes, bitmaps, lines, gradients,
//!             color transforms, masking).
//! Phase 1.4:  wire SwitchLogBackend and default Null backends for nav/ui/
//!             storage/audio/video into PlayerBuilder.
//! Phase 1.5:  build a real Player, tick + render each frame, and try to
//!             load `sdmc:/switch/ruffle/test.swf` if present.

#![feature(restricted_std)]

mod backend;
mod ffi;
mod keymap;
mod library;
mod menu;
mod player;

use core::ffi::{c_char, c_int};
use std::sync::{Arc, Mutex, OnceLock};

use ruffle_core::events::{
    KeyDescriptor, KeyLocation, LogicalKey, MouseButton, NamedKey, PhysicalKey, PlayerEvent,
};
use ruffle_core::tag_utils::SwfMovie;
use ruffle_core::{FloatDuration, Player, PlayerBuilder};
use ruffle_render::backend::RenderBackend;

use backend::audio::SwitchAudioBackend;
use backend::log::SwitchLogBackend;
use backend::render::SwitchRenderBackend;
use backend::storage::SwitchStorageBackend;
use backend::tracing::SwitchTracingSubscriber;

extern "C" {
    fn ruffle_log_cstr(msg: *const c_char);
    fn ruffle_query_ram(used_out: *mut u64, total_out: *mut u64) -> c_int;
    /// Writes `msg` to `sdmc:/switch/ruffle-crash.log` AND nxlink stdout, then
    /// sleeps ~150 ms so the TCP buffer drains before abort() races us. Used
    /// only from the panic hook.
    fn ruffle_crash_dump(msg: *const c_char);
    /// Monotonic system tick counter (armGetSystemTick). Used to profile
    /// tick() vs render() time per frame.
    fn ruffle_tick_now() -> u64;
    /// System tick frequency in Hz (~19.2 MHz on Switch).
    fn ruffle_tick_freq() -> u64;
}

/// Per-frame tick/render time accumulators. Cleared by the render backend's
/// heartbeat code once per 60 frames. Stored as system-tick counts (not
/// ns/us) so we don't lose precision on each frame's addition.
pub(crate) static TICK_TICKS_ACCUM: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static RENDER_TICKS_ACCUM: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Snapshot of process RAM in bytes (used, total). Returns (0,0) if the
/// underlying svcGetInfo call fails.
pub(crate) fn query_ram() -> (u64, u64) {
    let mut used = 0u64;
    let mut total = 0u64;
    let rc = unsafe { ruffle_query_ram(&mut used as *mut _, &mut total as *mut _) };
    if rc == 0 { (used, total) } else { (0, 0) }
}

struct State {
    player: Arc<Mutex<Player>>,
    /// Last reported cursor position in screen pixels. We track it so we can
    /// (a) overlay a visible crosshair after `Player::render()`, and (b) send
    /// it as the click position when `ruffle_handle_mouse_button` fires
    /// without a preceding move (e.g. touch tap).
    cursor_x: f32,
    cursor_y: f32,
    /// Last reported mouse-button state (left only for now). Used purely to
    /// tint the cursor overlay so the user gets feedback on click.
    cursor_clicked: bool,
}

static mut STATE: Option<State> = None;

const VIEWPORT_W: u32 = 1280;
const VIEWPORT_H: u32 = 720;

/// Candidate paths tried in order. We use a hardcoded list because
/// `std::fs::read_dir` on Horizon corrupts entry names — observed
/// 2026-05-24 on nightly stdlib: a 23-char filename came back missing its
/// first 2 bytes. Suspected dirent struct-layout mismatch between Rust's
/// Unix `dirent` model and devkitPro's newlib (alignment of d_reclen/d_type
/// vs d_name on aarch64). Until 1.5.c writes a libnx-direct file picker
/// (which avoids stdlib's dir reading entirely), we look for known names.
const SWF_CANDIDATES: &[&str] = &[
    "sdmc:/ruffle/test.swf",
    "sdmc:/ruffle/mario.swf",
    "sdmc:/ruffle/Super_Mario_63_2010.swf",
    "sdmc:/switch/ruffle/test.swf",
];

/// Path supplied at runtime by `ruffle_set_swf_path` (Phase 2.6) — C++ scans
/// the SD card via libnx fsdev, picks the first `.swf` it finds, and stores
/// the absolute path here. `find_and_load_swf` consults this first so we
/// support any filename the user drops in `sdmc:/ruffle/` without rebuilding
/// the candidates list.
static OVERRIDE_SWF_PATH: OnceLock<std::string::String> = OnceLock::new();

/// Raw SWF bytes + synthesized URL, populated by the first successful
/// `find_and_load_swf` and reused on every subsequent re-init (e.g. when the
/// pause-menu's REDEMARRER tears the Player down and rebuilds it).
///
/// **Why**: Mario 63 is 15.3 MB. The first `std::fs::read` succeeds on a
/// fresh heap, but after several minutes of play the newlib heap fragments
/// (gc_arena does a lot of small allocs that scatter free blocks). When we
/// drop the Player on restart, the big SwfMovie chunk frees but the heap
/// can't satisfy another 15+ MB contiguous request — `read_to_end` reports
/// `OutOfMemory` and we'd fall back to the 43-byte embedded red screen.
/// Caching the bytes once at first boot bypasses re-reading entirely, so
/// restart only needs the smaller transient SwfMovie::from_data
/// decompression buffer (~25 MB) which fits comfortably in the headroom
/// freed by dropping the old Player.
static CACHED_SWF: OnceLock<(std::vec::Vec<u8>, std::string::String)> = OnceLock::new();

/// Called from C++ (cpp/src/swf_picker.cpp) before `ruffle_init`. Copies the
/// path into a Rust-owned String. Idempotent — only the first call sticks.
/// Returns 0 on success, non-zero on malformed input.
#[no_mangle]
pub extern "C" fn ruffle_set_swf_path(path: *const c_char) -> c_int {
    if path.is_null() {
        return -1;
    }
    // SAFETY: caller (swf_picker.cpp) passes a NUL-terminated C string. We
    // copy immediately so the caller's buffer can be freed.
    let s = unsafe { core::ffi::CStr::from_ptr(path) };
    let Ok(string) = s.to_str() else {
        return -2;
    };
    let _ = OVERRIDE_SWF_PATH.set(std::string::String::from(string));
    0
}

/// Embedded fallback: a 43-byte SWF that just sets a red stage background.
/// Pulled from the upstream ruffle tree as a known-good reproducible target
/// when no `.swf` is found on the SD card.
const EMBEDDED_FALLBACK_SWF: &[u8] =
    include_bytes!("../../third_party/ruffle/swf/tests/swfs/SimpleRedBackground.swf");

#[no_mangle]
pub extern "C" fn ruffle_init() -> c_int {
    // Pipe panics through nxlink so we don't die silently. `panic = "abort"`
    // means the hook fires once, then we're done — but at least the message
    // makes it out. We snapshot RAM at the moment of the crash too: lets us
    // distinguish a logic bug (Mario 63 unimplemented filter etc) from a
    // genuine OOM kill where the headroom collapsed in the last few frames.
    std::panic::set_hook(std::boxed::Box::new(|info| {
        let (used, total) = query_ram();
        let location = info
            .location()
            .map(|l| std::format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| std::string::String::from("<unknown>"));
        // Try to recover a string message (str or String); ignore anything
        // else (e.g. a custom panic payload).
        let payload = info.payload();
        let payload_msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
            *s
        } else if let Some(s) = payload.downcast_ref::<std::string::String>() {
            s.as_str()
        } else {
            "<non-string panic payload>"
        };
        let free_mb = total.saturating_sub(used) / (1024 * 1024);
        let used_mb = used / (1024 * 1024);
        let total_mb = total / (1024 * 1024);
        let msg = std::format!(
            "\n=== PANIC ===\nat {}\nmsg: {}\nram: used={}MB total={}MB free={}MB\n=============\n",
            location, payload_msg, used_mb, total_mb, free_mb,
        );
        let mut bytes = msg.into_bytes();
        bytes.push(0);
        // Use crash_dump (file + stdout + 150 ms sleep) so the message
        // survives the imminent abort() — plain ruffle_log_cstr previously
        // got swallowed by the kernel TCP buffer when the process died
        // before nxlink finished sending.
        unsafe { ruffle_crash_dump(bytes.as_ptr() as *const c_char) };
    }));

    log_str(&std::format!("phase 1.5: ruffle_init starting\n"));

    let _ = tracing::subscriber::set_global_default(SwitchTracingSubscriber::new());
    log(b"ruffle_init: tracing subscriber installed (INFO level)\n\0");

    let renderer = match SwitchRenderBackend::new(VIEWPORT_W, VIEWPORT_H) {
        Some(r) => r,
        None => {
            log(b"ruffle_init: SwitchRenderBackend::new failed\n\0");
            return -1;
        }
    };
    log(b"ruffle_init: renderer constructed\n\0");

    // SharedObject persistence (Phase 2.4.bis). Stored next to the game on
    // SD so the user can manage saves alongside the SWFs (e.g. drop in a
    // `.sol` exported from Ruffle desktop / Flash Player on PC, or back up
    // progression). Full layout under here is `<host>/<swf_path>/<name>.sol`
    // — Ruffle builds that key string itself, our backend just appends
    // `.sol` and joins with this base.
    let storage_path = std::path::PathBuf::from("sdmc:/ruffle/saves");
    let storage = SwitchStorageBackend::new(storage_path);

    let mut builder = PlayerBuilder::new()
        .with_boxed_renderer(std::boxed::Box::new(renderer) as std::boxed::Box<dyn RenderBackend>)
        .with_audio(SwitchAudioBackend::new())
        .with_log(SwitchLogBackend::new())
        .with_storage(std::boxed::Box::new(storage))
        .with_autoplay(true)
        .with_viewport_dimensions(VIEWPORT_W, VIEWPORT_H, 1.0);
    log(b"ruffle_init: audio + storage backends constructed\n\0");

    // Look for a SWF on the SD card. First call populates `CACHED_SWF` so
    // subsequent ruffle_init invocations (e.g. menu REDEMARRER) skip the
    // expensive `std::fs::read` — see CACHED_SWF docs for the OOM reason.
    let (movie_bytes, source_label) = ensure_swf_loaded();

    // Load the user's keymap (sidecar → default → hardcoded fallback). Uses
    // the SWF basename to find a per-game sidecar like
    // `sdmc:/ruffle/Super_Mario_63_2010.swf.keymap.json`. Idempotent across
    // restarts so REDEMARRER doesn't reload a different keymap mid-session.
    let basename = source_label
        .rsplit('/')
        .next()
        .unwrap_or("unknown.swf");
    keymap::init_for_swf(basename);

    match SwfMovie::from_data(movie_bytes, source_label.clone(), None) {
        Ok(movie) => {
            log_str(&std::format!(
                "ruffle_init: SwfMovie parsed (version={}, dims={}x{})\n",
                movie.version(),
                movie.width().to_pixels(),
                movie.height().to_pixels(),
            ));
            builder = builder.with_movie(movie);
        }
        Err(e) => {
            log_str(&std::format!(
                "ruffle_init: SwfMovie::from_data failed: {}\n",
                e
            ));
        }
    }

    log(b"ruffle_init: calling PlayerBuilder::build()\n\0");
    let player = builder.build();
    log(b"ruffle_init: PlayerBuilder::build() returned\n\0");

    unsafe {
        STATE = Some(State {
            player,
            cursor_x: VIEWPORT_W as f32 * 0.5,
            cursor_y: VIEWPORT_H as f32 * 0.5,
            cursor_clicked: false,
        });
    }
    0
}

/// Return a reference to the cached `(bytes, url)`, loading from disk on the
/// first call only. On every subsequent call (post-restart) returns the same
/// data without touching the SD or growing a fresh 15+ MB Vec — see
/// `CACHED_SWF` docs for why that matters.
fn ensure_swf_loaded() -> &'static (std::vec::Vec<u8>, std::string::String) {
    CACHED_SWF.get_or_init(|| {
        match find_and_load_swf_uncached() {
            Some((bytes, path)) => {
                log_str(&std::format!(
                    "ruffle_init: loaded {} bytes from {} (cached for future restarts)\n",
                    bytes.len(),
                    path,
                ));
                // Ruffle's URL parser rejects "sdmc" as an IDN, so we
                // synthesize an http URL keyed by the basename. Stable
                // across restarts → SharedObject paths stay the same.
                let basename = path
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or("movie.swf");
                let url = std::format!("http://flashforswitch.local/{}", basename);
                (bytes, url)
            }
            None => {
                log_str(&std::format!(
                    "ruffle_init: no .swf found on SD, using embedded fallback ({} bytes)\n",
                    EMBEDDED_FALLBACK_SWF.len(),
                ));
                (
                    EMBEDDED_FALLBACK_SWF.to_vec(),
                    std::string::String::from(
                        "http://flashforswitch.local/SimpleRedBackground.swf",
                    ),
                )
            }
        }
    })
}

/// Try the runtime override path (set by C++ swf_picker.cpp via
/// `ruffle_set_swf_path`) first, then fall back to `SWF_CANDIDATES`.
/// Returns the first file we can successfully read. Only called once —
/// `ensure_swf_loaded` memoises the result.
fn find_and_load_swf_uncached() -> Option<(std::vec::Vec<u8>, std::string::String)> {
    if let Some(path) = OVERRIDE_SWF_PATH.get() {
        match std::fs::read(path) {
            Ok(bytes) => {
                log_str(&std::format!("scan: using override path {}\n", path));
                return Some((bytes, path.clone()));
            }
            Err(err) => {
                log_str(&std::format!(
                    "scan: override path {} read failed ({}), falling back to candidates\n",
                    path, err,
                ));
            }
        }
    }
    for path in SWF_CANDIDATES {
        match std::fs::read(path) {
            Ok(bytes) => {
                return Some((bytes, std::string::String::from(*path)));
            }
            Err(err) => {
                log_str(&std::format!("scan: {} not found ({})\n", path, err));
            }
        }
    }
    None
}

#[no_mangle]
pub extern "C" fn ruffle_render_frame() {
    // Back-compat entry: fall back to 1/60 if the C++ side didn't measure
    // elapsed time itself. New main.cpp uses ruffle_render_frame_dt instead.
    render_frame_with_dt(FloatDuration::from_secs(1.0 / 60.0));
}

#[no_mangle]
pub extern "C" fn ruffle_render_frame_dt(dt_us: u64) {
    let dt = FloatDuration::from_secs(dt_us as f64 / 1_000_000.0);
    render_frame_with_dt(dt);
}

fn render_frame_with_dt(dt: FloatDuration) {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };

    let mut player = match state.player.lock() {
        Ok(p) => p,
        Err(_) => return,
    };
    // Profile tick (AVM1 advance + game logic + filter cache) vs render
    // (our backend dispatch: shape/bitmap/gradient draws to GL) so the
    // heartbeat in render.rs can show the breakdown — tells us whether
    // CPU (AVM) or GPU (draws) is the perf bottleneck in any given scene.
    use std::sync::atomic::Ordering;
    let t0 = unsafe { ruffle_tick_now() };
    player.tick(dt);
    let t1 = unsafe { ruffle_tick_now() };
    player.render();
    let t2 = unsafe { ruffle_tick_now() };
    TICK_TICKS_ACCUM.fetch_add(t1.saturating_sub(t0), Ordering::Relaxed);
    RENDER_TICKS_ACCUM.fetch_add(t2.saturating_sub(t1), Ordering::Relaxed);

    // Overlay the cursor crosshair on top of whatever Ruffle drew. We pull
    // a `&mut SwitchRenderBackend` out of the Player by downcasting the
    // trait object — `RenderBackend: Any` so this is just a vtable check.
    let cx = state.cursor_x;
    let cy = state.cursor_y;
    let clicked = state.cursor_clicked;
    let renderer = player.renderer_mut();
    if let Some(backend) =
        <dyn std::any::Any>::downcast_mut::<SwitchRenderBackend>(renderer)
    {
        backend.draw_cursor_overlay(cx, cy, clicked);
    }
}

/// Redraw the current Player state WITHOUT advancing AVM/animation by a
/// time step — used while the pause modal is open so the frame behind the
/// modal stays frozen but doesn't go black (the back buffer would otherwise
/// be stale after a swap). Also redraws the cursor overlay so input still
/// visually responds while paused.
#[no_mangle]
pub extern "C" fn ruffle_redraw_paused() {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    let mut player = match state.player.lock() {
        Ok(p) => p,
        Err(_) => return,
    };
    player.render();
    let cx = state.cursor_x;
    let cy = state.cursor_y;
    let clicked = state.cursor_clicked;
    let renderer = player.renderer_mut();
    if let Some(backend) =
        <dyn std::any::Any>::downcast_mut::<SwitchRenderBackend>(renderer)
    {
        backend.draw_cursor_overlay(cx, cy, clicked);
    }
}

/// Draw the pause-menu overlay on top of whatever's already in the
/// framebuffer. `selected` indexes into `render::MENU_ITEMS`. C++ calls
/// this right after `ruffle_redraw_paused` so the menu sits on top of a
/// frozen game frame, then `gl_context_swap`s.
#[no_mangle]
pub extern "C" fn ruffle_draw_menu(selected: c_int) {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    let Ok(mut player) = state.player.lock() else {
        return;
    };
    let renderer = player.renderer_mut();
    if let Some(backend) =
        <dyn std::any::Any>::downcast_mut::<SwitchRenderBackend>(renderer)
    {
        let idx = selected.max(0) as usize;
        backend.draw_menu_overlay(idx);
    }
}

/// Drop the current Player + renderer (Ruffle owns the SwitchRenderBackend,
/// so its Drop frees VAOs/VBOs/atlases/programs) and re-run `ruffle_init`
/// to load the SWF afresh. The C++-managed GL context stays alive across
/// this call. Used by the pause menu's "REDEMARRER" entry. Returns 0 on
/// success, non-zero on init failure.
#[no_mangle]
pub extern "C" fn ruffle_restart() -> c_int {
    log(b"ruffle_restart: tearing down current Player\n\0");
    unsafe {
        STATE = None;
    }
    log(b"ruffle_restart: re-initialising\n\0");
    ruffle_init()
}

/// Look up the Flash key bound to a Switch button by NAME (e.g. "A",
/// "StickLLeft"). Called from C++ once per binding at boot to fill its
/// runtime BINDINGS array. Returns the matching SK_* code, or `SK_NONE` if
/// the button is unbound in the active keymap. The name must be one of the
/// values listed in `keymap::FALLBACK_BINDINGS` (case sensitive). Caller
/// passes a NUL-terminated UTF-8 C string.
#[no_mangle]
pub extern "C" fn ruffle_keymap_lookup(name: *const c_char) -> c_int {
    if name.is_null() {
        return SK_NONE;
    }
    // SAFETY: caller guarantees NUL-terminated UTF-8.
    let s = unsafe { core::ffi::CStr::from_ptr(name) };
    let Ok(button) = s.to_str() else {
        return SK_NONE;
    };
    keymap::lookup(button).unwrap_or(SK_NONE)
}

// ── TOUCHES sub-screen FFI ────────────────────────────────────────────────
//
// Thin wrappers over `menu::*`. C++ owns the pause-main modal (Reprendre /
// Touches / Redemarrer / Quitter); when the user picks "Touches", it calls
// `ruffle_touches_open` and from then on forwards joycon down-edges via
// `ruffle_touches_input` until `ruffle_touches_active` returns 0 again
// (user pressed B to back out). Each frame, C++ calls `ruffle_touches_draw`
// over the frozen-game backdrop, then `ruffle_touches_consume_dirty` to
// know whether to refresh its runtime BINDINGS table.

#[no_mangle]
pub extern "C" fn ruffle_touches_open() {
    menu::open();
}

#[no_mangle]
pub extern "C" fn ruffle_touches_close() {
    menu::close();
}

#[no_mangle]
pub extern "C" fn ruffle_touches_active() -> c_int {
    if menu::is_active() { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn ruffle_touches_input(button_name: *const c_char) -> c_int {
    if button_name.is_null() {
        return 0;
    }
    let s = unsafe { core::ffi::CStr::from_ptr(button_name) };
    let Ok(b) = s.to_str() else { return 0 };
    if menu::input(b) { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn ruffle_touches_consume_dirty() -> c_int {
    if menu::consume_dirty() { 1 } else { 0 }
}

/// Render the active TOUCHES screen on top of whatever's already in the
/// framebuffer. No-op when the sub-screen is inactive. Caller should call
/// `ruffle_redraw_paused` first so a frozen game frame sits underneath.
#[no_mangle]
pub extern "C" fn ruffle_touches_draw() {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    let Ok(mut player) = state.player.lock() else { return };
    let renderer = player.renderer_mut();
    if let Some(backend) =
        <dyn std::any::Any>::downcast_mut::<SwitchRenderBackend>(renderer)
    {
        menu::draw(backend);
    }
}

// ── Library boot screen (Phase 3.4) ──────────────────────────────────────
//
// Standalone SwitchRenderBackend used by the pre-Ruffle library UI. Lives
// in its own slot because the Ruffle one is owned by `Player` and doesn't
// exist yet at boot. Once the user picks a game we drop this renderer so
// its GL resources (96 MB arena VBO/IBO, shader programs, banner texture)
// free up before `ruffle_init` builds Ruffle's own.

static LIBRARY_RENDERER: Mutex<Option<SwitchRenderBackend>> = Mutex::new(None);

#[no_mangle]
pub extern "C" fn ruffle_library_init() -> c_int {
    let mut renderer = match SwitchRenderBackend::new(VIEWPORT_W, VIEWPORT_H) {
        Some(r) => r,
        None => {
            log(b"library_init: SwitchRenderBackend::new failed\n\0");
            return -1;
        }
    };
    // Decode the embedded banner PNG and upload as a GL texture. On
    // failure (corrupt asset, OOM) we set tex=0 and the library falls back
    // to ASCII title — no fatal.
    if let Some((rgba, w, h)) = library::decode_banner() {
        let tex = renderer.upload_rgba_texture(&rgba, w, h);
        if tex != 0 {
            library::set_banner_texture(tex, w, h);
        }
    }
    if let Ok(mut slot) = LIBRARY_RENDERER.lock() {
        *slot = Some(renderer);
    }
    log(b"library_init: standalone renderer + banner ready\n\0");
    0
}

/// Push one `.swf` path onto the library's scan list. Called by
/// `swf_picker_run` (cpp/src/swf_picker.cpp) per file. Returns 0 on success.
#[no_mangle]
pub extern "C" fn ruffle_library_add_path(path: *const c_char) -> c_int {
    if path.is_null() {
        return -1;
    }
    let s = unsafe { core::ffi::CStr::from_ptr(path) };
    let Ok(p) = s.to_str() else { return -2 };
    if library::add_path(p) { 0 } else { -3 }
}

/// Transition the library from Inactive → List/Empty. Call after the SD
/// scan has populated all entries.
#[no_mangle]
pub extern "C" fn ruffle_library_open() {
    library::open();
}

#[no_mangle]
pub extern "C" fn ruffle_library_active() -> c_int {
    if library::is_active() { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn ruffle_library_picked() -> c_int {
    if library::picked() { 1 } else { 0 }
}

/// Forward a Switch-button down-edge (e.g. "A", "Up", "Minus") to the
/// library state machine. Returns 1 if consumed, 0 otherwise.
#[no_mangle]
pub extern "C" fn ruffle_library_input(button_name: *const c_char) -> c_int {
    if button_name.is_null() {
        return 0;
    }
    let s = unsafe { core::ffi::CStr::from_ptr(button_name) };
    let Ok(b) = s.to_str() else { return 0 };
    if library::input(b) { 1 } else { 0 }
}

/// Render one library frame to the current GL framebuffer. C++ calls
/// `gl_context_swap` afterwards.
#[no_mangle]
pub extern "C" fn ruffle_library_render() {
    let Ok(mut slot) = LIBRARY_RENDERER.lock() else { return };
    let Some(backend) = slot.as_mut() else { return };
    library::render(backend);
}

/// Copy the selected SWF path into a C-owned buffer. Returns 0 on success.
/// -1 if no path was picked (user quit), -2 if `cap` is too small.
#[no_mangle]
pub extern "C" fn ruffle_library_selected_path(out: *mut c_char, cap: c_int) -> c_int {
    let Some(path) = library::selected_path() else { return -1 };
    let bytes = path.as_bytes();
    let needed = bytes.len() + 1; // +1 for NUL
    if (cap as usize) < needed {
        return -2;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, bytes.len());
        *out.add(bytes.len()) = 0;
    }
    0
}

/// Drop the standalone library renderer so its GL resources (~96 MB arena
/// + shader programs + banner texture) free BEFORE `ruffle_init` builds
/// Ruffle's own. Idempotent.
#[no_mangle]
pub extern "C" fn ruffle_library_shutdown() {
    if let Ok(mut slot) = LIBRARY_RENDERER.lock() {
        if slot.is_some() {
            log(b"library_shutdown: dropping standalone renderer\n\0");
        }
        *slot = None;
    }
}

/// Switch button codes shared with `cpp/src/main.cpp`. Keep these in sync.
/// We map joycon → Flash key events; Mario 63 (and most AS2 Flash games)
/// use Space/Z for jump, Enter for start, arrows for movement.
pub(crate) const SK_NONE: c_int = 0;
pub(crate) const SK_SPACE: c_int = 1;
pub(crate) const SK_ENTER: c_int = 2;
pub(crate) const SK_ESCAPE: c_int = 3;
pub(crate) const SK_LEFT: c_int = 4;
pub(crate) const SK_RIGHT: c_int = 5;
pub(crate) const SK_UP: c_int = 6;
pub(crate) const SK_DOWN: c_int = 7;
pub(crate) const SK_Z: c_int = 8;
pub(crate) const SK_X: c_int = 9;
pub(crate) const SK_SHIFT: c_int = 10;
pub(crate) const SK_P: c_int = 11;

fn key_descriptor(code: c_int) -> Option<KeyDescriptor> {
    let (physical, logical) = match code {
        SK_SPACE => (PhysicalKey::Space, LogicalKey::Character(' ')),
        SK_ENTER => (PhysicalKey::Enter, LogicalKey::Named(NamedKey::Enter)),
        SK_ESCAPE => (PhysicalKey::Escape, LogicalKey::Named(NamedKey::Escape)),
        SK_LEFT => (PhysicalKey::ArrowLeft, LogicalKey::Named(NamedKey::ArrowLeft)),
        SK_RIGHT => (PhysicalKey::ArrowRight, LogicalKey::Named(NamedKey::ArrowRight)),
        SK_UP => (PhysicalKey::ArrowUp, LogicalKey::Named(NamedKey::ArrowUp)),
        SK_DOWN => (PhysicalKey::ArrowDown, LogicalKey::Named(NamedKey::ArrowDown)),
        SK_Z => (PhysicalKey::KeyZ, LogicalKey::Character('z')),
        SK_X => (PhysicalKey::KeyX, LogicalKey::Character('x')),
        SK_SHIFT => (PhysicalKey::ShiftLeft, LogicalKey::Named(NamedKey::Shift)),
        SK_P => (PhysicalKey::KeyP, LogicalKey::Character('p')),
        _ => return None,
    };
    Some(KeyDescriptor {
        physical_key: physical,
        logical_key: logical,
        key_location: KeyLocation::Standard,
    })
}

/// Forward a key event from the C++ side. `code` is one of the `SK_*`
/// constants above. `down = true` for press, `false` for release.
#[no_mangle]
pub extern "C" fn ruffle_handle_key(code: c_int, down: bool) {
    if code == SK_NONE {
        return;
    }
    let Some(key) = key_descriptor(code) else {
        return;
    };
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    let event = if down {
        PlayerEvent::KeyDown { key }
    } else {
        PlayerEvent::KeyUp { key }
    };
    if let Ok(mut p) = state.player.lock() {
        p.handle_event(event);
    }
}

/// Move the virtual cursor to `(x, y)` in screen pixels and forward a
/// `MouseMove` event to the Player. Called from C++ when the right stick
/// is deflected or a touch event happens.
#[no_mangle]
pub extern "C" fn ruffle_handle_mouse_move(x: c_int, y: c_int) {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    let cx = x.clamp(0, VIEWPORT_W as c_int) as f32;
    let cy = y.clamp(0, VIEWPORT_H as c_int) as f32;
    state.cursor_x = cx;
    state.cursor_y = cy;
    if let Ok(mut p) = state.player.lock() {
        p.handle_event(PlayerEvent::MouseMove {
            x: cx as f64,
            y: cy as f64,
        });
    }
}

/// Click / release the left mouse button at the current cursor position.
/// `down = true` for press, `false` for release.
#[no_mangle]
pub extern "C" fn ruffle_handle_mouse_button(down: bool) {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };
    state.cursor_clicked = down;
    let x = state.cursor_x as f64;
    let y = state.cursor_y as f64;
    if let Ok(mut p) = state.player.lock() {
        let event = if down {
            PlayerEvent::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                index: None,
            }
        } else {
            PlayerEvent::MouseUp {
                x,
                y,
                button: MouseButton::Left,
            }
        };
        p.handle_event(event);
    }
}

#[no_mangle]
pub extern "C" fn ruffle_shutdown() {
    unsafe {
        STATE = None;
    }
}

#[no_mangle]
pub unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    use core::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(0x9E3779B97F4A7C15);
    let mut state = SEED.load(Ordering::Relaxed);
    for i in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *dest.add(i) = (state.wrapping_mul(0x2545F4914F6CDD1D) >> 32) as u8;
    }
    SEED.store(state, Ordering::Relaxed);
    Ok(())
}

fn log(msg_nul: &[u8]) {
    unsafe { ruffle_log_cstr(msg_nul.as_ptr() as *const c_char) };
}

fn log_str(s: &str) {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    unsafe { ruffle_log_cstr(bytes.as_ptr() as *const c_char) };
}
