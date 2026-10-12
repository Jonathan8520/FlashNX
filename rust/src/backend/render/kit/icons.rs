//! Interface icons: a subset of the Lucide icon font.
//!
//! Lucide (https://lucide.dev) is an open icon set under the ISC license, and
//! the icons it took from Feather are MIT. Both notices travel with the font in
//! `icons-LICENSE.txt`, next to this file. `icons.ttf` keeps only the icons
//! listed below, as TrueType outlines with no hinting, which comes to about
//! 28 KB in the .nro (the script prints the exact size).
//!
//! Why a font and not images: an outline goes through the same rasterizer as
//! the interface text (`glyphs::raster_gid_px`, ttf-parser feeding
//! ab_glyph_rasterizer), at the size it is drawn. An icon is as crisp at 16 px
//! beside a hint as at 48 px on a tile, takes the colour of the text beside it,
//! and needs no texture file per size.
//!
//! Geometry. Lucide draws on a 24-unit grid, and the font maps that grid onto
//! the em, so the em square IS the icon's box: 1000 units per em, every icon
//! 1000 wide, the baseline on the bottom edge of the box and the ascender on
//! its top edge. Rasterized at `px` pixels per em, an icon fills a px by px
//! box, and Lucide's 2-unit stroke comes out px / 12 wide (2 px at 24 px). The
//! ink keeps a margin of about one grid unit on every side. `raster_gid_px`
//! returns bearings from the baseline, so a bitmap with bearings (x0, y0) and
//! height h goes x0 px right of the box's left edge and px - (y0 + h) px below
//! its top edge.
//!
//! Lookup. `ttf_parser::Face::parse(ICON_FONT, 0)` gives the `Face<'static>`
//! that `raster_gid_px` takes, and `face.glyph_index(icon.codepoint())` the
//! glyph. The code points are in the Private Use Area, the same block as the
//! NintendoExt button glyphs, and the two interleave (`Icon::Gamepad` is
//! U+E0DF, just below `Btn::A` at U+E0E0). So an icon is always looked up in
//! this face: put in a string and sent through the text module's fonts, it
//! would come out as whatever NintendoExt has there, or as nothing.
//!
//! Code points are pinned in the script rather than read from each Lucide
//! release, because Lucide renumbers its font between releases (1128 of the
//! 1521 icons present in both 0.460.0 and 1.55.0 moved). The script finds every
//! glyph by name and writes it at its pinned code point, so updating Lucide
//! rewrites `icons.ttf` and leaves this file alone. The values started as those
//! of lucide-static 1.55.0.
//!
//! To add an icon:
//! 1. Find its name on lucide.dev.
//! 2. Add a line for it to `ICONS` in `scripts/make_icon_font.py`, with `None`
//!    as its code point, and run the script on a lucide-static package (the
//!    commands are at the top of the script). It stops and names a free code
//!    point: write that into the line.
//! 3. Run the script again. It rewrites `icons.ttf`, checks every icon in it,
//!    and prints the `enum Icon` and `impl Icon` blocks: paste them over
//!    everything below `ICON_FONT`.

/// The subset, compiled into the binary.
pub(crate) const ICON_FONT: &[u8] = include_bytes!("icons.ttf");

