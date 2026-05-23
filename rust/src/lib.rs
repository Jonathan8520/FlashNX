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
use std::sync::{Arc, Mutex};

use ruffle_core::events::{
    KeyDescriptor, KeyLocation, LogicalKey, MouseButton, NamedKey, PhysicalKey, PlayerEvent,
};
use ruffle_core::tag_utils::SwfMovie;
use ruffle_core::{FloatDuration, Player, PlayerBuilder};
use ruffle_render::backend::RenderBackend;

use backend::log::SwitchLogBackend;
use backend::render::SwitchRenderBackend;

extern "C" {
    fn ruffle_log_cstr(msg: *const c_char);
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

/// Embedded fallback: a 43-byte SWF that just sets a red stage background.
/// Pulled from the upstream ruffle tree as a known-good reproducible target
/// when no `.swf` is found on the SD card.
const EMBEDDED_FALLBACK_SWF: &[u8] =
    include_bytes!("../../third_party/ruffle/swf/tests/swfs/SimpleRedBackground.swf");

#[no_mangle]
pub extern "C" fn ruffle_init() -> c_int {
    // Pipe panics through nxlink so we don't die silently. `panic = "abort"`
    // means the hook fires once, then we're done — but at least the message
    // makes it out.
    std::panic::set_hook(std::boxed::Box::new(|info| {
        let msg = std::format!("PANIC: {}\n", info);
        let mut bytes = msg.into_bytes();
        bytes.push(0);
        unsafe { ruffle_log_cstr(bytes.as_ptr() as *const c_char) };
    }));

    log_str(&std::format!("phase 1.5: ruffle_init starting\n"));

    let renderer = match SwitchRenderBackend::new(VIEWPORT_W, VIEWPORT_H) {
        Some(r) => r,
        None => {
            log(b"ruffle_init: SwitchRenderBackend::new failed\n\0");
            return -1;
        }
    };
    log(b"ruffle_init: renderer constructed\n\0");

    let mut builder = PlayerBuilder::new()
        .with_boxed_renderer(std::boxed::Box::new(renderer) as std::boxed::Box<dyn RenderBackend>)
        .with_log(SwitchLogBackend::new())
        .with_autoplay(true)
        .with_viewport_dimensions(VIEWPORT_W, VIEWPORT_H, 1.0);

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
                (bytes, std::format!("file://{}", path))
            }
            None => {
                log_str(&std::format!(
                    "ruffle_init: no .swf found on SD, using embedded fallback ({} bytes)\n",
                    EMBEDDED_FALLBACK_SWF.len(),
                ));
                (
                    EMBEDDED_FALLBACK_SWF.to_vec(),
                    std::string::String::from("embedded://SimpleRedBackground.swf"),
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

/// Try each path in `SWF_CANDIDATES` in order. Returns the first file we
/// can successfully read. Logs each miss so the user can see which paths
/// were tried.
fn find_and_load_swf() -> Option<(std::vec::Vec<u8>, std::string::String)> {
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
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };

    // Fixed 60 Hz tick. Real frame pacing comes later once we bind to
    // `appletGetFocusState`/`gfx_swap` timing.
    let dt = FloatDuration::from_secs(1.0 / 60.0);
    let mut player = match state.player.lock() {
        Ok(p) => p,
        Err(_) => return,
    };
    player.tick(dt);
    player.render();

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
