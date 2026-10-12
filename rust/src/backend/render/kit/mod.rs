//! The interface kit: the bricks every launcher screen is meant to be built from.
//!
//! - `theme`: every colour, type size and radius, in one place.
//! - `text`: the console's own fonts at the size they are drawn, and the button
//!   glyphs that replace "A:" in the hint bars.
//! - `shapes`: rounded rectangles, rings and shadows from one shader, and
//!   images with rounded corners.
//! - `icons`: the interface icons (a Lucide subset), drawn by `kit_icon`.
//! - `cover`: cover art in boxes of any shape.
//! - `window`: windows, list rows, hint bars, the in-game side sheet.
//! - `list`: a list that scrolls, with its pill, touch table and scrollbar.
//! - `controls`: what sits on the right of a settings row (a choice with every
//!   option shown, a value whose arrows show only while its row is armed, a
//!   switch), chips and the help box.
//! - `sheet`: the side panel (in-game pause, the game options), and the
//!   cascade that opens a sub-panel beside its parent.
//! - `pane`: a panel's rows inside any rectangle (the Réglages pane): sections,
//!   gliding pill, scroll, help box.
//!
//! These are children of `render` so they can reach the renderer's GL state the
//! way the rest of `render.rs` does. Screens move onto them one at a time; the
//! old 5x7 helpers stay until the last screen using them has moved.

// Bricks land before every screen that will use them, so some sit unused for a
// while; the warnings would only bury real ones.
#![allow(dead_code)]

pub(crate) mod controls;
pub(crate) mod cover;
pub(crate) mod icons;
pub(crate) mod list;
pub(crate) mod pane;
pub(crate) mod shapes;
pub(crate) mod sheet;
pub(crate) mod text;
pub(crate) mod theme;
pub(crate) mod window;
