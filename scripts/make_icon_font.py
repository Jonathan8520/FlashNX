#!/usr/bin/env python3
"""Build the launcher's icon font, a subset of Lucide, and print its Rust table.

Writes rust/src/backend/render/kit/icons.ttf and prints, on stdout, the
`enum Icon` and `impl Icon` blocks of rust/src/backend/render/kit/icons.rs.
Progress and the final check go to stderr, so stdout can be redirected.

Lucide (https://lucide.dev) is under the ISC license, and the icons it took
from Feather are MIT; both texts are in icons-LICENSE.txt next to the font.
The npm package `lucide-static` ships the whole set as one TrueType font plus
a map from icon names to code points. In a scratch folder outside the repo:

    npm pack lucide-static
    tar -xzf lucide-static-<version>.tgz
    py -3 <repo>/scripts/make_icon_font.py package/font/lucide.ttf package/font/codepoints.json

The map can be the package's font/codepoints.json, font/info.json or
font/lucide.css. Only fontTools is needed (py -3 -m pip install fonttools).
The same package always gives the same bytes, so a re-run that changes
nothing leaves icons.ttf identical.

To add an icon: find its name on lucide.dev and add a line to ICONS below
with None as its code point. The script stops and names a free code point;
write it into the line, run again, and paste the printed blocks over the ones
in icons.rs.
"""

import argparse
import difflib
import json
import logging
import re
import sys
from collections import namedtuple
from pathlib import Path

from fontTools.subset import Options, Subsetter
from fontTools.ttLib import TTFont
from fontTools.ttLib.tables._c_m_a_p import CmapSubtable

REPO = Path(__file__).resolve().parent.parent
DEFAULT_OUT = REPO / "rust" / "src" / "backend" / "render" / "kit" / "icons.ttf"

