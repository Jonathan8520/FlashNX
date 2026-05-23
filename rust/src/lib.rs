//! flash-for-switch — Ruffle Flash player port to Nintendo Switch.
//!
//! Phase 0:   ruffle_render_frame = glClear red.
//! Phase 0.5: GLSL triangle.
//! Phase 1.1: stdlib via -Z build-std on the tier-3 target.
//! Phase 1.2: ruffle_core links.
//! Phase 1.3:   SwitchRenderBackend full pipeline — shapes, bitmaps, lines,
//!              color transforms, gradients, masking.

#![feature(restricted_std)]

mod backend;
mod ffi;
mod player;

use core::ffi::{c_char, c_int};

use ruffle_render::backend::RenderBackend;
use ruffle_render::bitmap::{Bitmap, BitmapFormat, BitmapHandle, BitmapSize, BitmapSource};
use ruffle_render::commands::{CommandHandler, CommandList};
use ruffle_render::matrix::Matrix;
use ruffle_render::shape_utils::{DistilledShape, DrawCommand, DrawPath, FillRule};
use ruffle_render::transform::Transform;
use swf::{
    Color, ColorTransform, Fixed16, Fixed8, GradientInterpolation, GradientRecord, GradientSpread,
    Point, Rectangle, Twips,
};

use backend::render::SwitchRenderBackend;

extern "C" {
    fn ruffle_log_cstr(msg: *const c_char);
}

struct State {
    renderer: SwitchRenderBackend,
    /// Yellow triangle shape (1.3.2 demo, still drawn).
    demo_triangle: ruffle_render::backend::ShapeHandle,
    /// Shape with a horizontal linear gradient fill.
    demo_lin_gradient: ruffle_render::backend::ShapeHandle,
    /// Shape with a radial gradient fill.
    demo_rad_gradient: ruffle_render::backend::ShapeHandle,
    /// Solid cyan shape, drawn with a non-identity ColorTransform to
    /// demonstrate `u_mult`/`u_add`.
    demo_tinted_shape: ruffle_render::backend::ShapeHandle,
    /// Shape used as a mask (small square) for the orange rect below.
    demo_mask: ruffle_render::backend::ShapeHandle,
    /// 16x16 procedural checkerboard.
    demo_bitmap: BitmapHandle,
}

static mut STATE: Option<State> = None;

#[no_mangle]
pub extern "C" fn ruffle_init() -> c_int {
    let mut renderer = match SwitchRenderBackend::new(1280, 720) {
        Some(r) => r,
        None => {
            log(b"ruffle_init: SwitchRenderBackend::new failed\n\0");
            return -1;
        }
    };

    let banner = std::format!(
        "phase 1.3 complete: renderer={} ({})\n",
        renderer.name(),
        renderer.debug_info(),
    );
    let mut bytes: std::vec::Vec<u8> = banner.into_bytes();
    bytes.push(0);
    unsafe { ruffle_log_cstr(bytes.as_ptr() as *const c_char) };

    // Keep exercising the PlayerBuilder FFI surface.
    {
        let throwaway: std::boxed::Box<dyn RenderBackend> =
            std::boxed::Box::new(SwitchRenderBackend::new(1, 1).expect("throwaway backend"));
        let _builder = ruffle_core::PlayerBuilder::new().with_boxed_renderer(throwaway);
    }

    let demo_triangle = register_triangle_shape(&mut renderer);
    let demo_lin_gradient = register_linear_gradient_rect(&mut renderer);
    let demo_rad_gradient = register_radial_gradient_rect(&mut renderer);
    let demo_tinted_shape = register_solid_rect(&mut renderer, Color::from_rgb(0x00BCD4, 255));
    let demo_mask = register_solid_rect(&mut renderer, Color::WHITE);
    let demo_bitmap = match register_checkerboard(&mut renderer) {
        Ok(b) => b,
        Err(_) => {
            log(b"ruffle_init: register_checkerboard failed\n\0");
            return -2;
        }
    };

    unsafe {
        STATE = Some(State {
            renderer,
            demo_triangle,
            demo_lin_gradient,
            demo_rad_gradient,
            demo_tinted_shape,
            demo_mask,
            demo_bitmap,
        });
    }

    log(b"ruffle_init: all demo resources registered\n\0");
    0
}

