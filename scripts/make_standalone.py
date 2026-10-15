#!/usr/bin/env python3
"""Turn one Flash game into its own Switch homebrew app (.nro).

Takes FlashNX.nro as released (no build tools needed), the game's .swf and,
optionally, a picture and a name, and writes <Name>.nro: the FlashNX player with
the game inside. It shows in the Homebrew Menu with that picture and name, and
boots straight into the game, without FlashNX installed and without the game on
the SD card.

    python make_standalone.py FlashNX.nro MyGame.swf --name "My Game" --author "Me"

Run it without arguments (or double-click it) for a small window doing the same.

How it works: an .nro ends with an asset section holding the icon, the NACP
(name, author, version) and a RomFS (https://switchbrew.org/wiki/NRO). This
keeps the program part of FlashNX.nro byte for byte and writes a new asset
section after it: the picture as the icon, FlashNX's NACP with the new name,
and a RomFS holding the game, laid out as switch-tools lays one out
(`elf2nro --romfsdir`). FlashNX 1.9.0 and later boot the single .swf they find
at the root of their RomFS (issue #106). A multi-file game's companions, in a
`<game>.files` folder next to the .swf, go in with it.

Only the Python standard library is needed. Pillow (`pip install pillow`) lets
the picture be any size or format; without it, give a 256x256 JPEG.
"""

import argparse
import os
import struct
import sys
import tempfile

ASSET_HEADER_SIZE = 0x38
NACP_SIZE = 0x4000
NACP_LANGS = 16
NACP_LANG_STRIDE = 0x300
NACP_NAME_CAP = 0x200
NACP_AUTHOR_CAP = 0x100
NACP_VERSION_OFF = 0x3060
NACP_VERSION_CAP = 0x10
ROMFS_HEADER_SIZE = 0x50
ROMFS_FILE_PARTITION_OFS = 0x200
ROMFS_EMPTY = 0xFFFFFFFF
ICON_SIZE = 256
# The first release whose .nro boots a game from its own RomFS.
MIN_FLASHNX = (1, 9, 0)
COPY_CHUNK = 4 << 20


class StandaloneError(Exception):
    """A problem with the inputs, said in a sentence for the user."""


# ---------------------------------------------------------------------------
# The .nro: program part and asset section
# ---------------------------------------------------------------------------

def read_nro(path):
    """The program part of an .nro, and its icon, NACP and RomFS (b'' if absent)."""
    with open(path, 'rb') as f:
        data = f.read()
    if len(data) < 0x80 or data[0x10:0x14] != b'NRO0':
        raise StandaloneError('%s is not an .nro file.' % os.path.basename(path))
    size = struct.unpack_from('<I', data, 0x18)[0]
    if size > len(data):
        raise StandaloneError('%s is truncated.' % os.path.basename(path))
    icon = nacp = romfs = b''
    if len(data) >= size + ASSET_HEADER_SIZE and data[size:size + 4] == b'ASET':
        icon_off, icon_len, nacp_off, nacp_len, romfs_off, romfs_len = struct.unpack_from(
            '<6Q', data, size + 8)

        def take(off, length):
            return data[size + off:size + off + length] if length else b''

        icon = take(icon_off, icon_len)
        nacp = take(nacp_off, nacp_len)
        romfs = take(romfs_off, romfs_len)
    return data[:size], icon, nacp, romfs


def nacp_version(nacp):
    """The display version stored in a NACP, as a tuple of ints, or None."""
    if len(nacp) < NACP_VERSION_OFF + NACP_VERSION_CAP:
        return None
    raw = nacp[NACP_VERSION_OFF:NACP_VERSION_OFF + NACP_VERSION_CAP].split(b'\0')[0]
    try:
        return tuple(int(p) for p in raw.decode('ascii').split('.'))
    except ValueError:
        return None


def utf8_capped(text, cap):
    """`text` in UTF-8, cut on a character boundary to leave room for a NUL."""
    out = text.encode('utf-8')
    while len(out) > cap - 1:
        text = text[:-1]
        out = text.encode('utf-8')
    return out


