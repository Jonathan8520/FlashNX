//! Whole screens drawn from the kit's bricks, one file each.
//!
//! `render.rs` grew one `draw_library_*` function per screen, each with its own
//! copy of the layout code; a screen rebuilt on the kit moves here instead.
//! These are children of `render` for the same reason the kit is.

pub(crate) mod importer;
pub(crate) mod settings;
pub(crate) mod windows;
