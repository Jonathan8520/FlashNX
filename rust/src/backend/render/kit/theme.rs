//! Every colour, size and gap the kit draws with, in one place.
//!
//! The launcher grew one screen at a time, and each screen spelled its own
//! numbers: the selection amber was written two different ways on five screens,
//! and two lists meant to look identical sat 2 px apart. A screen built from the
//! kit takes its values from here, so changing the look is one edit.
//!
//! Colours are 0xAARRGGBB, the format `draw_overlay_rect` already takes.
//! The palette is the launcher's own navy and gold ("B" on the redesign canvas):
//! `BG` is exactly the `library_clear` colour.

pub(crate) mod col {
    /// Page background. Same as `library_clear`'s glClearColor.
    pub const BG: u32 = 0xFF_14_20_38;
    /// Raised panel: side sheets, the help box.
    pub const PANEL: u32 = 0xFF_1A_27_42;
    /// A cover tile before its art arrives, a quiet row fill.
    pub const TILE: u32 = 0xFF_1E_2C_4A;
    /// The active chip, a focused row.
    pub const CHIP: u32 = 0xFF_28_3A_5E;
    /// Hairlines between bands.
    pub const LINE: u32 = 0xFF_22_32_4F;
    /// Background of the small L / R / ZL / ZR pills beside the tabs.
    pub const PILL: u32 = 0xFF_2E_40_66;
    pub const PILL_INK: u32 = 0xFF_B9_C6_E4;
    pub const TEXT: u32 = 0xFF_EE_F2_FF;
    pub const TEXT2: u32 = 0xFF_A8_B6_D6;
    pub const TEXT3: u32 = 0xFF_80_90_B5;
    /// Button glyphs (A, B, +...). The glyph is a filled shape with the letter
    /// cut out, so the page shows through the letter.
    pub const GLYPH: u32 = 0xFF_EE_F2_FF;
    /// The one accent: focus, the active tab, the favourite star.
    pub const GOLD: u32 = 0xFF_FF_D7_40;
}

/// Type sizes in pixels per em. The console font has a single weight, so
/// hierarchy comes from these and from colour, never from bold.
pub(crate) mod size {
    pub const TITLE: f32 = 26.0;
    pub const TAB: f32 = 19.0;
    pub const BODY: f32 = 17.0;
    pub const HINT: f32 = 16.0;
    pub const LABEL: f32 = 15.0;
    pub const CAPTION: f32 = 13.0;
    /// Button glyphs in a hint bar. Larger than the label beside them because
    /// the glyph's circle fills most of its em, where a letter fills about 70%.
    pub const GLYPH: f32 = 24.0;
    /// L / R / ZL / ZR inside their pills.
    pub const PILL: f32 = 12.0;
}

pub(crate) mod radius {
    pub const TILE: f32 = 12.0;
    pub const PILL: f32 = 7.0;
    pub const BAR: f32 = 2.0;
}

/// The focus ring around a tile: a gap the colour of the page, then the ring,
/// so the ring reads against any cover.
pub(crate) mod ring {
    pub const GAP: f32 = 3.0;
    pub const WIDTH: f32 = 3.0;
}