def make_nacp(base, name, author, version):
    """FlashNX's NACP with the app's name and author in all sixteen languages
    (the Homebrew Menu reads the console's), and its version."""
    nacp = bytearray(base[:NACP_SIZE].ljust(NACP_SIZE, b'\0'))
    name_b = utf8_capped(name, NACP_NAME_CAP)
    author_b = utf8_capped(author, NACP_AUTHOR_CAP)
    for i in range(NACP_LANGS):
        off = i * NACP_LANG_STRIDE
        nacp[off:off + NACP_NAME_CAP] = name_b.ljust(NACP_NAME_CAP, b'\0')
        nacp[off + NACP_NAME_CAP:off + NACP_NAME_CAP + NACP_AUTHOR_CAP] = (
            author_b.ljust(NACP_AUTHOR_CAP, b'\0'))
    if version:
        nacp[NACP_VERSION_OFF:NACP_VERSION_OFF + NACP_VERSION_CAP] = (
            utf8_capped(version, NACP_VERSION_CAP).ljust(NACP_VERSION_CAP, b'\0'))
    return bytes(nacp)


# ---------------------------------------------------------------------------
# The icon
# ---------------------------------------------------------------------------

def jpeg_info(data):
    """(width, height, progressive) of a JPEG, or None if it is not one."""
    if data[:2] != b'\xff\xd8':
        return None
    i = 2
    while i + 4 <= len(data):
        if data[i] != 0xFF:
            return None
        marker = data[i + 1]
        if marker in (0xD8, 0x01) or 0xD0 <= marker <= 0xD7:
            i += 2
            continue
        length = struct.unpack_from('>H', data, i + 2)[0]
        if marker in (0xC0, 0xC1, 0xC2) and i + 9 <= len(data):
            height, width = struct.unpack_from('>HH', data, i + 5)
            return width, height, marker == 0xC2
        i += 2 + length
    return None


# The rule of FlashNX's tiles and HOME menu icons (rust/src/backend/render/kit/
# cover.rs, `cover_square_rgb`): a picture is cropped to the square only when
# that cuts at most 15 % of it; otherwise it shows whole over a blurred,
# darkened copy of itself. Transparent parts sit on the tile colour.
FILL_LIMIT = 0.15
AMBIENT_W = 20
AMBIENT_LIGHT = 0.5
AMBIENT_SATURATE = 1.3
AMBIENT_ZOOM = 1.3
TILE_RGB = (0x1E, 0x2C, 0x4A)


def crop_uv(w, h):
    """The centred part of a w x h picture that fills a square: [u0, v0, du, dv]."""
    r = w / float(h)
    if r > 1.0:
        f = 1.0 / r
        return [(1.0 - f) * 0.5, 0.0, f, 1.0]
    return [0.0, (1.0 - r) * 0.5, 1.0, r]


def ambient(img):
    """A tiny blurred, darkened, saturated copy of the picture on the tile
    colour: stretched with linear filtering, its pixels are the blur."""
    from PIL import Image
    w, h = img.size
    aw = max(1, min(AMBIENT_W, w))
    ah = min(h, max(2, min(40, int(round(AMBIENT_W * h / float(w))))))
    small = img.convert('RGBa').resize((aw, ah), Image.BOX)
    px = [[c / 1.0 for c in p[:3]] + [p[3] / 255.0] for p in small.getdata()]
    for _ in range(2):
        src = px
        px = []
        for y in range(ah):
            for x in range(aw):
                acc = [0.0, 0.0, 0.0, 0.0]
                for dy in (-1, 0, 1):
                    for dx in (-1, 0, 1):
                        sx = min(max(x + dx, 0), aw - 1)
                        sy = min(max(y + dy, 0), ah - 1)
                        s = src[sy * aw + sx]
                        for c in range(4):
                            acc[c] += s[c]
                px.append([v / 9.0 for v in acc])
    out = []
    for r, g, b, cov in px:
        p = [r * AMBIENT_LIGHT, g * AMBIENT_LIGHT, b * AMBIENT_LIGHT]
        lum = 0.213 * p[0] + 0.715 * p[1] + 0.072 * p[2]
        uncovered = 1.0 - min(max(cov, 0.0), 1.0)
        out.append(tuple(
            int(min(max(max(lum + (p[c] - lum) * AMBIENT_SATURATE, 0.0) + TILE_RGB[c] * uncovered,
                        0.0), 255.0))
            for c in range(3)))
    amb = Image.new('RGB', (aw, ah))
    amb.putdata(out)
    return amb


