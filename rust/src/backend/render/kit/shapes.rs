//! Rounded rectangles, rings and soft shadows from one distance-field shader.
//!
//! The launcher's rounded corners were painted: one 1-px sliver per row of the
//! corner, in the PAGE colour, over a square. That has no anti-aliasing, costs a
//! draw call per pixel row, and is wrong over anything that is not the page (the
//! cover picker grew a second variant just to pass a different colour). Here a
//! quad is shaded by its signed distance to the rounded edge, the way Zed's GPUI
//! and most GPU interface toolkits draw boxes, so a corner is smooth over any
//! background and a ring is the same shape with a hollow middle.
//!
//! The shape has its own VAO and VBO holding a static unit quad: `rect_vbo` is
//! rewritten by every text batch, and sharing it is the bug the bitmap quad
//! already taught us about.

use super::super::*;

const SHAPE_VERT: &[u8] = b"#version 330 core\n\
layout(location = 0) in vec2 a_pos;\n\
uniform mat3 u_world;\n\
uniform vec2 u_size;\n\
out vec2 v_local;\n\
void main() {\n\
    vec3 p = u_world * vec3(a_pos, 1.0);\n\
    gl_Position = vec4(p.xy, 0.0, 1.0);\n\
    v_local = (a_pos - 0.5) * u_size;\n\
}\n\0";

// Straight alpha out, to match the library's GL_SRC_ALPHA blend. The border is
// mixed in premultiplied space so a transparent fill does not darken the inside
// edge of a ring.
const SHAPE_FRAG: &[u8] = b"#version 330 core\n\
in vec2 v_local;\n\
out vec4 frag_color;\n\
uniform vec2 u_half;\n\
uniform float u_radius;\n\
uniform vec4 u_fill;\n\
uniform float u_border;\n\
uniform vec4 u_border_col;\n\
uniform float u_soft;\n\
float sd_rrect(vec2 p, vec2 b, float r) {\n\
    vec2 q = abs(p) - b + vec2(r);\n\
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;\n\
}\n\
void main() {\n\
    float d = sd_rrect(v_local, u_half, u_radius);\n\
    if (u_soft > 0.0) {\n\
        float a = 1.0 - smoothstep(-u_soft, u_soft, d);\n\
        frag_color = vec4(u_fill.rgb, u_fill.a * a);\n\
        return;\n\
    }\n\
    float aa = max(fwidth(d), 0.0001);\n\
    float cover = clamp(0.5 - d / aa, 0.0, 1.0);\n\
    vec4 pf = vec4(u_fill.rgb * u_fill.a, u_fill.a);\n\
    if (u_border > 0.0) {\n\
        float inner = clamp(0.5 - (d + u_border) / aa, 0.0, 1.0);\n\
        vec4 pb = vec4(u_border_col.rgb * u_border_col.a, u_border_col.a);\n\
        pf = mix(pb, pf, inner);\n\
    }\n\
    pf *= cover;\n\
    frag_color = pf.a > 0.0001 ? vec4(pf.rgb / pf.a, pf.a) : vec4(0.0);\n\
}\n\0";

pub(crate) struct ShapeProgram {
    program: GLuint,
    u_world: GLint,
    u_size: GLint,
    u_half: GLint,
    u_radius: GLint,
    u_fill: GLint,
    u_border: GLint,
    u_border_col: GLint,
    u_soft: GLint,
    vao: GLuint,
    vbo: GLuint,
}

impl ShapeProgram {
    fn build() -> Option<ShapeProgram> {
        let program = link_program(SHAPE_VERT, SHAPE_FRAG)?;
        #[rustfmt::skip]
        const QUAD: [f32; 12] = [
            0.0, 0.0,  1.0, 0.0,  1.0, 1.0,
            0.0, 0.0,  1.0, 1.0,  0.0, 1.0,
        ];
        let mut vao: GLuint = 0;
        let mut vbo: GLuint = 0;
        unsafe {
            glGenVertexArrays(1, &mut vao);
            glGenBuffers(1, &mut vbo);
            glBindVertexArray(vao);
            glBindBuffer(GL_ARRAY_BUFFER, vbo);
            glBufferData(
                GL_ARRAY_BUFFER,
                core::mem::size_of_val(&QUAD) as GLsizeiptr,
                QUAD.as_ptr() as *const _,
                GL_STATIC_DRAW,
            );
            glEnableVertexAttribArray(0);
            glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 8, core::ptr::null());
            glBindVertexArray(0);
            glBindBuffer(GL_ARRAY_BUFFER, 0);
        }
        Some(ShapeProgram {
            u_world: loc(program, b"u_world\0"),
            u_size: loc(program, b"u_size\0"),
            u_half: loc(program, b"u_half\0"),
            u_radius: loc(program, b"u_radius\0"),
            u_fill: loc(program, b"u_fill\0"),
            u_border: loc(program, b"u_border\0"),
            u_border_col: loc(program, b"u_border_col\0"),
            u_soft: loc(program, b"u_soft\0"),
            program,
            vao,
            vbo,
        })
    }
}

