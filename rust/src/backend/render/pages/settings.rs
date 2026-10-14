//! The RÉGLAGES tab, drawn the way the console's own System Settings are: the
//! sections down a column on the left, Quitter FlashNX pinned at its foot, and
//! beside it, past a hairline, the open section's settings, changed where they
//! are shown (Reglages.dc.html).
//!
//! The page it replaces was a flat list in three ad-hoc groups beside a card
//! that only repeated the focused row's name and two lines of help. Every real
//! setting was one more window away, five rows out of nine opened one, and
//! each of those opened over a wiped screen with nothing saying where it came
//! from. Here the column says where you are, the pane holds the settings
//! themselves (the four home layouts as cards, choices with every option on
//! show, switches), and a deeper level (the folder browser, a bug report's
//! game list) opens inside the pane with the path above it.
//!
//! These are the page's bricks. What goes in them, and in what order, is the
//! library's business (`library/settings_tab.rs`): it knows the settings, this
//! file knows where things sit.

use super::super::kit::icons::Icon;
use super::super::kit::list::{Row, ScrollRows};
use super::super::kit::pane::Pane;
use super::super::kit::theme::{col, radius, ring, size};
use super::super::*;

/// The page's geometry, from the mockup: a 290 px column at the 48 px margin,
/// a hairline 32 px past it, the pane over the rest of the width down to the
/// same margin. The body runs from under the tab strip to above the footer.
const NAV_X: f32 = 48.0;
const NAV_W: f32 = 290.0;
const BODY_TOP: f32 = 84.0;
const BODY_BOTTOM: f32 = 652.0;
const NAV_ITEM_H: f32 = 50.0;
const NAV_GAP: f32 = 4.0;
/// Quitter sits at the foot of the column, apart from the sections: it is the
/// one entry that does something at once, and the gap says so.
const NAV_QUIT_Y: f32 = 602.0;
const DIVIDER_X: f32 = 370.0;
const PANE_X: f32 = 403.0;
const PANE_W: f32 = 829.0;
/// A row in one of the pane's blocks (the folder, the nickname...).
const BLOCK_ROW_H: f32 = 58.0;
const BLOCK_RADIUS: f32 = 14.0;
/// Rows of the game defaults, and of the pane's sub-pages.
const PANE_ROW_H: f32 = 48.0;
const LIST_ROW_H: f32 = 50.0;
const GAME_ROW_H: f32 = 56.0;
/// The explanations under a block: one size, one colour, everywhere.
const NOTE_PX: f32 = size::CAPTION + 1.0;
const NOTE_LH: f32 = 20.0;
/// The crumb over a page opened in the pane: its line, and where its words
/// start past the chevron.
const CRUMB_CY: f32 = 96.0;
const CRUMB_TEXT_DX: f32 = 22.0;

/// Glide keys of the page's cursors. One slot each is shared by every list in
/// the app (`eased_list_y`, `eased_list_x`), so these only have to differ from
/// each other and from the lists drawn on the same frame.
const GLIDE_KEY_NAV: u32 = 0x5E10_0000;
const GLIDE_KEY_CARDS: u32 = 0x5E30_0000;
const GLIDE_KEY_BUG_PAGE: u32 = 58;

/// How long a new section takes to settle in the pane, and from how far: the
/// mockup's cursor timing (120 ms). A slide and not a cut, so the eye sees the
/// pane change while the column's cursor moves; short, so a held Down through
/// six sections never waits on it.
const PANE_SLIDE_MS: f32 = 120.0;
const PANE_SLIDE_PX: f32 = 18.0;

/// The pane on screen, and when it arrived: (key, tick). `u32::MAX` before the
/// page has ever been drawn.
static PANE_SLIDE: std::sync::Mutex<(u32, u64)> = std::sync::Mutex::new((u32::MAX, 0));

/// Set while the page is drawn under a window (the controls editor, a
/// question). Nothing glides then: the window above has a cursor of its own,
/// and the shared glide slots would make the two of them snap every frame.
static STILL: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn still() -> bool {
    STILL.load(core::sync::atomic::Ordering::Relaxed)
}

/// The page's focus between its parts (Jonathan: "il manque la transition
/// fluide dans settings en passant de la sidebar gauche au contenu; pareil
/// pour de vue bibliothèque à dossier des jeux"). Each part (the column, the
/// layout cards, a block of rows, the game defaults' rows, the languages)
/// keeps drawing its own cursor under its text and gliding it inside itself.
/// When the cursor goes from one part to another, the old part's cursor goes
/// and the new one's fades in, while a gold outline travels between them over
/// what it crosses, turning from a card's ring into a row's pill on the way:
/// before, one vanished and the other appeared, with nothing in between.
struct FocusHandoff {
    /// The part with the cursor (an owner key below, or a part's glide key),
    /// 0 before any part has had it.
    owner: u32,
    /// Where its cursor is, the tile radius a ring takes round it, and its
    /// look (0 a card's ring, 1 a row's pill).
    rect: (f32, f32, f32, f32),
    radius: f32,
    look: f32,
    /// Where the travel started (rect, radius, look), while it lasts.
    from: Option<((f32, f32, f32, f32), f32, f32)>,
    start: u64,
    /// When a part last said it had the cursor: a part taking it after a
    /// pause (a tab switch, back from a page or a window) arrives at once.
    last_report: u64,
}