/// Yellow triangle (shape-local pixels), reused from Phase 1.3.2.
fn register_triangle_shape(renderer: &mut SwitchRenderBackend) -> ruffle_render::backend::ShapeHandle {
    let p = |x: f64, y: f64| Point { x: Twips::from_pixels(x), y: Twips::from_pixels(y) };
    let fill = swf::FillStyle::Color(Color::from_rgb(0xFFC107, 255));
    let cmds = std::vec![
        DrawCommand::MoveTo(p(900.0, 500.0)),
        DrawCommand::LineTo(p(1100.0, 620.0)),
        DrawCommand::LineTo(p(900.0, 620.0)),
        DrawCommand::LineTo(p(900.0, 500.0)),
    ];
    let path = DrawPath::Fill {
        style: &fill,
        commands: cmds,
        winding_rule: FillRule::EvenOdd,
    };
    let bounds = Rectangle {
        x_min: Twips::from_pixels(900.0),
        y_min: Twips::from_pixels(500.0),
        x_max: Twips::from_pixels(1100.0),
        y_max: Twips::from_pixels(620.0),
    };
    renderer.register_shape(
        DistilledShape {
            paths: std::vec![path],
            shape_bounds: bounds,
            edge_bounds: bounds,
            id: 0,
        },
        &NullBitmapSource,
    )
}

/// 200x100 rect filled with `color`, drawn at shape-local origin.
fn register_solid_rect(
    renderer: &mut SwitchRenderBackend,
    color: Color,
) -> ruffle_render::backend::ShapeHandle {
    let p = |x: f64, y: f64| Point { x: Twips::from_pixels(x), y: Twips::from_pixels(y) };
    let fill = swf::FillStyle::Color(color);
    let cmds = std::vec![
        DrawCommand::MoveTo(p(0.0, 0.0)),
        DrawCommand::LineTo(p(200.0, 0.0)),
        DrawCommand::LineTo(p(200.0, 100.0)),
        DrawCommand::LineTo(p(0.0, 100.0)),
        DrawCommand::LineTo(p(0.0, 0.0)),
    ];
    let path = DrawPath::Fill {
        style: &fill,
        commands: cmds,
        winding_rule: FillRule::EvenOdd,
    };
    let bounds = Rectangle {
        x_min: Twips::ZERO,
        y_min: Twips::ZERO,
        x_max: Twips::from_pixels(200.0),
        y_max: Twips::from_pixels(100.0),
    };
    renderer.register_shape(
        DistilledShape {
            paths: std::vec![path],
            shape_bounds: bounds,
            edge_bounds: bounds,
            id: 0,
        },
        &NullBitmapSource,
    )
}

/// 200x100 rect with a horizontal linear gradient (red → blue).
fn register_linear_gradient_rect(
    renderer: &mut SwitchRenderBackend,
) -> ruffle_render::backend::ShapeHandle {
    let p = |x: f64, y: f64| Point { x: Twips::from_pixels(x), y: Twips::from_pixels(y) };
    // The SWF gradient unit box spans 32768 twips (-16384..16384). For our
    // 200x100 px shape (4000x2000 twips) we want the gradient to fit
    // horizontally → scale matrix.a so 32768 unit twips map to 4000 shape
    // twips → factor 4000/32768 ≈ 0.1221. Centred via tx = 2000 twips
    // (middle of shape).
    let scale = 4000.0 / 32768.0;
    let gradient = swf::Gradient {
        matrix: swf::Matrix {
            a: Fixed16::from_f32(scale as f32),
            b: Fixed16::ZERO,
            c: Fixed16::ZERO,
            d: Fixed16::from_f32((2000.0 / 32768.0) as f32),
            tx: Twips::from_pixels(100.0),
            ty: Twips::from_pixels(50.0),
        },
        spread: GradientSpread::Pad,
        interpolation: GradientInterpolation::Rgb,
        records: std::vec![
            GradientRecord { ratio: 0,   color: Color::from_rgb(0xFF1744, 255) },
            GradientRecord { ratio: 255, color: Color::from_rgb(0x2962FF, 255) },
        ],
    };
    let fill = swf::FillStyle::LinearGradient(gradient);
    let cmds = std::vec![
        DrawCommand::MoveTo(p(0.0, 0.0)),
        DrawCommand::LineTo(p(200.0, 0.0)),
        DrawCommand::LineTo(p(200.0, 100.0)),
        DrawCommand::LineTo(p(0.0, 100.0)),
        DrawCommand::LineTo(p(0.0, 0.0)),
    ];
    let path = DrawPath::Fill {
        style: &fill,
        commands: cmds,
        winding_rule: FillRule::EvenOdd,
    };
    let bounds = Rectangle {
        x_min: Twips::ZERO,
        y_min: Twips::ZERO,
        x_max: Twips::from_pixels(200.0),
        y_max: Twips::from_pixels(100.0),
    };
    renderer.register_shape(
        DistilledShape {
            paths: std::vec![path],
            shape_bounds: bounds,
            edge_bounds: bounds,
            id: 0,
        },
        &NullBitmapSource,
    )
}

