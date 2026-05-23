//! `SwitchRenderBackend` — Ruffle `RenderBackend` impl backed by switch-mesa GL.
//!
//! Phase 1.3 iteration 1 (2026-05-23):
//!   - Full trait surface implemented (all required methods).
//!   - `submit_frame` honours `clear` (glClearColor + glClear).
//!   - `viewport_dimensions` / `set_viewport_dimensions` carry real state.
//!   - Every other method is a Null-style stub (returns Ok with an empty
//!     handle, or Err::Unimplemented). This is enough for `PlayerBuilder`
//!     to accept us and for `Player::render()` to drive at least the clear
//!     path end-to-end, without crashing on .swf content that registers
//!     shapes or bitmaps.
//!
//! Future iterations (Phase 1.3.2+):
//!   - Walk `CommandList` in submit_frame → tessellated triangles → glDrawArrays.
//!   - Real bitmap upload via glTexImage2D / glTexSubImage2D.
//!   - Real shape compilation (tessellator output → VBO/VAO cache keyed on
//!     ShapeHandle).
//!
//! Pivot path if upstream's wgpu backend ever stabilises on switch-mesa: drop
//! this whole file and use ruffle_render_wgpu instead. Today (2026-05) wgpu's
//! GL backend has known "Mesa-only" caveats so going direct is the safer bet.

use std::borrow::Cow;
use std::num::NonZeroU32;
use std::sync::Arc;

use ruffle_render::backend::{
    BitmapCacheEntry, Context3D, Context3DProfile, PixelBenderOutput, PixelBenderTarget,
    RenderBackend, ShapeHandle, ShapeHandleImpl, ViewportDimensions,
};
use ruffle_render::bitmap::{
    Bitmap, BitmapHandle, BitmapHandleImpl, BitmapSource, PixelRegion, RgbaBufRead, SyncHandle,
};
use ruffle_render::commands::CommandList;
use ruffle_render::error::Error;
use ruffle_render::pixel_bender::{PixelBenderShader, PixelBenderShaderHandle};
use ruffle_render::pixel_bender_support::PixelBenderShaderArgument;
use ruffle_render::quality::StageQuality;
use ruffle_render::shape_utils::DistilledShape;
use swf::Color;

use crate::ffi::gl::*;

#[derive(Clone, Debug)]
struct SwitchBitmapHandle;
impl BitmapHandleImpl for SwitchBitmapHandle {}

#[derive(Clone, Debug)]
struct SwitchShapeHandle;
impl ShapeHandleImpl for SwitchShapeHandle {}

pub struct SwitchRenderBackend {
    dimensions: ViewportDimensions,
}

impl SwitchRenderBackend {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            dimensions: ViewportDimensions {
                width,
                height,
                scale_factor: 1.0,
            },
        }
    }
}

impl RenderBackend for SwitchRenderBackend {
    fn viewport_dimensions(&self) -> ViewportDimensions {
        self.dimensions
    }

    fn set_viewport_dimensions(&mut self, dimensions: ViewportDimensions) {
        self.dimensions = dimensions;
        unsafe {
            glViewport(0, 0, dimensions.width as GLsizei, dimensions.height as GLsizei);
        }
    }

    fn register_shape(
        &mut self,
        _shape: DistilledShape,
        _bitmap_source: &dyn BitmapSource,
    ) -> ShapeHandle {
        ShapeHandle(Arc::new(SwitchShapeHandle))
    }

    fn render_offscreen(
        &mut self,
        _handle: BitmapHandle,
        _commands: CommandList,
        _quality: StageQuality,
        _bounds: PixelRegion,
    ) -> Option<Box<dyn SyncHandle>> {
        None
    }

    fn submit_frame(
        &mut self,
        clear: Color,
        _commands: CommandList,
        _cache_entries: Vec<BitmapCacheEntry>,
    ) {
        unsafe {
            glClearColor(
                clear.r as GLfloat / 255.0,
                clear.g as GLfloat / 255.0,
                clear.b as GLfloat / 255.0,
                clear.a as GLfloat / 255.0,
            );
            glClear(GL_COLOR_BUFFER_BIT);
        }
    }

    fn create_empty_texture(
        &mut self,
        _width: NonZeroU32,
        _height: NonZeroU32,
    ) -> Result<BitmapHandle, Error> {
        Ok(BitmapHandle(Arc::new(SwitchBitmapHandle)))
    }

    fn register_bitmap(&mut self, _bitmap: Bitmap<'_>) -> Result<BitmapHandle, Error> {
        Ok(BitmapHandle(Arc::new(SwitchBitmapHandle)))
    }

    fn update_texture(
        &mut self,
        _handle: &BitmapHandle,
        _bitmap: Bitmap<'_>,
        _region: PixelRegion,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn create_context3d(
        &mut self,
        _profile: Context3DProfile,
    ) -> Result<Box<dyn Context3D>, Error> {
        Err(Error::Unimplemented("createContext3D".into()))
    }

    fn debug_info(&self) -> Cow<'static, str> {
        Cow::Borrowed("Renderer: SwitchRenderBackend (phase 1.3 iter 1, clear-only)")
    }

    fn name(&self) -> &'static str {
        "switch-mesa-gl"
    }

    fn set_quality(&mut self, _quality: StageQuality) {}

    fn compile_pixelbender_shader(
        &mut self,
        _shader: PixelBenderShader,
    ) -> Result<PixelBenderShaderHandle, Error> {
        Err(Error::Unimplemented(
            "Pixel bender shader compilation".into(),
        ))
    }

    fn run_pixelbender_shader(
        &mut self,
        _handle: PixelBenderShaderHandle,
        _arguments: &[PixelBenderShaderArgument],
        _target: &PixelBenderTarget,
    ) -> Result<PixelBenderOutput, Error> {
        Err(Error::Unimplemented("Pixel bender shader".into()))
    }

    fn resolve_sync_handle(
        &mut self,
        _handle: Box<dyn SyncHandle>,
        _with_rgba: RgbaBufRead,
    ) -> Result<(), Error> {
        Err(Error::Unimplemented("Sync handle resolution".into()))
    }
}