# (Rust variant, Lucide name, code point, what the launcher uses it for)
#
# The code points are pinned here on purpose. Lucide renumbers its font from
# one release to the next (1128 of the 1521 icons present in both 0.460.0 and
# 1.55.0 moved), so taking them from whatever package the script is given
# would quietly change the Rust table on every Lucide update, and an icons.rs
# left behind would draw the wrong icons. Each glyph is found by NAME in the
# package and written at the code point given here. These started as the code
# points of lucide-static 1.55.0, so they still match that release's map.
#
# Use the current Lucide name. Lucide keeps an old name working as an alias
# for a while, but an alias is what a later release drops.
ICONS = [
    ("Gamepad", "gamepad-2", 0xE0DF, "controls and button mapping"),
    ("Folder", "folder", 0xE0D7, "a folder of games"),
    ("FolderOpen", "folder-open", 0xE247, "an open folder"),
    ("Star", "star", 0xE176, "favourite"),
    ("Search", "search", 0xE151, "search the library"),
    ("Sort", "arrow-up-down", 0xE37D, "sort order"),
    ("Grid", "layout-grid", 0xE0FF, "grid view"),
    ("List", "list", 0xE106, "list view"),
    ("Strip", "gallery-horizontal", 0xE4CE, "strip view"),
    ("Shelf", "library", 0xE100, "shelf view"),
    ("Download", "download", 0xE0B2, "download a game"),
    ("CloudDownload", "cloud-download", 0xE089, "download from an online source"),
    ("Link", "link", 0xE102, "add a game by URL"),
    ("Archive", "archive", 0xE041, "the archive.org source"),
    ("Globe", "globe", 0xE0E8, "language"),
    ("User", "user", 0xE19F, "profile and nickname"),
    ("Info", "info", 0xE0F9, "information"),
    ("Help", "circle-question-mark", 0xE082, "help (formerly `circle-help`)"),
    ("Bug", "bug", 0xE20C, "report a bug"),
    ("Lightbulb", "lightbulb", 0xE1C2, "send a suggestion"),
    ("Power", "power", 0xE140, "quit the app"),
    ("LogOut", "log-out", 0xE10E, "quit the game"),
    ("Play", "play", 0xE13C, "play or resume"),
    ("Pause", "pause", 0xE12E, "pause the game"),
    ("Restart", "rotate-ccw", 0xE148, "restart the game"),
    ("Monitor", "monitor", 0xE11D, "display settings"),
    ("Rotate", "rotate-cw-square", 0xE5D1, "rotation, a quarter turn at a time"),
    ("ZoomIn", "zoom-in", 0xE1B6, "free zoom"),
    ("Pixels", "grid-3x3", 0xE0E9, "the pixels setting, smooth or sharp bitmaps"),
    ("Tv", "tv", 0xE195, "screen filter (scanlines, CRT)"),
    ("Cpu", "cpu", 0xE0A9, "overclock"),
    ("Gauge", "gauge", 0xE1BF, "frame rate counter"),
    ("Pointer", "mouse-pointer-2", 0xE1C3, "cursor"),
    ("Image", "image", 0xE0F6, "cover art"),
    ("Pencil", "pencil", 0xE1F9, "rename"),
    ("Trash", "trash", 0xE18D, "delete (formerly `trash-2`, now an alias of `trash`)"),
    ("Home", "house", 0xE0F5, "shortcut on the HOME menu"),
    ("Copy", "copy", 0xE09E, "copy a profile"),
    ("Share", "share-2", 0xE156, "share"),
    ("Users", "users", 0xE1A4, "community profiles"),
    ("Undo", "undo-2", 0xE2A1, "revert"),
    ("Check", "check", 0xE06C, "done, a ticked option"),
    ("Close", "x", 0xE1B2, "close or cancel"),
    ("ChevronRight", "chevron-right", 0xE06F, "next, or opens a submenu"),
    ("ChevronLeft", "chevron-left", 0xE06E, "previous, or back"),
    ("ChevronDown", "chevron-down", 0xE06D, "opens a list"),
    ("Keyboard", "keyboard", 0xE284, "keyboard input"),
    ("Mouse", "mouse", 0xE28E, "mouse input"),
    ("Sliders", "sliders-horizontal", 0xE29A, "preferences"),
    ("HardDrive", "hard-drive", 0xE0ED, "size on the SD card"),
    ("File", "file", 0xE0C0, "a SWF file"),
    ("Code", "code", 0xE093, "engine"),
    ("Clock", "clock", 0xE087, "play time"),
    ("Layers", "layers", 0xE529, "a game made of several files"),
    ("Sparkles", "sparkles", 0xE412, "something new"),
    ("WifiOff", "wifi-off", 0xE1AF, "offline"),
    ("Alert", "triangle-alert", 0xE193, "error or warning"),
    ("CircleCheck", "circle-check", 0xE226, "success"),
    ("CircleX", "circle-x", 0xE084, "failure"),
    ("Plus", "plus", 0xE13D, "add"),
    ("FolderPlus", "folder-plus", 0xE0D9, "make a new folder"),
    ("ParentFolder", "corner-left-up", 0xE0A4, "the folder above"),
    ("Calendar", "calendar", 0xE063, "date added"),
    ("History", "history", 0xE1F5, "recently played"),
    ("SortAlpha", "arrow-down-a-z", 0xE415, "alphabetical order"),
    ("Flame", "flame", 0xE0D2, "most played"),
    ("Refresh", "refresh-cw", 0xE145, "try again"),
    ("Eye", "eye", 0xE0BA, "shown"),
    ("EyeOff", "eye-off", 0xE0BB, "hidden"),
    ("Compass", "compass", 0xE09B, "gyroscope cursor"),
    ("Filter", "funnel", 0xE0DC, "filter results"),
    ("Tag", "tag", 0xE17F, "a shelf a game is on by label"),
    ("Package", "package", 0xE129, "a game archive (GameZIP)"),
    ("Wifi", "wifi", 0xE1AE, "online"),
    ("Joystick", "joystick", 0xE355, "a stick"),
    ("Hourglass", "hourglass", 0xE296, "waiting"),
    ("Pin", "pin", 0xE259, "pinned row"),
    ("Volume", "volume-2", 0xE1AB, "interface sounds on"),
    ("VolumeOff", "volume-x", 0xE1AC, "interface sounds off"),
    ("WifiHigh", "wifi-high", 0xE5F7, "Wi-Fi, two bars of three"),
    ("WifiLow", "wifi-low", 0xE5F8, "Wi-Fi, one bar of three"),
    ("WifiZero", "wifi-zero", 0xE5F9, "Wi-Fi, connected but no bar"),
    ("BatteryFull", "battery-full", 0xE055, "battery, well charged"),
    ("BatteryMedium", "battery-medium", 0xE057, "battery, half charged"),
    ("BatteryLow", "battery-low", 0xE056, "battery, low"),
    ("BatteryWarning", "battery-warning", 0xE3AC, "battery, nearly empty"),
    ("BatteryCharging", "battery-charging", 0xE054, "battery, charging"),
]