/// One icon of `ICON_FONT`. Each comment starts with the Lucide name of the
/// icon the variant draws.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Icon {
    /// `gamepad-2`: controls and button mapping.
    Gamepad,
    /// `folder`: a folder of games.
    Folder,
    /// `folder-open`: an open folder.
    FolderOpen,
    /// `star`: favourite.
    Star,
    /// `search`: search the library.
    Search,
    /// `arrow-up-down`: sort order.
    Sort,
    /// `layout-grid`: grid view.
    Grid,
    /// `list`: list view.
    List,
    /// `gallery-horizontal`: strip view.
    Strip,
    /// `library`: shelf view.
    Shelf,
    /// `download`: download a game.
    Download,
    /// `cloud-download`: download from an online source.
    CloudDownload,
    /// `link`: add a game by URL.
    Link,
    /// `archive`: the archive.org source.
    Archive,
    /// `globe`: language.
    Globe,
    /// `user`: profile and nickname.
    User,
    /// `info`: information.
    Info,
    /// `circle-question-mark`: help (formerly `circle-help`).
    Help,
    /// `bug`: report a bug.
    Bug,
    /// `lightbulb`: send a suggestion.
    Lightbulb,
    /// `power`: quit the app.
    Power,
    /// `log-out`: quit the game.
    LogOut,
    /// `play`: play or resume.
    Play,
    /// `pause`: pause the game.
    Pause,
    /// `rotate-ccw`: restart the game.
    Restart,
    /// `monitor`: display settings.
    Monitor,
    /// `rotate-cw-square`: rotation, a quarter turn at a time.
    Rotate,
    /// `zoom-in`: free zoom.
    ZoomIn,
    /// `grid-3x3`: the pixels setting, smooth or sharp bitmaps.
    Pixels,
    /// `tv`: screen filter (scanlines, CRT).
    Tv,
    /// `cpu`: overclock.
    Cpu,
    /// `gauge`: frame rate counter.
    Gauge,
    /// `mouse-pointer-2`: cursor.
    Pointer,
    /// `image`: cover art.
    Image,
    /// `pencil`: rename.
    Pencil,
    /// `trash`: delete (formerly `trash-2`, now an alias of `trash`).
    Trash,
    /// `house`: shortcut on the HOME menu.
    Home,
    /// `copy`: copy a profile.
    Copy,
    /// `share-2`: share.
    Share,
    /// `users`: community profiles.
    Users,
    /// `undo-2`: revert.
    Undo,
    /// `check`: done, a ticked option.
    Check,
    /// `x`: close or cancel.
    Close,
    /// `chevron-right`: next, or opens a submenu.
    ChevronRight,
    /// `chevron-left`: previous, or back.
    ChevronLeft,
    /// `chevron-down`: opens a list.
    ChevronDown,
    /// `keyboard`: keyboard input.
    Keyboard,
    /// `mouse`: mouse input.
    Mouse,
    /// `sliders-horizontal`: preferences.
    Sliders,
    /// `hard-drive`: size on the SD card.
    HardDrive,
    /// `file`: a SWF file.
    File,
    /// `code`: engine.
    Code,
    /// `clock`: play time.
    Clock,
    /// `layers`: a game made of several files.
    Layers,
    /// `sparkles`: something new.
    Sparkles,
    /// `wifi-off`: offline.
    WifiOff,
    /// `triangle-alert`: error or warning.
    Alert,
    /// `circle-check`: success.
    CircleCheck,
    /// `circle-x`: failure.
    CircleX,
    /// `plus`: add.
    Plus,
    /// `folder-plus`: make a new folder.
    FolderPlus,
    /// `corner-left-up`: the folder above.
    ParentFolder,
    /// `calendar`: date added.
    Calendar,
    /// `history`: recently played.
    History,
    /// `arrow-down-a-z`: alphabetical order.
    SortAlpha,
    /// `flame`: most played.
    Flame,
    /// `refresh-cw`: try again.
    Refresh,
    /// `eye`: shown.
    Eye,
    /// `eye-off`: hidden.
    EyeOff,
    /// `compass`: gyroscope cursor.
    Compass,
    /// `funnel`: filter results.
    Filter,
    /// `tag`: a shelf a game is on by label.
    Tag,
    /// `package`: a game archive (GameZIP).
    Package,
    /// `wifi`: online.
    Wifi,
    /// `joystick`: a stick.
    Joystick,
    /// `hourglass`: waiting.
    Hourglass,
    /// `pin`: pinned row.
    Pin,
}