impl Drop for ShapeProgram {
    fn drop(&mut self) {
        unsafe {
            glDeleteBuffers(1, &self.vbo);
            glDeleteVertexArrays(1, &self.vao);
            glDeleteProgram(self.program);
        }
    }
}

fn rgba_f(c: u32) -> [f32; 4] {
    [
        ((c >> 16) & 0xFF) as f32 / 255.0,
        ((c >> 8) & 0xFF) as f32 / 255.0,
        (c & 0xFF) as f32 / 255.0,
        ((c >> 24) & 0xFF) as f32 / 255.0,
    ]
}

impl SwitchRenderBackend {
    /// One shape draw. `(x, y, w, h)` is the shape itself; `pad` grows the quad
    /// around it so a soft edge has room to fade out.
    #[allow(clippy::too_many_arguments)]
    fn kit_shape(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        fill: u32,
        border: f32,
        border_col: u32,
        soft: f32,
    ) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        if !self.kit_shapes_tried {
            self.kit_shapes_tried = true;
            self.kit_shapes = ShapeProgram::build();
            if self.kit_shapes.is_none() {
                log_str("kit: shape shader failed, falling back to square boxes\n");
            }
            self.gl_state.invalidate();
        }
        let Some(sp) = self.kit_shapes.as_ref() else {
            // No shader: square corners rather than nothing.
            if border <= 0.0 && soft <= 0.0 {
                self.draw_overlay_rect(x, y, w, h, fill);
            }
            return;
        };
        let (program, vao) = (sp.program, sp.vao);
        let (u_world, u_size, u_half, u_radius, u_fill, u_border, u_border_col, u_soft) = (
            sp.u_world, sp.u_size, sp.u_half, sp.u_radius, sp.u_fill, sp.u_border, sp.u_border_col, sp.u_soft,
        );
        // One pixel of margin even for a crisp shape, so the anti-aliased edge is
        // not cut by the quad.
        let pad = soft.max(1.0) + 1.0;
        let (qx, qy, qw, qh) = (x - pad, y - pad, w + 2.0 * pad, h + 2.0 * pad);
        let m = Matrix {
            a: qw, b: 0.0, c: 0.0, d: qh,
            tx: swf::Twips::from_pixels(qx as f64),
            ty: swf::Twips::from_pixels(qy as f64),
        };
        let world = self.world_matrix(&m);
        let r = radius.min(w * 0.5).min(h * 0.5).max(0.0);
        let f = rgba_f(fill);
        let b = rgba_f(border_col);
        self.gl_state.use_program(program);
        self.gl_state.bind_vao(vao);
        self.draw_calls_this_window = self.draw_calls_this_window.saturating_add(1);
        unsafe {
            glUniformMatrix3fv(u_world, 1, GL_FALSE, world.as_ptr());
            glUniform2f(u_size, qw, qh);
            glUniform2f(u_half, w * 0.5, h * 0.5);
            glUniform1f(u_radius, r);
            glUniform4f(u_fill, f[0], f[1], f[2], f[3]);
            glUniform1f(u_border, border);
            glUniform4f(u_border_col, b[0], b[1], b[2], b[3]);
            glUniform1f(u_soft, soft);
            glDrawArrays(GL_TRIANGLES, 0, 6);
        }
    }

    /// Filled rounded rectangle.
    pub fn kit_rrect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, fill: u32) {
        self.kit_shape(x, y, w, h, radius, fill, 0.0, 0, 0.0);
    }

    /// A ring OUTSIDE the box `(x, y, w, h)`: `gap` of nothing, then `width` of
    /// `col`. Its corners follow the box's radius outwards, so the ring and the
    /// tile it surrounds stay concentric.
    #[allow(clippy::too_many_arguments)]
    pub fn kit_ring(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, gap: f32, width: f32, col: u32) {
        let o = gap + width;
        self.kit_shape(x - o, y - o, w + 2.0 * o, h + 2.0 * o, radius + o, 0, width, col, 0.0);
    }

    /// Soft shadow under a box: the box's shape blurred by `blur` pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn kit_shadow(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, blur: f32, col: u32) {
        self.kit_shape(x, y, w, h, radius, col, 0.0, 0, blur.max(0.5));
    }
}