# Unicode's Private Use Area in the Basic Multilingual Plane. Every pinned code
# point must sit here, which also keeps the cmap a single format 4 subtable.
PUA_FIRST, PUA_LAST = 0xE000, 0xF8FF

# Tables for hinting or text layout. The icons are drawn one glyph at a time
# and unhinted, so none of them is needed. Lucide's GSUB holds ligatures that
# turn the typed name "gamepad-2" into the icon, which is also why its cmap
# maps "-", the digits and the letters; dropping it lets those go too. Tags are
# written without their trailing space ("cvt"), the way the subsetter compares.
DROP_TABLES = ["GSUB", "GPOS", "GDEF", "kern", "fpgm", "prep", "cvt", "gasp",
               "hdmx", "VDMX", "LTSH"]

# One ICONS line matched to its glyph in the full Lucide font.
Row = namedtuple("Row", "variant name codepoint purpose source_codepoint source_glyph")


def load_codepoint_map(path):
    """Icon name to code point, from codepoints.json, info.json or a CSS file."""
    text = path.read_text(encoding="utf-8")
    names = {}
    if path.suffix.lower() == ".json":
        for name, value in json.loads(text).items():
            if isinstance(value, int):
                # codepoints.json: {"activity": 57400, ...}
                names[name] = value
            elif isinstance(value, dict) and "encodedCode" in value:
                # info.json: {"activity": {"encodedCode": "\\e038", ...}, ...}
                names[name] = int(value["encodedCode"].lstrip("\\"), 16)
    else:
        # lucide.css: .icon-activity::before { content: "\e038"; }
        pattern = r'\.icon-([a-z0-9-]+)::?before\s*\{\s*content:\s*"\\([0-9a-fA-F]+)"'
        for name, hexcode in re.findall(pattern, text):
            names[name] = int(hexcode, 16)
    if not names:
        sys.exit(f"error: no icon code points found in {path}")
    return names


