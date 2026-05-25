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

    // Look for a SWF on the SD card. We scan each search dir in order and
    // take the first `.swf` we find. If nothing turns up, we use the
    // embedded red-background fallback so the Player always has content.
    let (movie_bytes, source_label): (std::vec::Vec<u8>, std::string::String) =
        match find_and_load_swf() {
            Some((bytes, path)) => {
                log_str(&std::format!(
                    "ruffle_init: loaded {} bytes from {}\n",
                    bytes.len(),
                    path,
                ));
                // We can't use `file://sdmc:/...` — Ruffle's URL parser rejects
                // "sdmc" as an IDN. SharedObject path keying derives from the
                // movie URL, so we synthesize a stable http URL keyed by the
                // basename. This is what Ruffle Web also does for blob URLs.
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
        };
    match SwfMovie::from_data(&movie_bytes, source_label, None) {
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

/// Try the runtime override path (set by C++ swf_picker.cpp via
/// `ruffle_set_swf_path`) first, then fall back to `SWF_CANDIDATES`.
/// Returns the first file we can successfully read.
fn find_and_load_swf() -> Option<(std::vec::Vec<u8>, std::string::String)> {
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

/// Switch button codes shared with `cpp/src/main.cpp`. Keep these in sync.
/// We map joycon → Flash key events; Mario 63 (and most AS2 Flash games)
/// use Space/Z for jump, Enter for start, arrows for movement.
const SK_NONE: c_int = 0;
const SK_SPACE: c_int = 1;
const SK_ENTER: c_int = 2;
const SK_ESCAPE: c_int = 3;
const SK_LEFT: c_int = 4;
const SK_RIGHT: c_int = 5;
const SK_UP: c_int = 6;
const SK_DOWN: c_int = 7;
const SK_Z: c_int = 8;
const SK_X: c_int = 9;
const SK_SHIFT: c_int = 10;

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