def cover_square(img, side=ICON_SIZE):
    """The picture laid out in a side x side square by the tile rule."""
    from PIL import Image
    img = img.convert('RGBA')
    w, h = img.size
    amb = ambient(img)
    u0, v0, du, dv = crop_uv(w, h)
    zu, zv = du / AMBIENT_ZOOM, dv / AMBIENT_ZOOM
    bu, bv = u0 + (du - zu) * 0.5, v0 + (dv - zv) * 0.5
    ground = amb.resize((side, side), Image.BILINEAR,
                        box=(bu * amb.width, bv * amb.height,
                             (bu + zu) * amb.width, (bv + zv) * amb.height)).convert('RGBA')
    r = w / float(h)
    if 1.0 - min(r, 1.0) / max(r, 1.0) <= FILL_LIMIT:
        part = img.crop((u0 * w, v0 * h, (u0 + du) * w, (v0 + dv) * h))
        fw = fh = side
    else:
        part = img
        if r > 1.0:
            fw, fh = side, min(max(int(round(side / r)), 1), side)
        else:
            fw, fh = min(max(int(round(side * r)), 1), side), side
    part = part.convert('RGBa').resize((fw, fh), Image.BOX).convert('RGBA')
    ground.alpha_composite(part, ((side - fw) // 2, (side - fh) // 2))
    return ground.convert('RGB')


def make_icon(path):
    """The picture as the 256x256 baseline JPEG the Homebrew Menu draws."""
    with open(path, 'rb') as f:
        data = f.read()
    try:
        from PIL import Image
    except ImportError:
        info = jpeg_info(data)
        if info and info[:2] == (ICON_SIZE, ICON_SIZE) and not info[2]:
            return data
        raise StandaloneError(
            'The picture must be a 256x256 JPEG (not progressive), or install '
            'Pillow ("pip install pillow") to use any picture.')
    import io
    try:
        img = Image.open(io.BytesIO(data))
        img.load()
    except Exception:
        raise StandaloneError('%s is not a picture Pillow can read.' % os.path.basename(path))
    out = io.BytesIO()
    # Quality 85, as FlashNX's HOME menu icons: flat box art bands below ~80.
    cover_square(img).save(out, 'JPEG', quality=85, progressive=False)
    return out.getvalue()


# ---------------------------------------------------------------------------
# The RomFS, as switch-tools' build_romfs writes it
# ---------------------------------------------------------------------------

def align(value, alignment):
    return (value + alignment - 1) & ~(alignment - 1)


def path_hash(parent, name):
    h = parent ^ 123456789
    for c in name:
        h = ((h >> 5) | (h << 27)) & 0xFFFFFFFF
        h ^= c
    return h


def hash_table_count(n):
    if n < 3:
        return 3
    if n < 19:
        return n | 1
    count = n
    while any(count % p == 0 for p in (2, 3, 5, 7, 11, 13, 17)):
        count += 1
    return count


class _Dir(object):
    def __init__(self, name, path, parent):
        self.name = name        # bytes, b'' for the root
        self.path = path        # b'/a/b', b'' for the root
        self.parent = parent if parent is not None else self
        self.dirs = {}
        self.files = {}
        self.offset = 0


class _File(object):
    def __init__(self, name, path, parent, src, size):
        self.name = name
        self.path = path
        self.parent = parent
        self.src = src
        self.size = size
        self.offset = 0
        self.data_offset = 0


class RomfsPlan(object):
    """Every offset of a RomFS image, worked out before a byte is written, so
    the files can be streamed into the output instead of held in memory."""

    def __init__(self, entries):
        # entries: (path inside the RomFS with '/' separators, source file)
        root = _Dir(b'', b'', None)
        for rel, src in entries:
            parts = [p.encode('utf-8') for p in rel.split('/') if p]
            d = root
            for part in parts[:-1]:
                if part not in d.dirs:
                    d.dirs[part] = _Dir(part, d.path + b'/' + part, d)
                d = d.dirs[part]
            name = parts[-1]
            d.files[name] = _File(name, d.path + b'/' + name, d, src, os.path.getsize(src))

        dirs = []
        files = []

        def walk(d):
            dirs.append(d)
            files.extend(d.files.values())
            for sub in d.dirs.values():
                walk(sub)

        walk(root)
        # Byte order of the full path, which is the order switch-tools keeps.
        dirs.sort(key=lambda d: d.path)
        files.sort(key=lambda f: f.path)
        self.root = root
        self.dirs = dirs
        self.files = files

        entry = 0
        part = 0
        for f in files:
            part = align(part, 0x10)
            f.data_offset = part
            part += f.size
            f.offset = entry
            entry += 0x20 + align(len(f.name), 4)
        self.file_table_size = entry
        self.partition_size = part

        entry = 0
        for d in dirs:
            d.offset = entry
            entry += 0x18 + (align(len(d.name), 4) if d is not root else 0)
        self.dir_table_size = entry

        self.dir_hash_count = hash_table_count(len(dirs))
        self.file_hash_count = hash_table_count(len(files))
        self.dir_hash_ofs = align(part + ROMFS_FILE_PARTITION_OFS, 4)
        self.dir_table_ofs = self.dir_hash_ofs + 4 * self.dir_hash_count
        self.file_hash_ofs = self.dir_table_ofs + self.dir_table_size
        self.file_table_ofs = self.file_hash_ofs + 4 * self.file_hash_count
        self.size = self.file_table_ofs + self.file_table_size

    def header(self):
        return struct.pack(
            '<10Q', ROMFS_HEADER_SIZE,
            self.dir_hash_ofs, 4 * self.dir_hash_count,
            self.dir_table_ofs, self.dir_table_size,
            self.file_hash_ofs, 4 * self.file_hash_count,
            self.file_table_ofs, self.file_table_size,
            ROMFS_FILE_PARTITION_OFS)

    def tables(self):
        """The two hash tables and two entry tables, in file order."""
        dir_hash = [ROMFS_EMPTY] * self.dir_hash_count
        file_hash = [ROMFS_EMPTY] * self.file_hash_count
        file_table = bytearray(self.file_table_size)
        dir_table = bytearray(self.dir_table_size)

        def first(items):
            items = sorted(items, key=lambda x: x.path)
            return items[0].offset if items else ROMFS_EMPTY

        def next_sibling(item, siblings):
            items = sorted(siblings, key=lambda x: x.path)
            i = items.index(item)
            return items[i + 1].offset if i + 1 < len(items) else ROMFS_EMPTY

        for f in self.files:
            bucket = path_hash(f.parent.offset, f.name) % self.file_hash_count
            struct.pack_into(
                '<IIQQII', file_table, f.offset,
                f.parent.offset, next_sibling(f, f.parent.files.values()),
                f.data_offset, f.size, file_hash[bucket], len(f.name))
            file_table[f.offset + 0x20:f.offset + 0x20 + len(f.name)] = f.name
            file_hash[bucket] = f.offset

        for d in self.dirs:
            is_root = d is self.root
            sibling = ROMFS_EMPTY if is_root else next_sibling(d, d.parent.dirs.values())
            bucket = path_hash(0 if is_root else d.parent.offset, d.name) % self.dir_hash_count
            struct.pack_into(
                '<IIIIII', dir_table, d.offset,
                d.parent.offset, sibling, first(d.dirs.values()), first(d.files.values()),
                dir_hash[bucket], len(d.name))
            dir_table[d.offset + 0x18:d.offset + 0x18 + len(d.name)] = d.name
            dir_hash[bucket] = d.offset

        return (struct.pack('<%dI' % len(dir_hash), *dir_hash), bytes(dir_table),
                struct.pack('<%dI' % len(file_hash), *file_hash), bytes(file_table))


def pad_to(out, position):
    gap = position - out.tell()
    if gap < 0:
        raise AssertionError('RomFS layout overlap')
    out.write(b'\0' * gap)


def write_romfs(out, plan):
    start = out.tell()
    out.write(plan.header())
    pad_to(out, start + ROMFS_FILE_PARTITION_OFS)
    for f in plan.files:
        pad_to(out, start + ROMFS_FILE_PARTITION_OFS + f.data_offset)
        copied = 0
        with open(f.src, 'rb') as src:
            while True:
                chunk = src.read(COPY_CHUNK)
                if not chunk:
                    break
                out.write(chunk)
                copied += len(chunk)
        if copied != f.size:
            raise StandaloneError('%s changed while it was being copied.' % f.src)
    pad_to(out, start + plan.dir_hash_ofs)
    for table in plan.tables():
        out.write(table)
    if out.tell() - start != plan.size:
        raise AssertionError('RomFS size mismatch')


# ---------------------------------------------------------------------------
# Reading the result back the way libnx does
# ---------------------------------------------------------------------------

def verify_nro(path, entries):
    """Open the written .nro as the console will: find each file through the
    RomFS hash tables (libnx's lookup) and through the directory links
    (`readdir`, how FlashNX finds its game), and compare it with its source."""
    prefix, _icon, _nacp, romfs = read_nro(path)
    del prefix
    hdr = struct.unpack_from('<10Q', romfs, 0)
    (_, dh_ofs, dh_size, dt_ofs, _dt_size, fh_ofs, fh_size, ft_ofs, _ft_size, part_ofs) = hdr
    dh_count = dh_size // 4
    fh_count = fh_size // 4

    def dir_entry(off):
        parent, sibling, child, first_file, nxt, nlen = struct.unpack_from('<6I', romfs, dt_ofs + off)
        name = romfs[dt_ofs + off + 0x18:dt_ofs + off + 0x18 + nlen]
        return parent, sibling, child, first_file, nxt, name

    def file_entry(off):
        parent, sibling, data_off, size, nxt, nlen = struct.unpack_from('<IIQQII', romfs, ft_ofs + off)
        name = romfs[ft_ofs + off + 0x20:ft_ofs + off + 0x20 + nlen]
        return parent, sibling, data_off, size, nxt, name

    def lookup_dir(parent, name):
        off = struct.unpack_from('<I', romfs, dh_ofs + 4 * (path_hash(parent, name) % dh_count))[0]
        while off != ROMFS_EMPTY:
            p, _s, _c, _f, nxt, n = dir_entry(off)
            if p == parent and n == name:
                return off
            off = nxt
        return None

    def lookup_file(parent, name):
        off = struct.unpack_from('<I', romfs, fh_ofs + 4 * (path_hash(parent, name) % fh_count))[0]
        while off != ROMFS_EMPTY:
            p, _s, data_off, size, nxt, n = file_entry(off)
            if p == parent and n == name:
                return data_off, size
            off = nxt
        return None

    if lookup_dir(0, b'') != 0:
        raise AssertionError('RomFS root not found')
    for rel, src in entries:
        parts = [p.encode('utf-8') for p in rel.split('/') if p]
        parent = 0
        for part in parts[:-1]:
            parent = lookup_dir(parent, part)
            if parent is None:
                raise AssertionError('folder of %s not found in the RomFS' % rel)
        found = lookup_file(parent, parts[-1])
        if found is None:
            raise AssertionError('%s not found in the RomFS' % rel)
        data_off, size = found
        with open(src, 'rb') as f:
            if f.read() != romfs[part_ofs + data_off:part_ofs + data_off + size]:
                raise AssertionError('%s differs from its source' % rel)

    listed = set()

    def walk(off, prefix):
        _p, _s, child, first_file, _n, _name = dir_entry(off)
        f = first_file
        while f != ROMFS_EMPTY:
            _fp, sibling, _d, _sz, _nx, n = file_entry(f)
            listed.add(prefix + n.decode('utf-8'))
            f = sibling
        c = child
        while c != ROMFS_EMPTY:
            _cp, sibling, _cc, _cf, _nx, n = dir_entry(c)
            walk(c, prefix + n.decode('utf-8') + '/')
            c = sibling

    walk(0, '')
    if listed != set(rel for rel, _src in entries):
        raise AssertionError('the RomFS lists %s' % sorted(listed))


# ---------------------------------------------------------------------------
# Putting it together
# ---------------------------------------------------------------------------

def game_entries(swf, files_dir=None):
    """The RomFS contents: the .swf at the root, its companions under
    `<game>.files/` (where FlashNX looks for them next to the game)."""
    if not os.path.isfile(swf):
        raise StandaloneError('%s does not exist.' % swf)
    name = os.path.basename(swf)
    if not name.lower().endswith('.swf'):
        raise StandaloneError('%s is not a .swf file.' % name)
    stem = name[:-4]
    entries = [(name, swf)]
    if files_dir is None:
        guess = os.path.join(os.path.dirname(os.path.abspath(swf)), stem + '.files')
        files_dir = guess if os.path.isdir(guess) else None
    if files_dir:
        if not os.path.isdir(files_dir):
            raise StandaloneError('%s is not a folder.' % files_dir)
        for top, subdirs, names in os.walk(files_dir):
            subdirs.sort()
            for n in sorted(names):
                full = os.path.join(top, n)
                rel = os.path.relpath(full, files_dir).replace(os.sep, '/')
                entries.append((stem + '.files/' + rel, full))
    return entries


def safe_file_name(text):
    """A file and folder name for the app. No spaces either: a HOME Menu
    forwarder made with Sphaira does not start an .nro whose path has a space
    in it ("the software was closed because an error occurred", before the app
    even runs; its owo.cpp passes the path unquoted). The Homebrew Menu and
    Sphaira show the app's name, not this."""
    bad = '<>:"/\\|?* '
    out = ''.join('_' if (c in bad or ord(c) < 32) else c for c in text.strip())
    while '__' in out:
        out = out.replace('__', '_')
    return out.strip('_.') or 'game'


def make_standalone(nro, swf, out_path, name=None, author=None, version=None,
                    icon=None, files_dir=None, log=print):
    """Write the standalone .nro; returns the list of files it carries."""
    prefix, base_icon, base_nacp, base_romfs = read_nro(nro)
    found = nacp_version(base_nacp)
    if found is not None and found < MIN_FLASHNX:
        raise StandaloneError(
            'This FlashNX.nro is version %s; a game inside the .nro needs %s or later.'
            % ('.'.join(map(str, found)), '.'.join(map(str, MIN_FLASHNX))))
    if not base_nacp:
        raise StandaloneError('%s carries no NACP: use FlashNX.nro as released.'
                              % os.path.basename(nro))
    entries = game_entries(swf, files_dir)
    stem = os.path.basename(swf)[:-4]
    name = name or stem
    author = author or 'FlashNX'
    version = version or '1.0.0'
    icon_data = make_icon(icon) if icon else base_icon
    nacp = make_nacp(base_nacp, name, author, version)
    plan = RomfsPlan(entries)
    if base_romfs:
        log('note: this .nro already carried a game; it is replaced.')
    if ' ' in os.path.basename(out_path):
        log('note: with a space in its name (or its folder\'s), a HOME Menu icon '
            'made with Sphaira will not start this app.')

    icon_off = ASSET_HEADER_SIZE
    nacp_off = icon_off + len(icon_data)
    romfs_off = nacp_off + len(nacp)
    asset_header = b'ASET' + struct.pack(
        '<I6Q', 0, icon_off, len(icon_data), nacp_off, len(nacp), romfs_off, plan.size)

    out_dir = os.path.dirname(os.path.abspath(out_path))
    fd, tmp = tempfile.mkstemp(suffix='.nro.part', dir=out_dir)
    try:
        with os.fdopen(fd, 'wb') as out:
            out.write(prefix)
            out.write(asset_header)
            out.write(icon_data)
            out.write(nacp)
            write_romfs(out, plan)
        verify_nro(tmp, entries)
        os.replace(tmp, out_path)
    except BaseException:
        if os.path.exists(tmp):
            os.remove(tmp)
        raise
    return entries


def describe(out_path, entries, name):
    folder = safe_file_name(name)
    return ('%s written (%.1f MB, %d file%s inside).\n'
            'Copy it to sdmc:/switch/%s/ on the SD card (a folder name without '
            'spaces): it shows in the Homebrew Menu.\n'
            'Its settings on the card are filed under "%s", the .swf name: give each game '
            'its own file name.'
            % (out_path, os.path.getsize(out_path) / 1e6, len(entries),
               '' if len(entries) == 1 else 's', folder, entries[0][0]))


# ---------------------------------------------------------------------------
# Command line and window
# ---------------------------------------------------------------------------

def main_cli(argv):
    p = argparse.ArgumentParser(
        description='Turn one Flash game into its own Switch homebrew app (.nro).')
    p.add_argument('nro', help='FlashNX.nro, as released (1.9.0 or later)')
    p.add_argument('swf', help="the game's .swf")
    p.add_argument('--name', help='the app name (default: the .swf name)')
    p.add_argument('--author', help='the author shown in the Homebrew Menu')
    p.add_argument('--version', help='the version shown in the Homebrew Menu (default 1.0.0)')
    p.add_argument('--icon', help='a picture for the icon (default: the FlashNX icon)')
    p.add_argument('--files', help='folder of companion files (default: <game>.files next to the .swf)')
    p.add_argument('-o', '--out', help='output .nro (default: <name>.nro here)')
    args = p.parse_args(argv)
    name = args.name or os.path.basename(args.swf)[:-4]
    out_path = args.out or safe_file_name(name) + '.nro'
    try:
        entries = make_standalone(args.nro, args.swf, out_path, name, args.author,
                                  args.version, args.icon, args.files)
    except StandaloneError as e:
        print('error: %s' % e, file=sys.stderr)
        return 1
    print(describe(out_path, entries, name))
    return 0


def main_gui():
    import tkinter as tk
    from tkinter import filedialog, messagebox, ttk

    root = tk.Tk()
    root.title('FlashNX: make a standalone app')
    frame = ttk.Frame(root, padding=12)
    frame.grid(sticky='nsew')
    root.columnconfigure(0, weight=1)
    frame.columnconfigure(1, weight=1)

    here = os.path.dirname(os.path.abspath(sys.argv[0]))
    default_nro = next((c for c in (os.path.join(here, 'FlashNX.nro'),
                                    os.path.join(os.getcwd(), 'FlashNX.nro'))
                        if os.path.isfile(c)), '')
    fields = {}

    def row(r, label, key, browse=None, value=''):
        ttk.Label(frame, text=label).grid(row=r, column=0, sticky='w', pady=3)
        var = tk.StringVar(value=value)
        ttk.Entry(frame, textvariable=var, width=48).grid(row=r, column=1, sticky='ew', padx=6)
        if browse:
            ttk.Button(frame, text='Browse...', command=browse).grid(row=r, column=2)
        fields[key] = var

    def pick(key, title, types):
        path = filedialog.askopenfilename(title=title, filetypes=types)
        if path:
            fields[key].set(path)
            if key == 'swf' and not fields['name'].get():
                fields['name'].set(os.path.basename(path)[:-4])

    row(0, 'FlashNX.nro', 'nro', lambda: pick('nro', 'FlashNX.nro', [('Switch homebrew', '*.nro')]),
        default_nro)
    row(1, 'Game (.swf)', 'swf', lambda: pick('swf', 'The game', [('Flash game', '*.swf')]))
    row(2, 'Picture (optional)', 'icon', lambda: pick(
        'icon', 'Picture for the icon', [('Pictures', '*.jpg *.jpeg *.png *.webp *.bmp *.gif'),
                                         ('All files', '*.*')]))
    row(3, 'Name', 'name')
    row(4, 'Author', 'author')
    row(5, 'Version', 'version', value='1.0.0')
    status = tk.StringVar(value='Companion files in a "<game>.files" folder next to the .swf go in too.')
    ttk.Label(frame, textvariable=status, wraplength=460, justify='left').grid(
        row=7, column=0, columnspan=3, sticky='w', pady=(10, 0))

    def create():
        nro, swf = fields['nro'].get().strip(), fields['swf'].get().strip()
        if not nro or not swf:
            messagebox.showerror('Missing file', 'Choose FlashNX.nro and the game first.')
            return
        name = fields['name'].get().strip() or os.path.basename(swf)[:-4]
        out_path = filedialog.asksaveasfilename(
            title='Save the app as', defaultextension='.nro',
            initialfile=safe_file_name(name) + '.nro', filetypes=[('Switch homebrew', '*.nro')])
        if not out_path:
            return
        status.set('Writing...')
        root.update_idletasks()
        try:
            entries = make_standalone(nro, swf, out_path, name, fields['author'].get().strip(),
                                      fields['version'].get().strip(),
                                      fields['icon'].get().strip() or None,
                                      log=lambda s: status.set(s))
        except StandaloneError as e:
            status.set('')
            messagebox.showerror('Could not make the app', str(e))
            return
        status.set(describe(out_path, entries, name))

    ttk.Button(frame, text='Make the .nro', command=create).grid(
        row=6, column=0, columnspan=3, pady=(12, 0))
    root.mainloop()
    return 0


if __name__ == '__main__':
    if len(sys.argv) > 1:
        sys.exit(main_cli(sys.argv[1:]))
    try:
        sys.exit(main_gui())
    except ImportError:
        sys.exit(main_cli(['--help']))