def package_version(ttf_path):
    """The lucide-static version, from the package.json above font/, or None."""
    package_json = ttf_path.resolve().parent.parent / "package.json"
    try:
        package = json.loads(package_json.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if not isinstance(package, dict) or package.get("name") != "lucide-static":
        return None
    version = package.get("version")
    return str(version) if version else None


def resolve(font, names):
    """Match every ICONS line to a glyph of the full font.

    Stops with the complete list of problems rather than the first one, so a
    Lucide update that renamed three icons is fixed in one pass.
    """
    cmap = font.getBestCmap()
    glyf = font["glyf"]
    errors = []
    rows = []
    unpinned = []
    seen = {"variant": {}, "name": {}, "codepoint": {}, "glyph": {}}

    def claim(kind, key, owner):
        if key in seen[kind]:
            errors.append(f"{owner} and {seen[kind][key]} share the same {kind}")
        seen[kind][key] = owner

    for variant, name, codepoint, purpose in ICONS:
        claim("variant", variant, variant)
        claim("name", name, variant)
        if name not in names:
            close = difflib.get_close_matches(name, list(names), n=4, cutoff=0.6)
            hint = f" (close: {', '.join(close)})" if close else ""
            errors.append(f"{variant}: no icon named `{name}` in the map{hint}")
            continue
        source_codepoint = names[name]
        glyph_name = cmap.get(source_codepoint)
        if glyph_name is None or font.getGlyphID(glyph_name) == 0:
            errors.append(f"{variant}: `{name}` (U+{source_codepoint:04X}) is in the map "
                          "but not in this font")
            continue
        glyph = glyf[glyph_name]
        if glyph.numberOfContours < 0:
            errors.append(f"{variant}: `{name}` is a composite glyph, which this script "
                          "does not handle")
            continue
        if glyph.numberOfContours == 0:
            errors.append(f"{variant}: `{name}` has no outline in this font")
            continue
        claim("glyph", glyph_name, variant)
        if codepoint is None:
            unpinned.append((variant, name, source_codepoint))
            continue
        if not PUA_FIRST <= codepoint <= PUA_LAST:
            errors.append(f"{variant}: U+{codepoint:04X} is outside the Private Use Area "
                          f"U+{PUA_FIRST:04X}..U+{PUA_LAST:04X}")
            continue
        claim("codepoint", codepoint, variant)
        rows.append(Row(variant, name, codepoint, purpose, source_codepoint, glyph_name))

    # A new icon takes the code point its Lucide release gives it when no other
    # line uses that one, else the first free one after the highest in use.
    taken = set(seen["codepoint"])
    for variant, name, source_codepoint in unpinned:
        if PUA_FIRST <= source_codepoint <= PUA_LAST and source_codepoint not in taken:
            free = source_codepoint
        else:
            free = max(taken | {PUA_FIRST - 1}) + 1
        taken.add(free)
        errors.append(f"{variant}: no code point yet; write 0x{free:04X} in its ICONS line")

    if errors:
        sys.exit("error: ICONS does not match this Lucide package:\n  " + "\n  ".join(errors))
    return rows


def build(source_path, rows, out_path, version):
    """Subset the full font down to `rows` and write it to `out_path`."""
    # recalcTimestamp=False keeps head.modified from Lucide's font, which is
    # what makes two runs on the same package produce the same bytes.
    font = TTFont(source_path, recalcBBoxes=True, recalcTimestamp=False)

    options = Options()
    options.layout_features = []
    options.hinting = False
    options.glyph_names = True
    options.recalc_bounds = True
    options.notdef_outline = False
    options.drop_tables += DROP_TABLES
    subsetter = Subsetter(options=options)
    subsetter.populate(unicodes=[row.source_codepoint for row in rows])
    subsetter.subset(font)

    # Name each glyph after its icon, in ICONS order. Lucide's font still
    # carries names from before icons were renamed (the house is "home", the
    # clock "clock-4"), and the cmap keys every table by glyph name, so a
    # rename has to touch glyf, hmtx and the glyph order together.
    old_order = font.getGlyphOrder()
    rename = {old_order[0]: ".notdef"}
    rename.update({row.source_glyph: row.name for row in rows})
    if set(old_order) != set(rename):
        extra = sorted(set(old_order) - set(rename))
        sys.exit(f"error: the subset pulled in glyphs nobody asked for: {extra}")
    new_order = [".notdef"] + [row.name for row in rows]
    glyf = font["glyf"]
    hmtx = font["hmtx"]
    glyf.glyphs = {rename[name]: glyph for name, glyph in glyf.glyphs.items()}
    hmtx.metrics = {rename[name]: metric for name, metric in hmtx.metrics.items()}
    font.setGlyphOrder(new_order)
    post = font["post"]
    post.formatType = 2.0
    post.extraNames = []
    post.mapping = {}

    # A fresh cmap holding only the pinned code points. The subsetter's would
    # keep Lucide's current numbers, which are not ours once Lucide moves them.
    mapping = {row.codepoint: row.name for row in rows}
    cmap = font["cmap"]
    cmap.tables = []
    for platform_id, encoding_id in ((0, 3), (3, 1)):
        table = CmapSubtable.newSubtable(4)
        table.platformID = platform_id
        table.platEncID = encoding_id
        table.language = 0
        table.cmap = dict(mapping)
        cmap.tables.append(table)

    # Lucide's font stores every glyph box with its minimum at (0, 0) and every
    # left side bearing as 0, whatever the outline. Real boxes, with each side
    # bearing equal to its box's xMin as TrueType expects, leave every icon
    # where it was in any rasterizer (FreeType places a glyph by xMin minus the
    # side bearing) and make ttf-parser's glyph_bounding_box, which reads the
    # stored box, tell the truth.
    for name in new_order:
        glyph = glyf[name]
        advance, _ = hmtx[name]
        if glyph.numberOfContours:
            glyph.recalcBounds(glyf)
            hmtx[name] = (advance, glyph.xMin)
        else:
            hmtx[name] = (advance, 0)

    os2 = font["OS/2"]
    os2.usFirstCharIndex = min(mapping)
    os2.usLastCharIndex = max(mapping)
    os2.recalcUnicodeRanges(font)

    name_table = font["name"]
    source = f"Lucide {version}" if version else "Lucide"
    name_table.setName(f"Subset of {source} (https://lucide.dev): the icons the "
                       "FlashNX launcher draws.", 10, 3, 1, 0x409)
    name_table.setName("ISC License; the icons Lucide took from Feather are MIT. "
                       "Full texts in icons-LICENSE.txt.", 13, 3, 1, 0x409)

    font.save(out_path)


def verify(out_path, source_font, rows):
    """Reopen the written font and check it is what icons.rs will expect."""
    font = TTFont(out_path)
    problems = []
    present = {tag.strip() for tag in font.keys()}
    if "glyf" not in present or "CFF" in present or "CFF2" in present:
        problems.append("outlines are not TrueType glyf")
    for tag in DROP_TABLES:
        if tag in present:
            problems.append(f"table {tag} is still there")
    cmap = font.getBestCmap()
    expected = {row.codepoint: row.name for row in rows}
    if cmap != expected:
        problems.append(f"cmap holds {len(cmap)} entries, expected exactly the {len(expected)} icons")
    if font["maxp"].numGlyphs != len(rows) + 1:
        problems.append(f"{font['maxp'].numGlyphs} glyphs, expected {len(rows) + 1}")
    glyf = font["glyf"]
    hmtx = font["hmtx"]
    source_glyf = source_font["glyf"]
    for row in rows:
        name = cmap.get(row.codepoint)
        if name is None or font.getGlyphID(name) == 0:
            problems.append(f"U+{row.codepoint:04X} ({row.variant}) gives .notdef")
            continue
        glyph = glyf[name]
        if glyph.numberOfContours <= 0:
            problems.append(f"{row.variant}: no outline")
            continue
        if hasattr(glyph, "program") and glyph.program.getBytecode():
            problems.append(f"{row.variant}: still hinted")
        if hmtx[name][1] != glyph.xMin:
            problems.append(f"{row.variant}: side bearing differs from xMin")
        # The outline itself must come through untouched.
        coords, ends, flags = glyph.getCoordinates(glyf)
        source = source_glyf[row.source_glyph]
        source_coords, source_ends, source_flags = source.getCoordinates(source_glyf)
        if (list(coords) != list(source_coords) or list(ends) != list(source_ends)
                or [f & 1 for f in flags] != [f & 1 for f in source_flags]):
            problems.append(f"{row.variant}: outline differs from Lucide's")
    if problems:
        sys.exit("error: the written font failed its check:\n  " + "\n  ".join(problems))
    return font


def rust_blocks(rows):
    """The `enum Icon` and `impl Icon` blocks of icons.rs."""
    out = [
        "/// One icon of `ICON_FONT`. Each comment starts with the Lucide name of the",
        "/// icon the variant draws.",
        "#[derive(Clone, Copy, PartialEq, Eq, Debug)]",
        "pub(crate) enum Icon {",
    ]
    for row in rows:
        out.append(f"    /// `{row.name}`: {row.purpose}.")
        out.append(f"    {row.variant},")
    out += [
        "}",
        "",
        "impl Icon {",
        "    /// The icon's code point in `ICON_FONT`.",
        "    pub(crate) fn codepoint(self) -> char {",
        "        match self {",
    ]
    for row in rows:
        out.append(f"            Icon::{row.variant} => '\\u{{{row.codepoint:04X}}}',")
    out += [
        "        }",
        "    }",
        "}",
    ]
    return "\n".join(out)


def main():
    parser = argparse.ArgumentParser(
        description="Subset Lucide's icon font for the launcher and print the Rust table.")
    parser.add_argument("lucide_ttf", type=Path,
                        help="font/lucide.ttf from the lucide-static npm package")
    parser.add_argument("codepoint_map", type=Path,
                        help="font/codepoints.json, font/info.json or font/lucide.css "
                             "from the same package")
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT,
                        help="where to write the subset (default: "
                             f"{DEFAULT_OUT.relative_to(REPO).as_posix()})")
    parser.add_argument("--lucide-version",
                        help="recorded in the font's name table (default: read from the "
                             "package.json above font/)")
    args = parser.parse_args()

    # fontTools warns about harmless quirks of Lucide's file (stray bytes at
    # the end of its glyph name table); real failures still raise.
    logging.basicConfig(level=logging.ERROR)
    logging.getLogger("fontTools").setLevel(logging.ERROR)

    names = load_codepoint_map(args.codepoint_map)
    source_font = TTFont(args.lucide_ttf)
    rows = resolve(source_font, names)
    version = args.lucide_version or package_version(args.lucide_ttf)
    build(args.lucide_ttf, rows, args.out, version)
    font = verify(args.out, source_font, rows)

    size = args.out.stat().st_size
    print(f"Lucide {version or '(version unknown)'}: {len(rows)} icons, "
          f"{font['maxp'].numGlyphs} glyphs with .notdef, {size} bytes "
          f"({size / 1024:.1f} KiB) -> {args.out}", file=sys.stderr)
    print(f"tables: {' '.join(sorted(t.strip() for t in font.keys() if t != 'GlyphOrder'))}",
          file=sys.stderr)
    print(rust_blocks(rows))


if __name__ == "__main__":
    main()
