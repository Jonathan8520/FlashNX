//! flash-for-switch — Ruffle Flash player port to Nintendo Switch.
//!
//! Compiles as a staticlib that the devkitPro C++ wrapper in ../cpp/
//! links against to produce the final .nro.
//!
//! Phase 0:   ruffle_render_frame = glClear red. Proved Rust→C→libnx/EGL→
//!            switch-mesa→Tegra X1 on real hardware (2026-05-20).
//! Phase 0.5: ruffle_render_frame compiles GLSL + draws an RGB triangle.
//!            Validated on hardware (2026-05-21).
//! Phase 1.1: std is pulled in via -Z build-std on the existing tier-3 target
//!            (its spec already says os=horizon, which stdlib supports via
//!            the 3DS cfg branches). No custom target JSON needed.
//! Phase 1.2: pull ruffle_core, validate PlayerBuilder constructs and drops
//!            without crashing (validated 2026-05-21).
//! Phase 1.3.1: SwitchRenderBackend skeleton — all 15 trait methods
//!            implemented Null-style. Validated on hardware (2026-05-23).
//! Phase 1.3.2: real ShapeTessellator + per-frame CommandList walk in
//!            submit_frame. RenderShape draws cached GPU meshes; DrawRect
//!            draws an immediate-mode unit quad. Other commands no-op with
//!            a one-shot log.

#![feature(restricted_std)]

mod backend;
mod ffi;
mod player;

use core::ffi::{c_char, c_int};

use ruffle_render::backend::RenderBackend;
use ruffle_render::bitmap::{BitmapHandle, BitmapSize, BitmapSource};
use ruffle_render::commands::{CommandHandler, CommandList};
use ruffle_render::matrix::Matrix;
use ruffle_render::shape_utils::{DistilledShape, DrawCommand, DrawPath, FillRule};
use ruffle_render::transform::Transform;
use swf::{Color, Point, Rectangle, Twips};

use backend::render::SwitchRenderBackend;

extern "C" {
    fn ruffle_log_cstr(msg: *const c_char);
}

/// We need stable storage to keep the renderer alive across the
/// per-frame ruffle_render_frame call. Single-threaded by design.
struct State {
    renderer: SwitchRenderBackend,
    /// Cached shape registered once at init for the visual demo.
    demo_shape: ruffle_render::backend::ShapeHandle,
}

static mut STATE: Option<State> = None;

#[no_mangle]
pub extern "C" fn ruffle_init() -> c_int {
    // Construct the renderer. This compiles + links our solid-color shader,
    // builds the unit-quad VBO/VAO, and is the first non-trivial GL activity
    // after the C++ side set up the EGL context.
    let mut renderer = match SwitchRenderBackend::new(1280, 720) {
        Some(r) => r,
        None => {
            log(b"ruffle_init: SwitchRenderBackend::new failed\n\0");
            return -1;
        }
    };

    // Banner.
    let banner = std::format!(
        "phase 1.3.2: renderer={} ({})\n",
        renderer.name(),
        renderer.debug_info(),
    );
    let mut bytes: std::vec::Vec<u8> = banner.into_bytes();
    bytes.push(0);
    unsafe { ruffle_log_cstr(bytes.as_ptr() as *const c_char); }

    // Force-construct a PlayerBuilder + hand the renderer to it so we keep
    // exercising the FFI surface Ruffle expects. We drop the builder
    // immediately and recover the renderer separately for our manual demo
    // — until Phase 1.5 wires up a real Player::tick() loop, manual
    // submission stays our test harness.
    {
        let throwaway_renderer: std::boxed::Box<dyn RenderBackend> =
            std::boxed::Box::new(SwitchRenderBackend::new(1, 1).expect("second backend"));
        let _builder = ruffle_core::PlayerBuilder::new().with_boxed_renderer(throwaway_renderer);
    }

    // Build a small triangle DistilledShape and register it. This exercises
    // ShapeTessellator + the upload path on hardware — the first time
    // lyon has run on Switch in this project.
    let demo_shape = register_demo_shape(&mut renderer);

    unsafe {
        STATE = Some(State { renderer, demo_shape });
    }

    log(b"ruffle_init: SwitchRenderBackend ready, demo shape registered\n\0");
    0
}