/// 200x100 rect with a radial gradient (white centre → purple edge).
fn register_radial_gradient_rect(
    renderer: &mut SwitchRenderBackend,
) -> ruffle_render::backend::ShapeHandle {
    let p = |x: f64, y: f64| Point { x: Twips::from_pixels(x), y: Twips::from_pixels(y) };
    let gradient = swf::Gradient {
        matrix: swf::Matrix {
            a: Fixed16::from_f32(4000.0 / 32768.0),
            b: Fixed16::ZERO,
            c: Fixed16::ZERO,
            d: Fixed16::from_f32(2000.0 / 32768.0),
            tx: Twips::from_pixels(100.0),
            ty: Twips::from_pixels(50.0),
        },
        spread: GradientSpread::Pad,
        interpolation: GradientInterpolation::Rgb,
        records: std::vec![
            GradientRecord { ratio: 0,   color: Color::from_rgb(0xFFFFFF, 255) },
            GradientRecord { ratio: 255, color: Color::from_rgb(0x6A1B9A, 255) },
        ],
    };
    let fill = swf::FillStyle::RadialGradient(gradient);
    let cmds = std::vec![
        DrawCommand::MoveTo(p(0.0, 0.0)),
        DrawCommand::LineTo(p(200.0, 0.0)),
        DrawCommand::LineTo(p(200.0, 100.0)),
        DrawCommand::LineTo(p(0.0, 100.0)),
        DrawCommand::LineTo(p(0.0, 0.0)),
    ];
    let path = DrawPath::Fill {
        style: &fill,
        commands: cmds,
        winding_rule: FillRule::EvenOdd,
    };
    let bounds = Rectangle {
        x_min: Twips::ZERO,
        y_min: Twips::ZERO,
        x_max: Twips::from_pixels(200.0),
        y_max: Twips::from_pixels(100.0),
    };
    renderer.register_shape(
        DistilledShape {
            paths: std::vec![path],
            shape_bounds: bounds,
            edge_bounds: bounds,
            id: 0,
        },
        &NullBitmapSource,
    )
}

/// Build a 16x16 RGBA checkerboard and register as a bitmap.
fn register_checkerboard(renderer: &mut SwitchRenderBackend) -> Result<BitmapHandle, ()> {
    const SIZE: u32 = 16;
    let mut data = std::vec::Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let cell = (x / 2) ^ (y / 2);
            let (r, g, b) = if cell & 1 == 0 { (255, 255, 255) } else { (40, 40, 40) };
            data.push(r);
            data.push(g);
            data.push(b);
            data.push(255u8);
        }
    }
    let bitmap = Bitmap::new(SIZE, SIZE, BitmapFormat::Rgba, data);
    renderer.register_bitmap(bitmap).map_err(|_| ())
}