impl Icon {
    /// The icon's code point in `ICON_FONT`.
    pub(crate) fn codepoint(self) -> char {
        match self {
            Icon::Gamepad => '\u{E0DF}',
            Icon::Folder => '\u{E0D7}',
            Icon::FolderOpen => '\u{E247}',
            Icon::Star => '\u{E176}',
            Icon::Search => '\u{E151}',
            Icon::Sort => '\u{E37D}',
            Icon::Grid => '\u{E0FF}',
            Icon::List => '\u{E106}',
            Icon::Strip => '\u{E4CE}',
            Icon::Shelf => '\u{E100}',
            Icon::Download => '\u{E0B2}',
            Icon::CloudDownload => '\u{E089}',
            Icon::Link => '\u{E102}',
            Icon::Archive => '\u{E041}',
            Icon::Globe => '\u{E0E8}',
            Icon::User => '\u{E19F}',
            Icon::Info => '\u{E0F9}',
            Icon::Help => '\u{E082}',
            Icon::Bug => '\u{E20C}',
            Icon::Lightbulb => '\u{E1C2}',
            Icon::Power => '\u{E140}',
            Icon::LogOut => '\u{E10E}',
            Icon::Play => '\u{E13C}',
            Icon::Pause => '\u{E12E}',
            Icon::Restart => '\u{E148}',
            Icon::Monitor => '\u{E11D}',
            Icon::Rotate => '\u{E5D1}',
            Icon::ZoomIn => '\u{E1B6}',
            Icon::Pixels => '\u{E0E9}',
            Icon::Tv => '\u{E195}',
            Icon::Cpu => '\u{E0A9}',
            Icon::Gauge => '\u{E1BF}',
            Icon::Pointer => '\u{E1C3}',
            Icon::Image => '\u{E0F6}',
            Icon::Pencil => '\u{E1F9}',
            Icon::Trash => '\u{E18D}',
            Icon::Home => '\u{E0F5}',
            Icon::Copy => '\u{E09E}',
            Icon::Share => '\u{E156}',
            Icon::Users => '\u{E1A4}',
            Icon::Undo => '\u{E2A1}',
            Icon::Check => '\u{E06C}',
            Icon::Close => '\u{E1B2}',
            Icon::ChevronRight => '\u{E06F}',
            Icon::ChevronLeft => '\u{E06E}',
            Icon::ChevronDown => '\u{E06D}',
            Icon::Keyboard => '\u{E284}',
            Icon::Mouse => '\u{E28E}',
            Icon::Sliders => '\u{E29A}',
            Icon::HardDrive => '\u{E0ED}',
            Icon::File => '\u{E0C0}',
            Icon::Code => '\u{E093}',
            Icon::Clock => '\u{E087}',
            Icon::Layers => '\u{E529}',
            Icon::Sparkles => '\u{E412}',
            Icon::WifiOff => '\u{E1AF}',
            Icon::Alert => '\u{E193}',
            Icon::CircleCheck => '\u{E226}',
            Icon::CircleX => '\u{E084}',
            Icon::Plus => '\u{E13D}',
            Icon::FolderPlus => '\u{E0D9}',
            Icon::ParentFolder => '\u{E0A4}',
            Icon::Calendar => '\u{E063}',
            Icon::History => '\u{E1F5}',
            Icon::SortAlpha => '\u{E415}',
            Icon::Flame => '\u{E0D2}',
            Icon::Refresh => '\u{E145}',
            Icon::Eye => '\u{E0BA}',
            Icon::EyeOff => '\u{E0BB}',
            Icon::Compass => '\u{E09B}',
            Icon::Filter => '\u{E0DC}',
            Icon::Tag => '\u{E17F}',
            Icon::Package => '\u{E129}',
            Icon::Wifi => '\u{E1AE}',
            Icon::Joystick => '\u{E355}',
            Icon::Hourglass => '\u{E296}',
            Icon::Pin => '\u{E259}',
        }
    }
}
