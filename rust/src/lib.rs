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
}

static mut STATE: Option<State> = None;

const VIEWPORT_W: u32 = 1280;
const VIEWPORT_H: u32 = 720;
const TEST_SWF_PATH: &str = "sdmc:/switch/ruffle/test.swf";

/// Embedded fallback: a 43-byte SWF that just sets a red stage background.
/// Pulled from the upstream ruffle tree as a known-good reproducible target
/// for Phase 1.5.b validation.
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

    // Try to load a real SWF from the SD card. If missing, fall back to
    // the embedded test SWF (a 43-byte red-background movie) so we always
    // have something for the Player to play.
    let (movie_bytes, source_label): (std::vec::Vec<u8>, std::string::String) =
        match std::fs::read(TEST_SWF_PATH) {
            Ok(b) => {
                log_str(&std::format!(
                    "ruffle_init: read {} bytes from {}\n",
                    b.len(),
                    TEST_SWF_PATH,
                ));
                (b, std::format!("file://{}", TEST_SWF_PATH))
            }
            Err(e) => {
                log_str(&std::format!(
                    "ruffle_init: no {} ({}), using embedded fallback ({} bytes)\n",
                    TEST_SWF_PATH,
                    e,
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
        STATE = Some(State { player });
    }
    0
}

#[no_mangle]
pub extern "C" fn ruffle_render_frame() {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };

    // Fixed 60 Hz tick. Real frame pacing comes in Phase 1.5+ once we
    // bind to `appletGetFocusState`/`gfx_swap` timing.
    let dt = FloatDuration::from_secs(1.0 / 60.0);
    let mut player = match state.player.lock() {
        Ok(p) => p,
        Err(_) => return,
    };
    player.tick(dt);
    player.render();
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