static FOCUS: std::sync::Mutex<FocusHandoff> = std::sync::Mutex::new(FocusHandoff {
    owner: 0,
    rect: (0.0, 0.0, 0.0, 0.0),
    radius: 0.0,
    look: 1.0,
    from: None,
    start: 0,
    last_report: 0,
});

/// The parts with keys of their own; the blocks and the game defaults' rows
/// go by their glide key.
const OWNER_NAV: u32 = 0x5E10_0001;
const OWNER_CARDS: u32 = 0x5E30_0001;
const OWNER_LANG: u32 = 0x5E40_0001;
/// How long the focus takes from one part to the next, and how long without a
/// report counts as a pause.
const HANDOFF_MS: f32 = 200.0;
const HANDOFF_GAP_MS: f32 = 150.0;

fn ms_since(tick: u64, now: u64) -> f32 {
    let freq = unsafe { ruffle_tick_freq() }.max(1) as f32;
    now.saturating_sub(tick) as f32 * 1000.0 / freq
}

fn handoff_progress(f: &FocusHandoff, now: u64) -> f32 {
    (ms_since(f.start, now) / HANDOFF_MS).clamp(0.0, 1.0)
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// The travelling focus at progress `p`: rect, tile radius, look.
fn handoff_at(from: ((f32, f32, f32, f32), f32, f32), f: &FocusHandoff, p: f32) -> ((f32, f32, f32, f32), f32, f32) {
    let e = ease_out(p);
    let (r0, rad0, look0) = from;
    (
        (lerp(r0.0, f.rect.0, e), lerp(r0.1, f.rect.1, e), lerp(r0.2, f.rect.2, e), lerp(r0.3, f.rect.3, e)),
        lerp(rad0, f.radius, e),
        lerp(look0, f.look, e),
    )
}

/// How much of its own cursor part `owner` draws: all of it once the focus
/// has arrived, fading in over the end of the travel, none while the focus is
/// with another part (it is the part that has just been left).
fn focus_alpha(owner: u32) -> f32 {
    if still() {
        return 1.0;
    }
    let now = unsafe { ruffle_tick_now() };
    match FOCUS.lock() {
        Ok(f) if f.owner == owner => match f.from {
            None => 1.0,
            Some(_) => {
                let p = handoff_progress(&f, now);
                ((p - 0.45) / 0.55).clamp(0.0, 1.0)
            }
        },
        Ok(_) => 0.0,
        Err(_) => 1.0,
    }
}

/// `owner` has the cursor this frame, on `rect` (with the tile radius a ring
/// takes round a card, and its look). A new owner starts the travel from
/// where the focus is now, unless the last report is too old to travel from.
fn focus_report(owner: u32, rect: (f32, f32, f32, f32), radius: f32, look: f32) {
    let now = unsafe { ruffle_tick_now() };
    let Ok(mut f) = FOCUS.lock() else { return };
    if f.owner != owner {
        let recent = f.owner != 0 && ms_since(f.last_report, now) <= HANDOFF_GAP_MS && !still();
        f.from = if recent {
            // From where it is drawn: mid-travel, from the point it had reached.
            Some(match f.from {
                Some(from) => handoff_at(from, &f, handoff_progress(&f, now)),
                None => (f.rect, f.radius, f.look),
            })
        } else {
            None
        };
        f.start = now;
        f.owner = owner;
    }
    f.rect = rect;
    f.radius = radius;
    f.look = look;
    f.last_report = now;
}

/// `eased_list_y`, or the target itself while the page is still.
fn glide_y(target: f32, key: u32, now: u64) -> f32 {
    if still() { target } else { eased_list_y(target, key, now) }
}

fn glide_x(target: f32, key: u32, now: u64) -> f32 {
    if still() { target } else { eased_list_x(target, key, now) }
}

extern "C" {
    fn flashnx_status(hour: *mut i32, minute: *mut i32, wifi: *mut i32, battery: *mut i32, charging: *mut i32);
}

impl SwitchRenderBackend {
    /// The page's corner of the tab bar, where Jouer shows its count and the
    /// importer its search: the console's time, network and battery, as the
    /// HOME Menu shows them (Jonathan, 2026-10-14). From the right edge: the
    /// battery's charge and its icon, the network, the time. What cannot be
    /// read is left out, the network excepted, which then says offline.
    pub(crate) fn settings_status_corner(&mut self) {
        const CY: f32 = 36.0;
        const ICON: f32 = 18.0;
        const GAP: f32 = 18.0;
        let (mut hour, mut minute, mut wifi, mut battery, mut charging) = (-1, -1, -1, -1, 0);
        unsafe { flashnx_status(&mut hour, &mut minute, &mut wifi, &mut battery, &mut charging) };
        let u = crate::loc_ui::ui();
        let mut x = self.dimensions.width as f32 - 40.0;
        if battery >= 0 {
            // Low as the console says it: red at 15 % and under, unless charging.
            let low = charging == 0 && battery <= 15;
            let ink = if low { col::DANGER } else { col::TEXT3 };
            let t = u.percent.replacen("{}", &battery.to_string(), 1);
            x -= self.kit_measure(&t, size::LABEL);
            self.kit_text_mid(x.round(), CY, size::LABEL, &t, ink);
            let (icon, icon_ink) = match battery {
                _ if charging != 0 => (Icon::BatteryCharging, col::OK),
                0..=15 => (Icon::BatteryWarning, ink),
                16..=33 => (Icon::BatteryLow, ink),
                34..=66 => (Icon::BatteryMedium, ink),
                _ => (Icon::BatteryFull, ink),
            };
            x -= 6.0 + ICON;
            self.kit_icon(x.round(), CY, ICON, icon, icon_ink);
            x -= GAP;
        }
        let net = match wifi {
            0 => Icon::WifiZero,
            1 => Icon::WifiLow,
            2 => Icon::WifiHigh,
            3 => Icon::Wifi,
            _ => Icon::WifiOff,
        };
        x -= ICON;
        self.kit_icon(x.round(), CY, ICON, net, col::TEXT3);
        x -= GAP;
        if hour >= 0 && minute >= 0 {
            let t = std::format!("{}:{:02}", hour, minute);
            x -= self.kit_measure(&t, size::LABEL);
            self.kit_text_mid(x.round(), CY, size::LABEL, &t, col::TEXT3);
        }
    }

    /// A caption over a block of the pane, as the home layouts have theirs
    /// ("Vue de l'accueil"). Returns where the block goes.
    pub(crate) fn settings_caption(&mut self, top: f32, text: &str) -> f32 {
        let c = self.kit_fit(text, size::LABEL, PANE_W);
        self.kit_text_mid(PANE_X, top + 9.0, size::LABEL, &c, col::TEXT2);
        top + 32.0
    }

    /// Draw the page still (`true`, under a window) or alive (`false`).
    pub(crate) fn settings_still(&mut self, on: bool) {
        STILL.store(on, core::sync::atomic::Ordering::Relaxed);
    }

    /// The column of sections and the hairline beside it. `entries` lists the
    /// sections top to bottom with Quitter last; `current` is the open one.
    /// With `focused` the cursor is in the column (the gold pill glides there);
    /// otherwise the open section keeps a quiet fill, the way the console marks
    /// the section a page belongs to. Returns one touch cell per entry.
    pub(crate) fn settings_nav(
        &mut self,
        entries: &[(Icon, &str)],
        current: usize,
        focused: bool,
    ) -> std::vec::Vec<(f32, f32, f32, f32)> {
        let n = entries.len();
        let quit = n.saturating_sub(1);
        let item_y = |i: usize| -> f32 {
            if i == quit {
                NAV_QUIT_Y
            } else {
                BODY_TOP + i as f32 * (NAV_ITEM_H + NAV_GAP)
            }
        };
        let now = unsafe { ruffle_tick_now() };
        if current < n {
            if focused {
                let hy = glide_y(item_y(current), GLIDE_KEY_NAV, now);
                let a = focus_alpha(OWNER_NAV);
                if a < 1.0 {
                    // The section's quiet fill, under a cursor still arriving.
                    self.kit_rrect(NAV_X, item_y(current), NAV_W, NAV_ITEM_H, radius::ROW, col::TILE);
                }
                if a > 0.0 {
                    self.kit_focus_pill_alpha(NAV_X, hy, NAV_W, NAV_ITEM_H, a);
                }
                focus_report(OWNER_NAV, (NAV_X, item_y(current), NAV_W, NAV_ITEM_H), radius::ROW, 1.0);
            } else {
                self.kit_rrect(NAV_X, item_y(current), NAV_W, NAV_ITEM_H, radius::ROW, col::TILE);
            }
        }
        let mut cells = std::vec::Vec::with_capacity(n);
        for (i, (icon, label)) in entries.iter().enumerate() {
            let y = item_y(i);
            let cy = y + NAV_ITEM_H * 0.5;
            let on = i == current;
            // Quitter is quieter than the sections at rest, and red under the
            // cursor: it closes the app on the next press.
            let (icon_ink, ink) = if i == quit {
                if on && focused { (col::DANGER, col::DANGER) } else { (col::TEXT3, col::TEXT3) }
            } else if on && focused {
                (col::GOLD, col::TEXT)
            } else if on {
                (col::TEXT, col::TEXT)
            } else {
                (col::TEXT2, col::TEXT2)
            };
            self.kit_icon(NAV_X + 16.0, cy, 20.0, *icon, icon_ink);
            // A size down before any cut: the Russian "Значения по
            // умолчанию" read "Значения по умолч…" at the column's size, and
            // is whole a size under it.
            let room = NAV_W - 50.0 - 14.0;
            let px = if self.kit_measure(label, size::BODY) <= room { size::BODY } else { size::LABEL };
            let l = self.kit_fit(label, px, room);
            self.kit_text_mid(NAV_X + 50.0, cy, px, &l, ink);
            cells.push((NAV_X, y, NAV_W, NAV_ITEM_H));
        }
        self.draw_overlay_rect(DIVIDER_X, BODY_TOP, 1.0, BODY_BOTTOM - BODY_TOP, col::LINE);
        cells
    }

    /// The focus on its way between two parts of the page, over everything
    /// (see `FocusHandoff`); nothing once it has arrived. `on_section` is
    /// false on a page opened in the pane (the folder browser, a report's
    /// games), whose list has a cursor of its own: the travel is dropped there.
    pub(crate) fn settings_focus_overlay(&mut self, on_section: bool) {
        let now = unsafe { ruffle_tick_now() };
        let drawn = {
            let Ok(mut f) = FOCUS.lock() else { return };
            if !on_section || still() {
                f.from = None;
                if !on_section {
                    f.owner = 0;
                }
                return;
            }
            match f.from {
                Some(from) => {
                    let p = handoff_progress(&f, now);
                    if p >= 1.0 {
                        f.from = None;
                        None
                    } else {
                        Some((handoff_at(from, &f, p), 1.0 - ((p - 0.7) / 0.3).clamp(0.0, 1.0)))
                    }
                }
                None => None,
            }
        };
        if let Some(((r, radius, look), alpha)) = drawn {
            self.kit_focus_outline(r.0, r.1, r.2, r.3, radius, look, alpha);
        }
    }

    /// Start drawing the pane for `key` (a section, or a sub-page): when the
    /// key is new, the pane slides in the last few pixels from the right.
    /// Returns what `settings_pane_end` puts back.
    pub(crate) fn settings_pane_begin(&mut self, key: u32) -> f32 {
        let now = unsafe { ruffle_tick_now() };
        let freq = unsafe { ruffle_tick_freq() }.max(1) as f32;
        let start = match PANE_SLIDE.lock() {
            Ok(mut g) => {
                if g.0 != key {
                    // A tab switch slides the whole page already, and a page
                    // seen for the first time has nothing to slide from: both
                    // arrive as they are.
                    let animate = g.0 != u32::MAX && !tab_slide_active() && !still();
                    *g = (key, if animate { now } else { 0 });
                }
                g.1
            }
            Err(_) => 0,
        };
        let saved = self.ui_translate_x;
        if start != 0 {
            let t = (now.saturating_sub(start) as f32 * 1000.0 / freq / PANE_SLIDE_MS).clamp(0.0, 1.0);
            let e = 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t);
            self.ui_translate_x = saved + (1.0 - e) * PANE_SLIDE_PX;
        }
        saved
    }

    pub(crate) fn settings_pane_end(&mut self, saved: f32) {
        self.ui_translate_x = saved;
    }

    /// The section's title at the top of the pane, and an optional line under
    /// it. Returns where the pane's content starts.
    pub(crate) fn settings_title(&mut self, title: &str, subtitle: Option<&str>) -> f32 {
        let px = size::TITLE - 2.0;
        let t = self.kit_fit(title, px, PANE_W);
        self.kit_text_mid(PANE_X, 102.0, px, &t, col::TEXT);
        match subtitle {
            Some(sub) => {
                let s = self.kit_fit(sub, NOTE_PX, PANE_W);
                self.kit_text_mid(PANE_X, 129.0, NOTE_PX, &s, col::TEXT3);
                146.0
            }
            None => 131.0,
        }
    }

    /// The head of a page opened inside the pane: where it hangs from (a
    /// chevron back and the section's name), its title, and a line saying what
    /// is shown (a path, what to pick), with a hairline under them. Returns
    /// where the list starts.
    pub(crate) fn settings_subpage_title(&mut self, crumb: &str, title: &str, subtitle: &str) -> f32 {
        let px = NOTE_PX;
        self.kit_icon(PANE_X, CRUMB_CY, 14.0, Icon::ChevronLeft, col::TEXT3);
        let c = self.kit_fit(crumb, px, PANE_W - CRUMB_TEXT_DX);
        self.kit_text_mid(PANE_X + CRUMB_TEXT_DX, CRUMB_CY, px, &c, col::TEXT2);
        let tpx = size::TITLE - 2.0;
        let t = self.kit_fit(title, tpx, PANE_W);
        self.kit_text_mid(PANE_X, 124.0, tpx, &t, col::TEXT);
        if !subtitle.is_empty() {
            // Cut in the middle: it is often a path, whose end is the part
            // that says where you are.
            let s = self.kit_fit_mid(subtitle, px, PANE_W);
            self.kit_text_mid(PANE_X, 151.0, px, &s, col::TEXT3);
        }
        self.draw_overlay_rect(PANE_X, 168.0, PANE_W, 1.0, col::LINE);
        176.0
    }

    /// The four home layouts as cards, under `caption`, with each one's name
    /// beneath it and `desc` (what the card under the cursor looks like) under
    /// the names. `in_use` is lifted (tile fill, gold tick in its corner);
    /// `card`, when the cursor is on the cards, gets the gold ring, which
    /// Left and Right walk to any card while the tick stays where it is until
    /// A applies the one under the ring, as on the languages. Returns the
    /// cards' touch cells and where the next block can start.
    pub(crate) fn settings_layouts(
        &mut self,
        top: f32,
        caption: &str,
        in_use: u8,
        card: Option<u8>,
        desc: &str,
    ) -> (std::vec::Vec<(f32, f32, f32, f32)>, f32) {
        const GAP: f32 = 16.0;
        const CARD_H: f32 = 118.0;
        const PAD: f32 = 18.0;
        let cw = ((PANE_W - 3.0 * GAP) / 4.0).floor();
        let c = self.kit_fit(caption, size::LABEL, PANE_W);
        self.kit_text_mid(PANE_X, top + 9.0, size::LABEL, &c, col::TEXT2);
        let y = top + 32.0;
        let cur = (in_use as usize).min(3);
        let now = unsafe { ruffle_tick_now() };
        let mut cells = std::vec::Vec::with_capacity(4);
        let label_cy = y + CARD_H + 20.0;
        for v in 0..4usize {
            let x = PANE_X + v as f32 * (cw + GAP);
            let on = v == cur;
            self.kit_rrect(x, y, cw, CARD_H, BLOCK_RADIUS, if on { col::TILE } else { col::PANEL });
            self.sketch_home_view(x + PAD, y + PAD, cw - 2.0 * PAD, CARD_H - 2.0 * PAD, v as u8);
            // The name with the layout's own icon, centred under the card.
            let icon = match v {
                1 => Icon::List,
                2 => Icon::Strip,
                3 => Icon::Shelf,
                _ => Icon::Grid,
            };
            let ink = if on || card == Some(v as u8) { col::TEXT } else { col::TEXT2 };
            let label = self.kit_fit(crate::loc_ui::view_label(v as u8), size::LABEL + 1.0, cw - 26.0);
            let lw = self.kit_measure(&label, size::LABEL + 1.0);
            let lx = (x + (cw - 16.0 - 8.0 - lw) * 0.5).round();
            self.kit_icon(lx, label_cy, 16.0, icon, ink);
            self.kit_text_mid(lx + 24.0, label_cy, size::LABEL + 1.0, &label, ink);
            cells.push((x, y, cw, CARD_H + 36.0));
        }
        let cx = PANE_X + cur as f32 * (cw + GAP);
        // The ring is the cursor, walked from card to card as the languages'
        // is; the tick stays on the layout in use until A moves it there.
        if let Some(f) = card {
            let fx = PANE_X + (f as usize).min(3) as f32 * (cw + GAP);
            let hx = glide_x(fx, GLIDE_KEY_CARDS, now);
            let a = focus_alpha(OWNER_CARDS);
            if a > 0.0 {
                let ink = super::super::kit::window::alpha_of(col::GOLD, a);
                self.kit_ring(hx, y, cw, CARD_H, BLOCK_RADIUS, ring::GAP, ring::WIDTH, ink);
            }
            focus_report(OWNER_CARDS, (fx, y, cw, CARD_H), BLOCK_RADIUS, 0.0);
        }
        // The tick on the layout in use, over the ring's corner.
        const DISC: f32 = 24.0;
        let (dx, dy) = (cx + cw - DISC + 8.0, y - 8.0);
        self.kit_rrect(dx, dy, DISC, DISC, DISC * 0.5, col::GOLD);
        self.kit_icon(dx + 5.0, dy + DISC * 0.5, 14.0, Icon::Check, 0xFF_1A_14_05);
        let d = self.kit_fit(desc, NOTE_PX, PANE_W);
        self.kit_text_mid(PANE_X, label_cy + 25.0, NOTE_PX, &d, col::TEXT3);
        (cells, label_cy + 35.0)
    }

    /// One layout, drawn in blocks the way the mockup draws it: covers are
    /// rectangles, the chosen cover is gold, text is a bar.
    fn sketch_home_view(&mut self, x: f32, y: f32, w: f32, h: f32, view: u8) {
        const ACCENT: u32 = 0xE6_FF_D7_40;
        const FAINT: u32 = 0xFF_34_46_6B;
        const TEXT_BAR: u32 = 0xFF_4A_5C_85;
        match view {
            // Grille: covers in rows.
            0 => {
                let (cols, rows, g) = (4usize, 2usize, 6.0f32);
                let tw = (w - g * (cols - 1) as f32) / cols as f32;
                let th = (h - g * (rows - 1) as f32) / rows as f32;
                for r in 0..rows {
                    for c in 0..cols {
                        let tint = if r == 0 && c == 0 { ACCENT } else { FAINT };
                        self.kit_rrect(x + c as f32 * (tw + g), y + r as f32 * (th + g), tw, th, 4.0, tint);
                    }
                }
            }
            // Liste: titles down the left, the chosen game's cover beside them.
            1 => {
                let side = (w * 0.44).round();
                let lw = w - side - 10.0;
                let (n, bar, g) = (5usize, 10.0f32, 7.0f32);
                let ly = y + ((h - (n as f32 * bar + (n - 1) as f32 * g)) * 0.5).max(0.0);
                for i in 0..n {
                    self.kit_rrect(x, ly + i as f32 * (bar + g), lw, bar, 3.0, if i == 0 { ACCENT } else { FAINT });
                }
                self.kit_rrect(x + lw + 10.0, y, side, h, 6.0, FAINT);
            }
            // Bande: a large cover with its title, a ribbon of small ones under.
            2 => {
                let ribbon = 22.0;
                let top_h = h - ribbon - 8.0;
                let hero = (w * 0.48).round();
                self.kit_rrect(x, y, hero, top_h, 6.0, FAINT);
                let tx = x + hero + 10.0;
                let tw = w - hero - 10.0;
                self.kit_rrect(tx, y + 4.0, tw * 0.8, 8.0, 3.0, TEXT_BAR);
                self.kit_rrect(tx, y + 18.0, tw * 0.55, 6.0, 3.0, FAINT);
                let (n, g) = (4usize, 6.0f32);
                let cw = (w - g * (n - 1) as f32) / n as f32;
                for i in 0..n {
                    let tint = if i == 0 { ACCENT } else { FAINT };
                    self.kit_rrect(x + i as f32 * (cw + g), y + h - ribbon, cw, ribbon, 4.0, tint);
                }
            }
            // Étagère: one row of covers, the chosen one grown, its title under.
            _ => {
                let bar = 8.0;
                let row_h = h - bar - 10.0;
                let big = (w * 0.36).round();
                let g = 8.0;
                let small = (w - big - 2.0 * g) * 0.5;
                self.kit_rrect(x, y, big, row_h, 6.0, ACCENT);
                let sh = (row_h * 0.72).round();
                self.kit_rrect(x + big + g, y + row_h - sh, small, sh, 6.0, FAINT);
                self.kit_rrect(x + big + 2.0 * g + small, y + row_h - sh, small, sh, 6.0, FAINT);
                self.kit_rrect(x, y + h - bar, w * 0.6, bar, 3.0, TEXT_BAR);
            }
        }
    }

    /// Rows in a raised block, a hairline between two rows, the gold pill on
    /// the row `focus` points at (gliding on `glide_key` from wherever the
    /// pane's cursor was). Returns the rows' touch cells, the block's foot,
    /// and the parts of the rows' controls a finger can tap (`CtrlSegs`, by
    /// row of the block).
    pub(crate) fn settings_block(
        &mut self,
        top: f32,
        rows: &[Row],
        focus: Option<usize>,
        glide_key: u32,
    ) -> (std::vec::Vec<(f32, f32, f32, f32)>, f32, super::super::kit::list::CtrlSegs) {
        let h = BLOCK_ROW_H * rows.len() as f32;
        self.kit_rrect(PANE_X, top, PANE_W, h, BLOCK_RADIUS, col::PANEL);
        for i in 1..rows.len() {
            let ly = (top + i as f32 * BLOCK_ROW_H).round();
            self.draw_overlay_rect(PANE_X + 18.0, ly, PANE_W - 36.0, 1.0, col::LINE);
        }
        if let Some(f) = focus.filter(|f| *f < rows.len()) {
            let now = unsafe { ruffle_tick_now() };
            let hy = glide_y(top + f as f32 * BLOCK_ROW_H, glide_key, now);
            let a = focus_alpha(glide_key);
            if a > 0.0 {
                self.kit_focus_pill_alpha(PANE_X, hy, PANE_W, BLOCK_ROW_H, a);
            }
            focus_report(glide_key, (PANE_X, top + f as f32 * BLOCK_ROW_H, PANE_W, BLOCK_ROW_H), radius::ROW, 1.0);
        }
        let mut cells = std::vec::Vec::with_capacity(rows.len());
        let mut segs = std::vec::Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let y = top + i as f32 * BLOCK_ROW_H;
            let parts = self.kit_row_item(PANE_X, y + BLOCK_ROW_H * 0.5, PANE_W, row, focus == Some(i));
            segs.extend(parts.into_iter().map(|(p, r)| (i, p, r)));
            cells.push((PANE_X, y, PANE_W, BLOCK_ROW_H));
        }
        (cells, top + h, segs)
    }

    /// The explanation under a block: small, quiet, on at most `max_lines`
    /// lines. Returns its foot.
    pub(crate) fn settings_note(&mut self, top: f32, text: &str, max_lines: usize) -> f32 {
        if text.trim().is_empty() {
            return top;
        }
        let lines = self.kit_wrap(text, NOTE_PX, PANE_W - 8.0, max_lines);
        let mut cy = top + NOTE_LH * 0.5;
        for line in &lines {
            self.kit_text_mid(PANE_X + 4.0, cy, NOTE_PX, line, col::TEXT3);
            cy += NOTE_LH;
        }
        top + NOTE_LH * lines.len() as f32
    }

    /// A line led by an icon (where something is, a link). Returns its foot.
    pub(crate) fn settings_note_icon(&mut self, top: f32, icon: Icon, text: &str, ink: u32) -> f32 {
        let cy = top + NOTE_LH * 0.5;
        self.kit_icon(PANE_X + 4.0, cy, 16.0, icon, ink);
        let t = self.kit_fit(text, NOTE_PX, PANE_W - 32.0);
        self.kit_text_mid(PANE_X + 4.0 + 16.0 + 10.0, cy, NOTE_PX, &t, ink);
        top + NOTE_LH
    }

    /// The game defaults: rows in groups that scroll above the box explaining
    /// the row with the cursor, which keeps its room at the foot of the pane.
    /// `selection` is `usize::MAX` while the cursor is in the column. Returns
    /// the rows' touch cells and their controls' parts (`kit_pane_rows`).
    pub(crate) fn settings_rows(
        &mut self,
        top: f32,
        rows: &[Row],
        sections: &[(usize, &str)],
        selection: usize,
        glide_key: u32,
        help: &str,
    ) -> (std::vec::Vec<(f32, f32, f32, f32)>, super::super::kit::list::CtrlSegs) {
        let p = Pane {
            rows,
            sections,
            selection,
            row_h: PANE_ROW_H,
            glide_key,
            help,
            reserve_help: true,
            still: still(),
            focus_alpha: if selection < rows.len() { focus_alpha(glide_key) } else { 0.0 },
        };
        let (cells, segs, focus) = self.kit_pane_rows(&p, PANE_X, top, PANE_W, BODY_BOTTOM);
        if let Some(r) = focus {
            focus_report(glide_key, r, radius::ROW, 1.0);
        }
        (cells, segs)
    }

    /// The languages, three to a row, each named in its own language beside
    /// its two-letter code. A code and not a flag: a flag stands for a
    /// country, and Portuguese, English or Spanish are each spoken under
    /// several. The language in use says so under its name; the cursor (when
    /// `focus` is set) glides in both axes. Returns the tiles' touch cells and
    /// the grid's foot.
    pub(crate) fn settings_languages(
        &mut self,
        top: f32,
        langs: &[(&str, &str, &str)],
        active: usize,
        focus: Option<usize>,
        current_caption: &str,
    ) -> (std::vec::Vec<(f32, f32, f32, f32)>, f32) {
        const COLS: usize = 3;
        const TW: f32 = 250.0;
        const TH: f32 = 88.0;
        const GX: f32 = 16.0;
        const GY: f32 = 12.0;
        const CHIP_W: f32 = 34.0;
        const CHIP_H: f32 = 22.0;
        let pos = |i: usize| -> (f32, f32) {
            (PANE_X + (i % COLS) as f32 * (TW + GX), top + (i / COLS) as f32 * (TH + GY))
        };
        for i in 0..langs.len() {
            let (x, y) = pos(i);
            self.kit_rrect(x, y, TW, TH, radius::TILE, col::PANEL);
        }
        if let Some(f) = focus.filter(|f| *f < langs.len()) {
            let now = unsafe { ruffle_tick_now() };
            let (fx, fy) = pos(f);
            let hx = glide_x(fx, GLIDE_KEY_LANG, now);
            let hy = glide_y(fy, GLIDE_KEY_LANG, now);
            let a = focus_alpha(OWNER_LANG);
            if a > 0.0 {
                self.kit_focus_pill_alpha(hx, hy, TW, TH, a);
            }
            focus_report(OWNER_LANG, (fx, fy, TW, TH), radius::ROW, 1.0);
        }
        let mut cells = std::vec::Vec::with_capacity(langs.len());
        for (i, (code, name, in_ui)) in langs.iter().enumerate() {
            let (x, y) = pos(i);
            let cy = y + TH * 0.5;
            let lit = focus == Some(i) || i == active;
            let chip_x = x + 18.0;
            self.kit_rrect(chip_x, (cy - CHIP_H * 0.5).round(), CHIP_W, CHIP_H, 6.0, col::PILL);
            let cw = self.kit_measure(code, size::CAPTION);
            self.kit_text_mid((chip_x + (CHIP_W - cw) * 0.5).round(), cy, size::CAPTION, code, col::PILL_INK);
            let nx = chip_x + CHIP_W + 14.0;
            let nmax = x + TW - 14.0 - nx;
            let n = self.kit_fit(name, size::ROW, nmax);
            if i == active {
                self.kit_text_mid(nx, cy - 10.0, size::ROW, &n, col::TEXT);
                self.kit_icon(nx, cy + 14.0, 14.0, Icon::CircleCheck, col::OK);
                let c = self.kit_fit(current_caption, size::CAPTION, nmax - 20.0);
                self.kit_text_mid(nx + 20.0, cy + 14.0, size::CAPTION, &c, col::TEXT3);
            } else if !in_ui.is_empty() && in_ui.to_lowercase() != name.to_lowercase() {
                // Its name in the interface's language under its own, where
                // the two differ ("Italiano" is the same word in Spanish).
                self.kit_text_mid(nx, cy - 10.0, size::ROW, &n, if lit { col::TEXT } else { col::TEXT2 });
                let t = self.kit_fit(in_ui, size::CAPTION, nmax);
                self.kit_text_mid(nx, cy + 14.0, size::CAPTION, &t, col::TEXT3);
            } else {
                self.kit_text_mid(nx, cy, size::ROW, &n, if lit { col::TEXT } else { col::TEXT2 });
            }
            cells.push((x, y, TW, TH));
        }
        let rows = langs.len().div_ceil(COLS).max(1);
        (cells, top + rows as f32 * TH + (rows - 1) as f32 * GY)
    }

    /// The head of the About section: the logo, the version, who made it and
    /// under which licence. Returns its foot.
    pub(crate) fn settings_about_header(&mut self, top: f32, logo: (GLuint, u32, u32), version: &str, by: &str) -> f32 {
        const LOGO_H: f32 = 30.0;
        let (tex, lw, lh) = logo;
        if tex != 0 && lw > 0 && lh > 0 {
            let w = lw as f32 * LOGO_H / lh as f32;
            self.draw_textured_rect(PANE_X, top, w, LOGO_H, tex);
        } else {
            self.kit_text_mid(PANE_X, top + LOGO_H * 0.5, size::TITLE, "FlashNX", col::GOLD);
        }
        self.kit_text_mid(PANE_X, top + LOGO_H + 24.0, size::BODY, version, col::TEXT);
        let b = self.kit_fit(by, NOTE_PX, PANE_W);
        self.kit_text_mid(PANE_X, top + LOGO_H + 48.0, NOTE_PX, &b, col::TEXT2);
        top + LOGO_H + 60.0
    }

    /// The folder browser inside the pane: its first `pinned` rows (use this
    /// folder, go up) held above the listing with a hairline under them, the
    /// listing scrolling below. `key` is the list's glide key, new for every
    /// folder.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn settings_dir_list(
        &mut self,
        top: f32,
        rows: &[Row],
        selection: usize,
        scroll: usize,
        pinned: usize,
        visible: usize,
        key: u32,
    ) {
        let scrolls = rows.len() > visible;
        let spec = ScrollRows {
            x: PANE_X,
            w: PANE_W - if scrolls { 16.0 } else { 0.0 },
            top,
            row_h: LIST_ROW_H,
            pill_h: LIST_ROW_H - 4.0,
            pinned,
            visible,
            key,
            scrollbar_x: if scrolls { Some(PANE_X + PANE_W - 10.0) } else { None },
        };
        let (x, w) = (spec.x, spec.w);
        self.kit_scroll_rows(&spec, rows.len(), selection.min(rows.len().saturating_sub(1)), scroll, |s, i, cy, focused| {
            s.kit_row_item(x, cy, w, &rows[i], focused);
        });
        if pinned > 0 && rows.len() > pinned {
            let ly = (top + pinned as f32 * LIST_ROW_H).round();
            self.draw_overlay_rect(x + 12.0, ly, w - 24.0, 1.0, col::LINE);
        }
    }

    /// A bug report's game list inside the pane: each game's cover, its name
    /// and the time played, last played first. `games` is (name, basename,
    /// time played).
    pub(crate) fn settings_game_list(
        &mut self,
        top: f32,
        games: &[(&str, &str, &str)],
        selection: usize,
        scroll: usize,
        visible: usize,
    ) {
        const THUMB_W: f32 = 64.0;
        const THUMB_H: f32 = 40.0;
        let scrolls = games.len() > visible;
        let spec = ScrollRows {
            x: PANE_X,
            w: PANE_W - if scrolls { 16.0 } else { 0.0 },
            top,
            row_h: GAME_ROW_H,
            pill_h: GAME_ROW_H - 4.0,
            pinned: 0,
            visible,
            key: GLIDE_KEY_BUG_PAGE,
            scrollbar_x: if scrolls { Some(PANE_X + PANE_W - 10.0) } else { None },
        };
        let (x, w) = (spec.x, spec.w);
        self.kit_scroll_rows(&spec, games.len(), selection, scroll, |s, i, cy, focused| {
            let (name, basename, played) = games[i];
            let tx = x + 14.0;
            let ty = (cy - THUMB_H * 0.5).round();
            // Covers are decoded once and cached; only the rows on screen ask.
            match s.cover_thumb(basename) {
                // The cover rule, as everywhere: whole over its blur when
                // filling the box would crop too much.
                Some((tex, tw, th, amb)) if tex != 0 => {
                    s.kit_cover(tx, ty, THUMB_W, THUMB_H, tex, tw, th, amb, 1.0);
                }
                _ => {
                    s.kit_rrect(tx, ty, THUMB_W, THUMB_H, 6.0, col::TILE);
                    s.kit_icon(tx + (THUMB_W - 18.0) * 0.5, cy, 18.0, Icon::Gamepad, col::TEXT3);
                }
            }
            // A on a game asks for the description on the Switch keyboard.
            let rx = tx + THUMB_W - 4.0;
            s.kit_row_item(rx, cy, x + w - rx, &Row::plain(name).value(played).keyboard(), focused);
        });
    }

    /// The footer band (the author's credit and the version on the left) and
    /// what the buttons do on the right. Ends the page.
    pub(crate) fn settings_footer(&mut self, hints: &[HintItem]) {
        let vw = self.dimensions.width as f32;
        let vh = self.dimensions.height as f32;
        self.kit_footer_band();
        if !hints.is_empty() {
            self.kit_hints_right(vw - 40.0, vh - 24.0, hints);
        }
        unsafe {
            glUseProgram(0);
            glBindVertexArray(0);
        }
        self.gl_state.invalidate();
    }

    /// Make `cells` the page's touch table, under the screen being drawn, with
    /// `segs` the parts of its controls (rows counted as cells of `cells`).
    pub(crate) fn settings_publish(
        &mut self,
        cells: std::vec::Vec<(f32, f32, f32, f32)>,
        segs: super::super::kit::list::CtrlSegs,
    ) {
        ui_cells_publish(ui_screen_kind(), cells);
        ui_segs_publish(ui_screen_kind(), segs);
    }

    /// A wait over the page, the column still on show: the small loading card
    /// over the pane, saying what is being done.
    pub(crate) fn settings_loading_card(&mut self, label: &str, now: u64) {
        let vw = self.dimensions.width as f32;
        self.draw_loading_card(label, now, DIVIDER_X, vw);
    }

    /// Where a finger takes the crumb `crumb` of `settings_subpage_title`: the
    /// chevron and the words, with room around them for a thumb.
    pub(crate) fn settings_crumb_cell(&mut self, crumb: &str) -> (f32, f32, f32, f32) {
        let c = self.kit_fit(crumb, NOTE_PX, PANE_W - CRUMB_TEXT_DX);
        let w = CRUMB_TEXT_DX + self.kit_measure(&c, NOTE_PX);
        (PANE_X - 12.0, CRUMB_CY - 18.0, w + 24.0, 36.0)
    }

    /// Put `cells` after the `base` rows a page in the pane published for its
    /// list (`ui_cells_set_tail`).
    pub(crate) fn settings_publish_tail(&mut self, base: usize, cells: std::vec::Vec<(f32, f32, f32, f32)>) {
        ui_cells_set_tail(ui_screen_kind(), base, cells);
    }

    /// Moving the library (#79): where from and where to, and a count of what
    /// has gone. No cancel: a half-cancelled rename sweep is worse than a
    /// finished one. `src` and `dest` are shown as they are given.
    pub(crate) fn draw_library_move_page(&mut self, src: &str, dest: &str, done: usize, total: usize) {
        // Empty at 0 of 0 rather than full: a move that has not started should
        // not look finished.
        let frac = if total == 0 { 0.0 } else { (done as f32 / total as f32).clamp(0.0, 1.0) };
        let name = if dest.is_empty() { src.to_string() } else { std::format!("{}  \u{2192}  {}", src, dest) };
        let detail = crate::loc_ui::ui()
            .move_items
            .replacen("{}", &done.to_string(), 1)
            .replacen("{}", &total.to_string(), 1);
        self.draw_progress_page(Icon::FolderOpen, crate::loc::s().games_dir_moving, &name, Some(frac), &detail, &[]);
    }
}
