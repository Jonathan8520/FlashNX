//! The interface kit: the bricks every launcher screen is meant to be built from.
//!
//! - `theme`: every colour, type size and radius, in one place.
//! - `text`: the console's own fonts at the size they are drawn, and the button
//!   glyphs that replace "A:" in the hint bars.
//! - `shapes`: rounded rectangles, rings and shadows from one shader.
//!
//! These are children of `render` so they can reach the renderer's GL state the
//! way the rest of `render.rs` does. Screens move onto them one at a time; the
//! old 5x7 helpers stay until the last screen using them has moved.

// Bricks land before every screen that will use them, so some sit unused for a
// while; the warnings would only bury real ones.
#![allow(dead_code)]

pub(crate) mod shapes;
pub(crate) mod text;
pub(crate) mod theme;