/// Synthesise a yellow triangle and register it. Returns the ShapeHandle
/// to draw via RenderShape every frame.
fn register_demo_shape(renderer: &mut SwitchRenderBackend) -> ruffle_render::backend::ShapeHandle {
    // Triangle in pixel coords: (640, 200), (840, 500), (440, 500).
    // (i.e. roughly the center of a 1280x720 viewport, pointing up).
    let p = |x_px: f64, y_px: f64| Point {
        x: Twips::from_pixels(x_px),
        y: Twips::from_pixels(y_px),
    };
    let fill_style = swf::FillStyle::Color(Color::from_rgb(0xFFC107, 255));
    let commands = std::vec![
        DrawCommand::MoveTo(p(640.0, 200.0)),
        DrawCommand::LineTo(p(840.0, 500.0)),
        DrawCommand::LineTo(p(440.0, 500.0)),
        DrawCommand::LineTo(p(640.0, 200.0)),
    ];
    let path = DrawPath::Fill {
        style: &fill_style,
        commands,
        winding_rule: FillRule::EvenOdd,
    };
    let bounds = Rectangle {
        x_min: Twips::from_pixels(440.0),
        y_min: Twips::from_pixels(200.0),
        x_max: Twips::from_pixels(840.0),
        y_max: Twips::from_pixels(500.0),
    };
    let shape = DistilledShape {
        paths: std::vec![path],
        shape_bounds: bounds,
        edge_bounds: bounds,
        id: 0,
    };
    renderer.register_shape(shape, &NullBitmapSource)
}

/// Empty BitmapSource — fine for solid-fill shapes.
struct NullBitmapSource;
impl BitmapSource for NullBitmapSource {
    fn bitmap_size(&self, _id: u16) -> Option<BitmapSize> { None }
    fn bitmap_handle(&self, _id: u16, _renderer: &mut dyn RenderBackend) -> Option<BitmapHandle> { None }
}

#[no_mangle]
pub extern "C" fn ruffle_render_frame() {
    let state = unsafe {
        match (*core::ptr::addr_of_mut!(STATE)).as_mut() {
            Some(s) => s,
            None => return,
        }
    };

    // Build a small CommandList:
    //   - clear to dark navy
    //   - 3 DrawRects across the top (red, green, blue) of width 200, height 100
    //   - 1 RenderShape: the yellow triangle we registered at init
    let mut commands = CommandList::new();
    commands.draw_rect(Color::from_rgb(0xE53935, 255), rect_matrix( 80.0, 60.0, 200.0, 100.0));
    commands.draw_rect(Color::from_rgb(0x43A047, 255), rect_matrix(540.0, 60.0, 200.0, 100.0));
    commands.draw_rect(Color::from_rgb(0x1E88E5, 255), rect_matrix(1000.0, 60.0, 200.0, 100.0));
    commands.render_shape(state.demo_shape.clone(), Transform::default());

    state.renderer.submit_frame(
        Color::from_rgb(0x0D0D1A, 255),
        commands,
        std::vec::Vec::new(),
    );
}

/// Build the Flash matrix that maps a unit (0..1) square to a screen-space
/// rect of `(w, h)` pixels translated to `(x, y)`.
fn rect_matrix(x: f64, y: f64, w: f64, h: f64) -> Matrix {
    Matrix {
        a: w as f32,
        b: 0.0,
        c: 0.0,
        d: h as f32,
        tx: Twips::from_pixels(x),
        ty: Twips::from_pixels(y),
    }
}

#[no_mangle]
pub extern "C" fn ruffle_shutdown() {
    unsafe {
        STATE = None;
    }
}

// getrandom 0.3 custom backend. Phase 1.2 stub: xorshift seeded from a static
// counter. Insecure but enough for game RNG. Phase 2 will route to libnx
// `csrngGetRandomBytes` for real entropy via Switch's `csrng` service.
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
    unsafe {
        ruffle_log_cstr(msg_nul.as_ptr() as *const c_char);
    }
}