/// Empty BitmapSource for solid-fill shapes.
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

    // ─── Layout (1280x720) ─────────────────────────────────────────────────
    // Row 1  (y= 60-160):  3 solid rects (red, green, blue)        [DrawRect]
    // Row 2  (y=200-300):  cyan rect tinted via color transform    [RenderShape]
    //                      horizontal line                          [DrawLine]
    // Row 3  (y=340-440):  checkerboard bitmap (×16 scale)         [RenderBitmap]
    //                      linear gradient rect                     [RenderShape]
    //                      radial gradient rect                     [RenderShape]
    // Row 4  (y=480-620):  masked orange rect (mask = small square) [push/activate/render/deact/pop_mask]
    //                      yellow triangle (top-right)              [RenderShape]
    //                      line rect outline                        [DrawLineRect]

    let mut commands = CommandList::new();

    // Row 1 — 3 solid rects.
    commands.draw_rect(Color::from_rgb(0xE53935, 255), rect_matrix( 80.0, 60.0, 200.0, 100.0));
    commands.draw_rect(Color::from_rgb(0x43A047, 255), rect_matrix(540.0, 60.0, 200.0, 100.0));
    commands.draw_rect(Color::from_rgb(0x1E88E5, 255), rect_matrix(1000.0, 60.0, 200.0, 100.0));

    // Row 2 — color-transformed shape + line.
    commands.render_shape(
        state.demo_tinted_shape.clone(),
        Transform {
            matrix: Matrix {
                a: 1.0, b: 0.0, c: 0.0, d: 1.0,
                tx: Twips::from_pixels(80.0),
                ty: Twips::from_pixels(200.0),
            },
            color_transform: ColorTransform {
                // Halve every channel except alpha → tinted (darker) cyan.
                r_multiply: Fixed8::from_f32(0.5),
                g_multiply: Fixed8::from_f32(0.5),
                b_multiply: Fixed8::from_f32(0.5),
                a_multiply: Fixed8::ONE,
                r_add: 0,
                g_add: 80, // shift green to bias the tint toward warm
                b_add: 0,
                a_add: 0,
            },
            perspective_projection: None,
        },
    );
    commands.draw_line(
        Color::from_rgb(0xFFEB3B, 255),
        Matrix {
            a: 540.0,   // line length in pixels
            b: 0.0,
            c: 0.0,
            d: 1.0,     // d unused for a horizontal line, but keep finite
            tx: Twips::from_pixels(540.0),
            ty: Twips::from_pixels(250.0),
        },
    );

    // Row 3 — bitmap + gradients.
    // Bitmap: scale a 16x16 source by 8x = 128x128 pixels.
    let bitmap_transform = Transform {
        matrix: Matrix {
            a: 8.0, b: 0.0, c: 0.0, d: 8.0,
            tx: Twips::from_pixels(80.0),
            ty: Twips::from_pixels(340.0),
        },
        ..Default::default()
    };
    commands.render_bitmap(
        state.demo_bitmap.clone(),
        bitmap_transform,
        true,
        ruffle_render::bitmap::PixelSnapping::Auto,
    );
    commands.render_shape(
        state.demo_lin_gradient.clone(),
        Transform {
            matrix: Matrix {
                a: 1.0, b: 0.0, c: 0.0, d: 1.0,
                tx: Twips::from_pixels(280.0),
                ty: Twips::from_pixels(340.0),
            },
            ..Default::default()
        },
    );
    commands.render_shape(
        state.demo_rad_gradient.clone(),
        Transform {
            matrix: Matrix {
                a: 1.0, b: 0.0, c: 0.0, d: 1.0,
                tx: Twips::from_pixels(540.0),
                ty: Twips::from_pixels(340.0),
            },
            ..Default::default()
        },
    );

    // Row 4 — masked rect (orange clipped by a small square mask).
    commands.push_mask();
    // Mask: a 100x80 square at (200, 500). Only this area lets the maskee
    // shine through.
    commands.render_shape(
        state.demo_mask.clone(),
        Transform {
            matrix: Matrix {
                a: 0.5, b: 0.0, c: 0.0, d: 0.8,
                tx: Twips::from_pixels(200.0),
                ty: Twips::from_pixels(500.0),
            },
            ..Default::default()
        },
    );
    commands.activate_mask();
    // Maskee: a big 400x140 orange rect — only the masked region shows.
    commands.draw_rect(
        Color::from_rgb(0xFF6F00, 255),
        rect_matrix(80.0, 480.0, 400.0, 140.0),
    );
    commands.deactivate_mask();
    commands.pop_mask();

    // The yellow triangle (registered with absolute coords) + a line-rect
    // outline around the whole bottom row.
    commands.render_shape(state.demo_triangle.clone(), Transform::default());
    commands.draw_line_rect(
        Color::from_rgb(0xFFFFFF, 255),
        rect_matrix(60.0, 40.0, 1160.0, 600.0),
    );

    state.renderer.submit_frame(
        Color::from_rgb(0x0D0D1A, 255),
        commands,
        std::vec::Vec::new(),
    );
}

/// Build the Flash matrix mapping the unit square (0..1) to a screen-space
/// rect (`x..x+w`, `y..y+h`) in pixels.
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
