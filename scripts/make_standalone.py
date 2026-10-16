#!/usr/bin/env python3
"""Turn one Flash game into its own Switch homebrew app (.nro).

Takes FlashNX.nro as released (no build tools needed), the game's .swf and,
optionally, a picture and a name, and writes <Name>.nro: the FlashNX player with
the game inside. It shows in the Homebrew Menu with that picture and name, and
boots straight into the game, without FlashNX installed and without the game on
the SD card.

    python make_standalone.py FlashNX.nro MyGame.swf --name "My Game" --author "Me"

Run it without arguments (or double-click it) for a small window doing the same.

An app stays on the FlashNX it was made from; to move it to a newer one, without
the game's files:

    python make_standalone.py latest --update My_Game.nro

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
import re
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

def nro_layout(path):
    """An .nro's parts, from its headers alone: the size of its program part,
    its icon and NACP (b'' if absent), and where its RomFS lies in the file as
    (offset, size), size 0 when it has none. The file is checked to hold every
    part, so a cut download is caught. The RomFS stays on disk: it is the game,
    and a game can run to gigabytes."""
    name = os.path.basename(path)
    with open(path, 'rb') as f:
        total = os.fstat(f.fileno()).st_size
        head = f.read(0x80)
        if len(head) < 0x80 or head[0x10:0x14] != b'NRO0':
            raise StandaloneError('%s is not an .nro file.' % name)
        size = struct.unpack_from('<I', head, 0x18)[0]
        if size > total:
            raise StandaloneError('%s is truncated.' % name)
        f.seek(size)
        aset = f.read(ASSET_HEADER_SIZE)
        if len(aset) < ASSET_HEADER_SIZE or aset[:4] != b'ASET':
            return size, b'', b'', (0, 0)
        icon_off, icon_len, nacp_off, nacp_len, romfs_off, romfs_len = struct.unpack_from(
            '<6Q', aset, 8)
        for off, length in ((icon_off, icon_len), (nacp_off, nacp_len), (romfs_off, romfs_len)):
            if length and size + off + length > total:
                raise StandaloneError('%s is truncated.' % name)

        def take(off, length):
            if not length:
                return b''
            f.seek(size + off)
            return f.read(length)

        romfs = (size + romfs_off, romfs_len) if romfs_len else (0, 0)
        return size, take(icon_off, icon_len), take(nacp_off, nacp_len), romfs


def nro_assets(path):
    """The icon and NACP of an .nro, read without the rest of it."""
    return nro_layout(path)[1:3]


def read_program(path, size):
    """The first `size` bytes of an .nro: its program part, which every app
    made from it carries unchanged."""
    with open(path, 'rb') as f:
        return f.read(size)


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


def is_web_picture(text):
    """Whether the picture is given as a web address or a data: URI."""
    return text.strip().strip('"').startswith(('http://', 'https://', 'data:image/'))


def picture_source(text):
    """The picture's own address from what a browser hands over: a Google
    Images result (`imgres?imgurl=...`) points at the picture it shows."""
    from urllib.parse import parse_qs, urlparse
    text = text.strip().strip('"')
    query = parse_qs(urlparse(text).query)
    if text.startswith(('http://', 'https://')) and query.get('imgurl'):
        return query['imgurl'][0]
    return text


def fetch_picture(src):
    """A picture given as a web address, or as a data: URI (what Google
    Images copies for its thumbnails), saved to a temporary file. Returns
    (path, temporary); a local path comes back as is."""
    src = picture_source(src)
    if src.startswith('data:image/'):
        import base64
        from urllib.parse import unquote_to_bytes
        header, _, payload = src.partition(',')
        try:
            data = base64.b64decode(payload) if ';base64' in header else unquote_to_bytes(payload)
        except Exception:
            raise StandaloneError('The picture\'s data: address is damaged.')
    elif src.startswith(('http://', 'https://')):
        import urllib.request
        # Some image hosts refuse a request that does not look like a browser.
        req = urllib.request.Request(src, headers={
            'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) FlashNX make_standalone.py'})
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                data = r.read(32 << 20)
        except Exception as e:
            raise StandaloneError('Could not download the picture: %s' % e)
    else:
        return src, False
    fd, path = tempfile.mkstemp(suffix='.picture')
    with os.fdopen(fd, 'wb') as f:
        f.write(data)
    return path, True


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
        raise StandaloneError('The picture is not one Pillow can read (JPG, PNG, WebP, BMP, GIF...).')
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


class Slice(object):
    """`size` bytes of the file `path` from `offset`: a file inside an app made
    earlier, copied out of it by `update_app` without being read into memory."""

    __slots__ = ('path', 'offset', 'size')

    def __init__(self, path, offset, size):
        self.path = path
        self.offset = offset
        self.size = size


# What a RomFS file can come from: bytes, the path of a file, or a `Slice`.

def source_size(src):
    if isinstance(src, bytes):
        return len(src)
    if isinstance(src, Slice):
        return src.size
    return os.path.getsize(src)


def source_chunks(src):
    """A RomFS source's bytes, `COPY_CHUNK` at a time, so that no file, however
    big, is ever held whole."""
    if isinstance(src, bytes):
        yield src
        return
    if isinstance(src, Slice):
        path, offset, left = src.path, src.offset, src.size
    else:
        path, offset, left = src, 0, None
    with open(path, 'rb') as f:
        f.seek(offset)
        while left is None or left > 0:
            chunk = f.read(COPY_CHUNK if left is None else min(COPY_CHUNK, left))
            if not chunk:
                return
            if left is not None:
                left -= len(chunk)
            yield chunk


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
            d.files[name] = _File(name, d.path + b'/' + name, d, src, source_size(src))

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


def write_romfs(out, plan, advance=None):
    """`advance(n)`, if given, hears of each `n` bytes of file data written."""
    start = out.tell()
    out.write(plan.header())
    pad_to(out, start + ROMFS_FILE_PARTITION_OFS)
    for f in plan.files:
        pad_to(out, start + ROMFS_FILE_PARTITION_OFS + f.data_offset)
        copied = 0
        for chunk in source_chunks(f.src):
            out.write(chunk)
            copied += len(chunk)
            if advance:
                advance(len(chunk))
        if copied != f.size:
            raise StandaloneError('%s changed while it was being copied.'
                                  % (f.src if isinstance(f.src, str) else f.path.decode('utf-8')))
    pad_to(out, start + plan.dir_hash_ofs)
    for table in plan.tables():
        out.write(table)
    if out.tell() - start != plan.size:
        raise AssertionError('RomFS size mismatch')


# ---------------------------------------------------------------------------
# Reading the result back the way libnx does
# ---------------------------------------------------------------------------

class RomfsReader(object):
    """A RomFS image inside an open file, read as libnx reads one: its header
    and its four tables in memory (they hold names and offsets, a few hundred
    KB for thousands of files), its files left where they are on disk."""

    def __init__(self, f, base):
        self.f = f
        self.base = base
        hdr = self._read(0, ROMFS_HEADER_SIZE)
        (_, dh_ofs, dh_size, dt_ofs, dt_size, fh_ofs, fh_size, ft_ofs, ft_size,
         part_ofs) = struct.unpack('<10Q', hdr)
        self.dir_hash = self._read(dh_ofs, dh_size)
        self.dir_table = self._read(dt_ofs, dt_size)
        self.file_hash = self._read(fh_ofs, fh_size)
        self.file_table = self._read(ft_ofs, ft_size)
        self.dh_count = dh_size // 4
        self.fh_count = fh_size // 4
        self.part = base + part_ofs

    def _read(self, off, size):
        self.f.seek(self.base + off)
        data = self.f.read(size)
        if len(data) != size:
            raise StandaloneError('The RomFS is damaged: it ends early.')
        return data

    def dir_entry(self, off):
        parent, sibling, child, first_file, nxt, nlen = struct.unpack_from('<6I', self.dir_table, off)
        return parent, sibling, child, first_file, nxt, self.dir_table[off + 0x18:off + 0x18 + nlen]

    def file_entry(self, off):
        parent, sibling, data_off, size, nxt, nlen = struct.unpack_from('<IIQQII', self.file_table, off)
        return parent, sibling, data_off, size, nxt, self.file_table[off + 0x20:off + 0x20 + nlen]

    def lookup_dir(self, parent, name):
        """libnx's lookup: the hash bucket, then its chain."""
        if not self.dh_count:
            return None
        bucket = path_hash(parent, name) % self.dh_count
        off = struct.unpack_from('<I', self.dir_hash, 4 * bucket)[0]
        seen = set()
        while off != ROMFS_EMPTY and off not in seen:
            seen.add(off)
            p, _s, _c, _f, nxt, n = self.dir_entry(off)
            if p == parent and n == name:
                return off
            off = nxt
        return None

    def lookup_file(self, parent, name):
        """(offset in the file, size) of a file, found as libnx finds it."""
        if not self.fh_count:
            return None
        bucket = path_hash(parent, name) % self.fh_count
        off = struct.unpack_from('<I', self.file_hash, 4 * bucket)[0]
        seen = set()
        while off != ROMFS_EMPTY and off not in seen:
            seen.add(off)
            p, _s, data_off, size, nxt, n = self.file_entry(off)
            if p == parent and n == name:
                return self.part + data_off, size
            off = nxt
        return None

    def files(self):
        """Every file as (path, offset in the file, size), through the
        directory links: `readdir`, which is how FlashNX finds its game."""
        out = []
        seen = set()

        def walk(off, prefix):
            if off in seen:
                raise StandaloneError('The RomFS is damaged: a folder lists itself.')
            seen.add(off)
            _p, _s, child, f, _n, _name = self.dir_entry(off)
            files_seen = set()
            while f != ROMFS_EMPTY and f not in files_seen:
                files_seen.add(f)
                _fp, sibling, data_off, size, _nx, n = self.file_entry(f)
                out.append((prefix + n.decode('utf-8'), self.part + data_off, size))
                f = sibling
            while child != ROMFS_EMPTY:
                _cp, sibling, _cc, _cf, _nx, n = self.dir_entry(child)
                walk(child, prefix + n.decode('utf-8') + '/')
                child = sibling

        try:
            walk(0, '')
        except (struct.error, UnicodeDecodeError):
            raise StandaloneError('The RomFS is damaged.')
        return out


def same_bytes(f, offset, size, src, advance=None):
    """Whether `size` bytes of `f` at `offset` are the source `src`, compared
    a chunk at a time."""
    if source_size(src) != size:
        return False
    f.seek(offset)
    chunks = source_chunks(src)
    try:
        for chunk in chunks:
            if f.read(len(chunk)) != chunk:
                return False
            if advance:
                advance(len(chunk))
    finally:
        chunks.close()
    return True


def verify_nro(path, entries, advance=None):
    """Open the written .nro as the console will: find each file through the
    RomFS hash tables (libnx's lookup) and through the directory links
    (`readdir`, how FlashNX finds its game), and compare it with its source.
    Read a chunk at a time, as it was written: a game of several GB is never
    held in memory."""
    _size, _icon, _nacp, (romfs_off, romfs_size) = nro_layout(path)
    if not romfs_size:
        raise AssertionError('no RomFS written')
    with open(path, 'rb') as f:
        rd = RomfsReader(f, romfs_off)
        if rd.lookup_dir(0, b'') != 0:
            raise AssertionError('RomFS root not found')
        for rel, src in entries:
            parts = [p.encode('utf-8') for p in rel.split('/') if p]
            parent = 0
            for part in parts[:-1]:
                parent = rd.lookup_dir(parent, part)
                if parent is None:
                    raise AssertionError('folder of %s not found in the RomFS' % rel)
            found = rd.lookup_file(parent, parts[-1])
            if found is None:
                raise AssertionError('%s not found in the RomFS' % rel)
            if not same_bytes(f, found[0], found[1], src, advance):
                raise AssertionError('%s differs from its source' % rel)
        listed = set(rel for rel, _off, _size in rd.files())
    if listed != set(rel for rel, _src in entries):
        raise AssertionError('the RomFS lists %s' % sorted(listed))


def romfs_files(path, romfs_off):
    """Every file of the RomFS at `romfs_off` in the file `path`, as (path in
    the RomFS, offset in the file, size): what an app made earlier carries, to
    write it again without reading it into memory."""
    with open(path, 'rb') as f:
        return RomfsReader(f, romfs_off).files()


# ---------------------------------------------------------------------------
# Putting it together
# ---------------------------------------------------------------------------

def game_entries(swf, files_dir=None, address=None):
    """The RomFS contents: the .swf at the root, its Flashpoint address beside
    it (`game_address`), its companions under `<game>.files/` (where FlashNX
    looks for them next to the game)."""
    if not os.path.isfile(swf):
        raise StandaloneError('%s does not exist.' % swf)
    name = os.path.basename(swf)
    if not name.lower().endswith('.swf'):
        raise StandaloneError('%s is not a .swf file.' % name)
    stem = name[:-4]
    entries = [(name, swf)]
    url = game_address(swf, address)
    if url:
        entries.append((name + '.base', url.encode('utf-8')))
    root = companions_root(swf, files_dir)
    if root:
        for top, subdirs, names in os.walk(root):
            subdirs.sort()
            for n in sorted(names):
                full = os.path.join(top, n)
                rel = os.path.relpath(full, root).replace(os.sep, '/')
                entries.append((stem + '.files/' + rel, full))
    return entries


def game_address(swf, address=None):
    """A Flashpoint game's original address, or None.

    FlashNX runs such a game under that URL (rust/src/lib.rs, `ruffle_init`,
    which reads `romfs:/<game>.swf.base`): its relative loads resolve against
    it into `<game>.files/<host>/<path>`, and its saves are named after it.
    Left out, the game ran under the flat `flashforswitch.local` address,
    found none of its files and stalled on its loader, and it saved apart
    from the same game in FlashNX.

    `address` None takes the `<game>.swf.base` FlashNX writes beside a game it
    downloads; False means none; a web address is that address; anything else
    is the path of a file holding one, a `.base` named otherwise."""
    if address is False:
        return None
    if address is None:
        if not os.path.isfile(swf + '.base'):
            return None
        address = swf + '.base'
    text = address.strip()
    if not text.startswith(('http://', 'https://')):
        try:
            with open(address, 'rb') as f:
                text = f.read(4096).decode('utf-8', 'replace').strip()
        except OSError:
            raise StandaloneError('%s does not exist.' % address)
        # Flashpoint quotes a launch command with a space in it; FlashNX
        # writes it unquoted, but a hand-made file may not be.
        text = text.strip('"').strip()
        if not text.startswith(('http://', 'https://')):
            raise StandaloneError('%s does not hold a web address: a .swf.base is one line, '
                                  'http://... or https://...' % os.path.basename(address))
    return text.splitlines()[0].strip()


def companions_root(swf, files_dir=None):
    """The folder whose tree goes in as the game's companions, or None: the
    one given, else `<game>.files` beside the game."""
    if files_dir is None:
        stem = os.path.basename(swf)[:-4]
        guess = os.path.join(os.path.dirname(os.path.abspath(swf)), stem + '.files')
        files_dir = guess if os.path.isdir(guess) else None
    if not files_dir:
        return None
    if not os.path.isdir(files_dir):
        raise StandaloneError('%s is not a folder.' % files_dir)
    # A Flashpoint GameZIP unpacked as is (content.json beside content/):
    # FlashNX keeps what is under content/, <host>/<path>, and so does this.
    if (os.path.isfile(os.path.join(files_dir, 'content.json'))
            and os.path.isdir(os.path.join(files_dir, 'content'))):
        files_dir = os.path.join(files_dir, 'content')
    return files_dir


# A folder named like a website (`www.example.com`, `static.nickjr.com`): how
# the files of a Flashpoint game are filed, `<host>/<path>`.
HOST_FOLDER = re.compile(r'^[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}$')


def website_folders(root):
    """The folders at the top of `root` named like websites, sorted."""
    try:
        names = os.listdir(root) if root else []
    except OSError:
        return []
    return sorted(n for n in names if HOST_FOLDER.match(n) and os.path.isdir(os.path.join(root, n)))


def address_note(swf, root, address=None):
    """The note for a game whose files are filed by website while it has no
    Flashpoint address (`game_address`), or None. FlashNX writes that address
    beside a game it downloads from Flashpoint; copying the game off the card
    without it is easy, and nothing else would say why the app then hangs."""
    if game_address(swf, address):
        return None
    sites = website_folders(root)
    if not sites:
        return None
    shown = ', '.join(sites[:2]) + (', ...' if len(sites) > 2 else '')
    return ('note: its files are filed by website (%s), but %s.base, the address the game '
            'runs at, is not beside it. Copy it from the SD card too, or give it with '
            '--address: without it the game will not find its files.'
            % (shown, os.path.basename(swf)))


def meta_title(swf):
    """The game's real title, from the `<game>.swf.meta.json` FlashNX keeps
    beside a game it downloaded, or None."""
    import json
    try:
        with open(swf + '.meta.json', 'rb') as f:
            title = json.loads(f.read(64 << 10).decode('utf-8')).get('display_name')
    except (OSError, ValueError, AttributeError):
        return None
    return title.strip() if isinstance(title, str) and title.strip() else None


def game_report(swf, files_dir=None, nro=None, address=None):
    """What an app made from `swf` would carry, worked out without writing
    anything, for the window to say as soon as the game is chosen: how many
    companion files and how big, and from which folder; its Flashpoint
    address, if any; the websites its files are filed under when that address
    is missing; and the app's size in bytes (None without a FlashNX.nro to
    count its program part)."""
    entries = game_entries(swf, files_dir, address)
    companions = [src for rel, src in entries if '/' in rel]
    root = companions_root(swf, files_dir)
    url = game_address(swf, address)
    report = {
        'files': len(companions),
        'bytes': sum(source_size(s) for s in companions),
        'folder': root,
        'address': url,
        'base': bool(url),
        'note': address_note(swf, root, address),
        # The folders named like websites while the address is missing.
        'sites': [] if url else website_folders(root),
        'game': os.path.basename(swf),
        'size': None,
    }
    if nro:
        try:
            program = nro_layout(nro)[0]
        except (OSError, StandaloneError):
            return report
        plan = RomfsPlan(entries + [marker(None)])
        # The icon is not made yet: FlashNX's is about 76 KB, a cover less.
        report['size'] = program + ASSET_HEADER_SIZE + (80 << 10) + NACP_SIZE + plan.size
    return report


def safe_file_name(text):
    """A file and folder name for the app. No spaces either: a HOME Menu
    forwarder made with Sphaira does not start an .nro whose path has a space
    in it ("the software was closed because an error occurred", before the app
    even runs; its owo.cpp passes the path unquoted). The Homebrew Menu and
    Sphaira show the app's name, not this."""
    # Letters, digits, _ - . only: apostrophes go ("Garfields"), anything else
    # becomes _, so no tool along the way has a character to stumble on.
    text = text.strip().replace("'", '').replace('’', '')
    out = ''.join(c if (c.isalnum() or c in '_-.') else '_' for c in text)
    while '__' in out:
        out = out.replace('__', '_')
    return out.strip('_.') or 'game'


# Beside the game in the RomFS: which FlashNX the app was made from, for
# `update_app` to say what it replaces. FlashNX itself only looks for the .swf.
MARKER = '.flashnx.json'


def version_text(version):
    return '.'.join(map(str, version)) if version else 'unknown'


def flashnx_program(nro):
    """The program part, icon and NACP of a FlashNX.nro fit to carry a game,
    whether it already carries one, and its version."""
    size, base_icon, base_nacp, (_romfs_off, romfs_size) = nro_layout(nro)
    found = nacp_version(base_nacp)
    if found is not None and found < MIN_FLASHNX:
        raise StandaloneError(
            'This FlashNX.nro is version %s; a game inside the .nro needs %s or later.'
            % (version_text(found), version_text(MIN_FLASHNX)))
    if not base_nacp:
        raise StandaloneError('%s carries no NACP: use FlashNX.nro as released.'
                              % os.path.basename(nro))
    return read_program(nro, size), base_icon, base_nacp, romfs_size > 0, found


def marker(version):
    import json
    return (MARKER, json.dumps({'flashnx': version_text(version)}).encode('utf-8'))


# The largest file FAT32 holds: 4 GiB less one byte. The homebrew guides
# recommend FAT32 for the Switch's card (exFAT corrupts more easily when an
# app is left with HOME); the console reads a bigger .nro from exFAT, since
# libnx reads a RomFS with 64-bit offsets.
FAT32_MAX_FILE = 0xFFFFFFFF


def fat32_note(size):
    """What to tell about an app of `size` bytes, or None when FAT32 holds it."""
    if size <= FAT32_MAX_FILE:
        return None
    return ('note: this app is %.1f GB. A card formatted FAT32, the usual one for '
            'homebrew, cannot hold a file over 4 GB: it needs a card formatted exFAT.'
            % (size / 1e9))


def write_app(out_path, prefix, icon_data, nacp, entries, log=print, progress=None):
    """Program part, then an asset section with the icon, the NACP and a
    RomFS of `entries`; written beside `out_path`, read back, then put there.
    `progress(done, total)` follows both passes over the game's bytes: a
    game of several GB takes minutes, and a window that only said
    "Writing..." all that time looked stuck."""
    plan = RomfsPlan(entries)
    icon_off = ASSET_HEADER_SIZE
    nacp_off = icon_off + len(icon_data)
    romfs_off = nacp_off + len(nacp)
    asset_header = b'ASET' + struct.pack(
        '<I6Q', 0, icon_off, len(icon_data), nacp_off, len(nacp), romfs_off, plan.size)
    # Said before the copy, which for a game this big takes a while.
    note = fat32_note(len(prefix) + romfs_off + plan.size)
    if note:
        log(note)
    total = 2 * sum(f.size for f in plan.files)
    done = [0]

    def advance(n):
        done[0] += n
        if progress:
            progress(done[0], total)

    out_dir = os.path.dirname(os.path.abspath(out_path))
    fd, tmp = tempfile.mkstemp(suffix='.nro.part', dir=out_dir)
    try:
        with os.fdopen(fd, 'wb') as out:
            out.write(prefix)
            out.write(asset_header)
            out.write(icon_data)
            out.write(nacp)
            write_romfs(out, plan, advance)
        verify_nro(tmp, entries, advance)
        os.replace(tmp, out_path)
    except BaseException:
        if os.path.exists(tmp):
            os.remove(tmp)
        raise


def app_info(path):
    """What an app made from FlashNX carries: its name, author and version,
    the FlashNX it was made from (None when unknown), and its files, as parts
    of `path` (`Slice`) rather than read into memory."""
    import json
    _size, icon, nacp, (romfs_off, romfs_size) = nro_layout(path)
    if not romfs_size:
        raise StandaloneError('%s carries no game: it is not an app made from FlashNX.'
                              % os.path.basename(path))
    try:
        files = romfs_files(path, romfs_off)
    except StandaloneError as e:
        raise StandaloneError('%s: %s' % (os.path.basename(path), e))
    if not any('/' not in rel and rel.lower().endswith('.swf') for rel, _off, _size in files):
        raise StandaloneError('%s carries no .swf: it is not an app made from FlashNX.'
                              % os.path.basename(path))
    made_from = None
    for rel, off, size in files:
        if rel == MARKER and size <= 4096:
            with open(path, 'rb') as f:
                f.seek(off)
                data = f.read(size)
            try:
                made_from = json.loads(data.decode('utf-8')).get('flashnx')
            except (ValueError, AttributeError):
                pass
    return {
        'name': nacp[0:NACP_NAME_CAP].split(b'\0')[0].decode('utf-8', 'replace'),
        'author': nacp[NACP_NAME_CAP:NACP_NAME_CAP + NACP_AUTHOR_CAP].split(b'\0')[0].decode('utf-8', 'replace'),
        'version': nacp[NACP_VERSION_OFF:NACP_VERSION_OFF + NACP_VERSION_CAP].split(b'\0')[0].decode('utf-8', 'replace'),
        'flashnx': made_from,
        'icon': icon,
        'nacp': nacp,
        'files': [(rel, Slice(path, off, size)) for rel, off, size in files if rel != MARKER],
    }


def update_app(app, nro, out_path, log=print, progress=None):
    """The app `app` with the FlashNX of `nro`: its game, files, icon, name,
    author and version kept as they are, the program part replaced. Saves and
    settings live on the SD card under the game's name, so they carry over.
    The game's files are copied out of the old app a chunk at a time, so
    `out_path` may be `app` itself: it is replaced only once the new one has
    been read back. Returns (name, FlashNX it had, FlashNX it has now)."""
    prefix, _icon, _nacp, _carried, found = flashnx_program(nro)
    info = app_info(app)
    try:
        had = tuple(int(p) for p in info['flashnx'].split('.'))
    except (AttributeError, ValueError):
        had = None
    if had and found and had == found:
        raise StandaloneError('%s already runs FlashNX %s: nothing to update.'
                              % (info['name'], version_text(found)))
    if had and found and had > found:
        raise StandaloneError('%s runs FlashNX %s, newer than this FlashNX.nro (%s).'
                              % (info['name'], version_text(had), version_text(found)))
    write_app(out_path, prefix, info['icon'], info['nacp'], info['files'] + [marker(found)],
              log, progress)
    return info['name'], info['flashnx'], version_text(found)


def make_standalone(nro, swf, out_path, name=None, author=None, version=None,
                    icon=None, files_dir=None, log=print, progress=None, address=None):
    """Write the standalone .nro; returns the list of files it carries."""
    prefix, base_icon, base_nacp, carried, found = flashnx_program(nro)
    entries = game_entries(swf, files_dir, address)
    stem = os.path.basename(swf)[:-4]
    name = name or meta_title(swf) or stem
    author = author or 'FlashNX'
    version = version or '1.0.0'
    icon_data = base_icon
    if icon:
        local, temporary = fetch_picture(icon)
        try:
            icon_data = make_icon(local)
        finally:
            if temporary:
                os.remove(local)
    nacp = make_nacp(base_nacp, name, author, version)
    if carried:
        log('note: this .nro already carried a game; it is replaced.')
    missing = address_note(swf, companions_root(swf, files_dir), address)
    if missing:
        log(missing)
    if ' ' in os.path.basename(out_path):
        log('note: with a space in its name (or its folder\'s), a HOME Menu icon '
            'made with Sphaira will not start this app.')
    write_app(out_path, prefix, icon_data, nacp, entries + [marker(found)], log, progress)
    return entries


RELEASES_API = 'https://api.github.com/repos/Jonathan8520/FlashNX/releases/latest'


def download_dir():
    """Where a downloaded FlashNX.nro goes: next to this script, else the
    Downloads folder, else the temporary folder."""
    here = os.path.dirname(os.path.abspath(__file__))
    for d in (here, os.path.join(os.path.expanduser('~'), 'Downloads'), tempfile.gettempdir()):
        if os.path.isdir(d) and os.access(d, os.W_OK):
            return d
    return tempfile.gettempdir()


def download_flashnx(dest_dir, progress=None):
    """The FlashNX.nro of the latest release on GitHub, saved in `dest_dir`.
    Returns (path, version). `progress(done, total)` follows the download."""
    import json
    import urllib.request
    agent = {'User-Agent': 'FlashNX make_standalone.py'}
    try:
        req = urllib.request.Request(RELEASES_API, headers=dict(agent, Accept='application/vnd.github+json'))
        with urllib.request.urlopen(req, timeout=30) as r:
            release = json.load(r)
    except Exception as e:
        raise StandaloneError('Could not ask GitHub for the latest FlashNX: %s' % e)
    asset = next((a for a in release.get('assets', []) if a.get('name') == 'FlashNX.nro'), None)
    if asset is None:
        raise StandaloneError('The latest FlashNX release has no FlashNX.nro.')
    version = (release.get('tag_name') or '').lstrip('v') or '?'
    total = asset.get('size') or 0
    path = os.path.join(dest_dir, 'FlashNX.nro')
    part = path + '.part'
    try:
        req = urllib.request.Request(asset['browser_download_url'], headers=agent)
        done = 0
        with urllib.request.urlopen(req, timeout=60) as r, open(part, 'wb') as out:
            while True:
                chunk = r.read(1 << 16)
                if not chunk:
                    break
                out.write(chunk)
                done += len(chunk)
                if progress:
                    progress(done, total)
        if total and done != total:
            raise StandaloneError('The download stopped at %d of %d bytes.' % (done, total))
        nro_layout(part)  # whole, and an .nro
        os.replace(part, path)
    except StandaloneError:
        if os.path.exists(part):
            os.remove(part)
        raise
    except Exception as e:
        if os.path.exists(part):
            os.remove(part)
        raise StandaloneError('Could not download FlashNX.nro: %s' % e)
    return path, version


def describe(out_path, entries, name):
    folder = safe_file_name(name)
    return ('%s written (%.1f MB, %d file%s inside).\n'
            'Copy it to sdmc:/switch/%s/ on the SD card (a folder name without '
            'spaces): it shows in the Homebrew Menu.\n'
            'Its settings on the card are filed under "%s", the .swf name: give each game '
            'its own file name.'
            % (out_path, os.path.getsize(out_path) / 1e6, len(entries),
               '' if len(entries) == 1 else 's', folder, entries[0][0]))


# The FlashNX wordmark for the window (assets/banner.png, cropped, 96 px high,
# 128 colours), so it shows even when this script is downloaded on its own.
LOGO_PNG = (
    'iVBORw0KGgoAAAANSUhEUgAAAi0AAABgCAMAAADmbrLbAAAB/lBMVEXbVyfNXDChWCPbYiuhKxyk'
    'Y0u3LBy0UyPWb0ypLBvHMR1xYBNpHQ/iaEnwZkbVYyXpiU7ldWj1izq4TCNuZWI1JxPih03TLSTG'
    'OSa7JRXvgzryjkT///91AnK2akUA/wCtk1QAbwAAAP/XjlTGNie3Rh4AAHH/AP8A//+lD1IAampz'
    'DgC6hlPchTuzZ0WtmR3/qqp//wD/AH8AAAD5WBryNxz6Zhn2Rxz8dBfxLB3wNiLvKyHxRiTxVybz'
    'Zif0dyj/AADUJxt+AAC6Ixfvlk6TNy2qOCevSC13OTTXNxn2hTD4kkTTh0zWSRd4OQ2UOBe0NxLQ'
    'NhmUZjevVy7MhUuTWDKTSCrrZzLXKSKmKCTLd06xNibsdjL3h0bNRyyoJhyqRy2zRyvKKRrUSCf/'
    '/wCqVy/QRynSZy/yaUR5KCinJxvvZ0vQVy6zVyrOVxbSNyaqNxiWNiiPKA6OKSjVZiyzZTPOZjLX'
    'SBrSVirXNiOtdkt8VzHPOCe0Nyfpe0+qKiQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'
    'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'
    'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADJkiG2AAAAgHRSTlPbYBmmGhOa'
    'YF1cogYMod4ipRLqlgQEXNNpzLLwAQJZARUCASUW1gIBAQQCVlRknAgDAgIA/v7+/v7+/f78+vj4'
    'AfECsNERMC4I7fn6Vs4ID2zPDC9nERHR8DctT9HMkEcUTs/RARKuje0JNtSPT87PMCsTEa4ub+mw'
    '7g4Jr26RSwNTyVgAADGOSURBVHja1X33Q9vIt6+quzHEsJDdlO3feu9979mGIGRbJl8X2GAHHJu6'
    'jk0NCYTeAoR//Z0pkkbSyDh4f7hMCJaNdObMOZ85bUaykOqlRaONqNpYiDYajYXGNWoL0NSF/1lY'
    'kBYkVV34snCtNtQvcCRFF758+aJK8Kn643ObxMiP6pfGly8LpMGZajQ6nHpkLaqabUEdblwvNNSt'
    'qPplum+609dfQF6SBCLF7brRQJ1I9wsoOryAdNJoXDfQxRLhzrpuGHG6AOfAbzgFNdDddfQ62oUm'
    'pqRiDoiah5uNZnNBGkkJ97Fz/eMvv/zyBrV3dnvjaJyP8Mff/fjFoFS+YBIh1xlq9HGBZfjHN3ik'
    '7xyi+OXHRp90JfXNd6zgzA6+e6N+6X5lRX1hMhIK2Tz9m0p2WP03w3DoxQtyzr/fvPnRl7Cq0vMt'
    'gvSyN6rUDS3PohJA5bvvBlH7z7e3wcEfU5uEg+94FAbfLKSMx4QW6d0gb5Rv1D6HAXT5Ih58d495'
    'abz4gSv5F8P4uusXgz/8YX74B24m0+qCH1i+sxnBp5sXgbq6oGUTDOMbNIY/nsAFP8z+MDs7+wPu'
    'kfzmNjj1yZMns7j9MahitGz+rA6iPmefwA+iBSfASX/85530mNBipM4Gf5glrJM2iyXyZ7xPtMQS'
    'gyAToDxLZQiCop0MnnU3v1IcX4Da7BPakIKCC3iaRl8M+mjpT7HBZTrReEdhBVr+AZQO7Q9M+4fB'
    'F9f+aAGLBFPpjx+w4vO85vP5bB59/OrVq7x8RtCSevO9fS46gL/BwRMx+qjQMnwmI9Zn0QCxUF7h'
    'cQjJPkEfFZ8gmdgixYfoN4iIqN0XLTKZmLNY7paE5ToB2U9SUshzlARj+P6Mx7QhBb/3qBUPUxCC'
    'km/ckpBUwMosUe23N3yRsEM8TWL3v57MusnAoNbE5qMKcaNBIe8cIZFjsNifyWqIT3yEPDsb9/MY'
    'uP2+L8x6L83ndySJeipgmUt79kko9iNH7Wfyk1kuKzAnfNEiSaHBP3+YfRBSCMOogyAJ/zav/+sJ'
    '54xZWaw8JrQkoud5nmrkYLQ/tNRlPznnZ78PdkOLA792WzuvJiha9k/5tMFqcfKiaQCuH1jQKP3Q'
    'EgIX/XCwkJl3SsW42RA5UgZHVX9UGVGiKnPRcir1VQnYTCVkXwMOSt3qcm1V5F64KKYoWlQpKPhY'
    'LbkueehtnX3PU3p+fPw0KFX5aPmbtCAO9mFYzD526iMEeme8yZNfO288KrSoNa5W8zv9DcNIiWv+'
    '7n5WPjP8r5S4Vil/GjTRkqrtn/pYLTno4fvvko9pGYegRdrko6Wqhv4CsLyy0PArHy0cdv93588S'
    'Hy1itT+6I8E1f1nPfr/ia1yiPj4MrJ0VwUalc8HXaqnufEiUuWofF+QgDhoEHzeUz/eNFkGkjlES'
    'uWjZqT+qsCV1zY8YBbE/si+r3dCSfxKS/K4cSvGvhCDXQstmKnjKVSUYF3d23jgb5PqhVwAWwoQX'
    'LUZT+issC4QtZ3TSbYlrXNMjTT8qtETPubo57Tf6kna6zMz87KAa9fFFz1LcZGqctXaGH3mUZDht'
    'yzTyQ7zADPxQueKDlqgk/jnbr2UZh3Z6RgD5s8RDS37xPPqoCrmbPo7oVOqT8JncTdqzf75RfS78'
    'vzGRd+X4mhhj44pzYZxvtcQzZ4grohA37w1xBVmqpfhoeb4lyv17IYiix3fqJFeIJrjeVdivPiq0'
    'vKz7BLl9oiV6D1qeDDZ8MnSjKvOjDHGEPS14mvOJcxPsedeJ75+88glaaoYPWhZUea1/N5SHTnaa'
    'BC1VLlpyp/uxR4WW4aCp1XFnZtdnkNs1bMGu6KzmgzM/a5dwezpuRSwvi2xZceEdyYfGOfmQeY4H'
    'LWro+78gZsGdUNuh8oNc8PePCi1RbqKbh2H2GbZ0RwvgUdziB3hVV645Pk6TzTNXB4IHBHjxZU20'
    'o2GjGaIhLnviONJjUor6oWXzjBM5jTtq3f6xioVa9CKbaIGwhWdbwIJvPrYgN28FZePkFUB/1mf4'
    '5YlCxz3F4rMFX/x6LgDpiw23CbJPGjchlUOEpYrJu3o2SEJc8wR0BQaLLEmGH1qQF2WuwJIhv16Z'
    'K0wWcuxFJzt8Hrc+ks9Ug5aQ1vImJYvr/OJO43HZloacZ9FCpJJjs9WHVVs8Ycu4awaCx9jih93n'
    'VLC2esexyVhwrxSiOHd83OIcB5aY8AItQyeG3tF8yJoIZIwoaLFJudGCVrjGGXxZDaCQy4333hbP'
    'GwZJ4WhhmyGFTAtaRHpcm1vABuRZUULL51DY0tcwKkE5z0Dw1TgzP7GKx1+tidx9C0YdWyX7VHIl'
    'uPgvnvhGGGfQQo9eAeEv/4+aljf/mSU2wOqeoiXIBvGCq5aMkD7+iqt/AdrnXpqM2h4NuDenEVrc'
    'tPK50+D1Y8KKkaqfwnwZd06hXE6AbNXohyw2EPZEcooJ+4JxsAG8vCtal21rYAl2PGctQNuFlB3B'
    'RZyAIi+vkF0AlRUUtNg4snWelKopf9uSkPN8rHyW94LBfasF72nlsmKmeRDkjnPQsj/yqKotKbAB'
    '4zlWMRgufQ7DQFHdq3vMNPiWBncBWs6PM06D4hfQ4oLvZiwoYL/gxiKOc/GaxrsnedasmcgT5Lpj'
    'N4bgXtRc46ElJwjJfSlajS3UyC7j4eHr6+vhaDRaxXu68c7uagXeX0u1anUkZgfxRuN8zevBYEyP'
    'awHaiJ2vcRTZ7zCwMb/PqXuyHBMta3lbu68sVXlSKGNEOuVFEShyGUKEVJQP8byJHHSaKRdatnZ4'
    'SAeDmww2HjaJjC15zctJblEsP6qwJcEF/XjufKv/Su69MSC4Ik7itSCyaLFVHCx6Eu4mNS5eGK5U'
    'IGhZGXzC9Sifg1K0G1q4vANYTvcfuuMHedc8h2LwUe2bMyr1HVvcOUY1fa6jj4A7uTd5QCWXH3lW'
    'iXflZXDXg6zo/mmOa7TWQl9SiQTNbNyeyO2H3Gip8NEy/nDdGte8IBfs5T63qGUYqRRNpQzf1qft'
    'xyQwFfPofprG1pmc48BF2G+4B0DZ77WSCwbiXrTk8nJC4lklnrU7lbx2aFhKCj4wlAAtPA1x/JAL'
    'LYYUXOOi5XT/+UOVo4oyL2w5DT6qe4mMRlDmKfV0v08TGe0FLVirbgiMiFy0LO5Imx60gGI/+8FQ'
    'HQqZSneSk4PNzW5o2WwEF3mqzT18nRWCXC7JPSnmPRfd4/glIUmS+sVskoRfrHsE8ZGEPlXV6+Gf'
    'v0kxUVUyKahbX9QtqSEtLGzBJ9ICuokSukW/JPj42m34jAV+2LJTN2OE4QqiTm7GxOxJPd2L2dhZ'
    '66GMlcvHz6oeq7TIQwvfN0b3L7nd5NbEhBhnwZJLp8mJn8W6Z33KgZbp+g5lIOcMMh6+ziqx/p5x'
    'REnJY61UdeXNm++4Dd3BRw7wD26hlZUVQNPwvXeZgKeJAsLO1JVQiNIJheDnDfwL4Y/gVcQNXkOY'
    'ciIhLbBkIWzxgh5nq4AWY2grIa2smMS/+44QSwCZMwDns67i6SFsIVp1FWhTTd40RGjh1bGq4Iq4'
    '/eRk2eIgh4MOGg+nOabFiZaq5QpdaHn4DtrrM5mLlmD0mTfG/zr4Z68N39w3+O7Nirpw7w68aFtK'
    'ACy+fkWXcKk9efK9o8my6KyfVs52chy0LJ6jabQlrYTeEeqDNjVQw9evX9+9UdvTXSu5PVXIc3lX'
    '9d8Aq5Tjhi37MW6UDsaFSxg1VtW/CfgTCFo4PtaBllrdJyV6uHeWRK4jEoLeMZ3trOW/rT0B0IRC'
    'ia2uq8ANSQWkxP/8JuI5QRabzML/LhvkpplhNFJRwAqghE8eYB0Knfln2QuhtZ6WU1CA4Qj0jOd1'
    'Plp2JN4qp5GS9vgd5fMOsDx9imxQLv1Z5u2DdaCluc91zrnTevWvRot3c0vUDodzbsyTt86GPszn'
    '19bADiR8b7//cQugMhhfW2NWuWwC4+R/zhXTE4subhms3+ebyH0JlbYAKgy7Fn38O78mfw1JfjcF'
    'NRBa0r25ooYz16xzk2JhzydoiAYFH8qOAOH1HB6HIHMNhMDib4vvCnM7Dy9BSaJnBiAZntbdM8CI'
    'coulfsLL2bMuv/Y1dMaXEagyFP8zn3fb2/s7SH8WmwyHTTfosYZzl+X2GYCFIjGdTrN4sY7WBkNq'
    'wqcYtdPrUm1erhsOtFj4zTpdfNO3sJPO3tOXIL+9RIyn05/328p9aOFHpLl+SlB1mYcWCHL/7rEt'
    'O9+yxM2Sy+fjIe4NoLGzUHwgn/PYqR5oZiGyYgxW+5yHls9yIhH6c8A0WyZa0rlsLmuelkX2ZSCk'
    'cjfXNuunPUEXh6NizWBzYosjB1ou9/3cgLQncNFCkY4IyeFOfCAHWdFnWeJOP8Gxt8UybmlTIGns'
    'NqS+0GJZWiJOrArvnlxpx5S5/RtdkbamLFeQaaSO3EBoyHsz8haqJDDaIKTgirQz/nCQo59mL/d3'
    'mejUtgFUuNksTjIlcSDviWeclBGDCM48Z4lMVm+eaDy3uLPFGCijeW5yxFyey17W/ZxyMyhkXUN1'
    'xLm5rByUdgAs2exnOViL3oeWpojRkrYbEdxp/cGbC16KGC0Okggt3rClUj6lesDdph3NDymoZaHl'
    'cvGQ9Hc3/BJf19BESbNnmwTT7i5c3aUvpRHDbSIx1jHWoNc0si3yIotGB1/WpEvjekloy1Uu+BuS'
    'ODYQ948TK9VRz1Uto23NK6QqYa/sl4EpZW89lwiQhFjCpXh4DmAB7ch+WY3A5mTnGOkuuQFepQcH'
    'uSMwebIuksDPZdBdJDGUjkCgmk2nxznq8wMLaSBL1y34RuNMJmBxmimmZXHjwiWd3mP3xFUQ6HP0'
    'Ihst6c+LuSxD3duLOeVza/IZGwioX740XoL128OW13WJBTXHFIN0nYm7mwlqtJ1X+ga5xLiYEHaK'
    'JIeUAmApJ2TktgUhWB++Hy3NHUBL1s064LXx4NvEqgBAt/Khjz3P9sSYJNpQ7TLtfYxBGsmS5fKZ'
    'qoprOY72eqQoiDWGRRzkEriYaKEqzN3PGqgii8IOxrg0VkLYNZX3cjm3fNC7iTT6lPk8i7rfObMn'
    'hAocuQU1jlx8zVcXW+VLolALx7kcFTlceBlU6jt4en1O1v0gx3iin8s7yGl50UI2TDxgNW8TBSMe'
    'eQJr3luHF5qiG6k9QwXPl0X5bJe1u1JI9s7bnoHoYBEni6QojtHCWsms01T5UAOELYp2GSK6Eo+f'
    'o+pfQvbiDTi8mEm7SCEKADg720Dpq0cQYLT3/dGiNMXPLFqoE8KvwmVHqaGFJ5gnELRM34+WCgTo'
    'Wa7gpB6A8jfeppCUxCGZhbDFs1emiaZZuo/mnLzGFgJL9sHUhMt9R7Io5/go6K0HdFrOvpFUWvm6'
    'JgRhClZFmWedJiIRwQW9LAbcuW2St8554oKwxT9oMKqQRKfHKRBBC8LcZ+qbsttBSUpgiaEiru+C'
    'L+OJdveFHGf4gJZEYmVlZWhoxdXMD4aGEtLWVqNoeNEiLuZ4+Ot4+Kh1Li3l8nWQ7WogsrlFds/Y'
    'bj2+6BVnr9pFaCmzaEnIPSPZr4fcYqhNhzoEYBGCbQMtDOYILBwjmnsblt2mihiXhFUBl7gcCcFi'
    't4ld3rN7ArT8FkgKCC4oUlFe1s/XkAbmgpL/fgPBucHKK04wVmvx+CBtX9E/1OJsw5+EhjwlhUTs'
    'nIcW9KA2D1r2T3NdJW7KzkfjgBbzcTFYmKE4x7JYNLraAUixYJIyMWn0Z1HO3m8/st0AidFMFqsT'
    'oQG0F7GIC914qmdxIpA1Ce0dSrLgpTQO3kw1wZA45aJFjHW7Rwu7IrMjBJFwck4QwMgkg0obwjzo'
    '8ULudjedjZZNFMpxJIyrS85GF0Fcn8br7tvJ1OYOzxvw0NLc7z59s7Y2/NRxWrcCFwNMS7aHCe/X'
    'l+CIFhHos105wxrvjpYsWAYcPagA5By4DGkTBbkmiVzWng57igJq9ZrYcZgQkpmkLfLRkkp0QUul'
    'I1scCnNBJVZV3oJ5EZJhaau8k0dokYPST72gpSLt5XwmNfizXC7bpeE/XnZ+djk8tD2Ray+96bxa'
    'P5fnemnCxYSPzk7rTXu9wWla7OOJiXvoX6C2kewwM2yoIvuGQNDvxAXTJnzRkpUTQDOxhcCSE4KV'
    '56nfy6c5RsQTExNU5QpEGAI/NqPPq25zjXYa/Ge3TWbPy7bNEpIK2KFqLZxMJsO1moSdYnpOlGqp'
    'XtBSq19y7XSPzn5C3+6kXLH09T7fXnK2YAxHpeDbXlpSnhP42risW9X/LWvqEe7pEObmZFmORLqR'
    'jwQikUgy2akxFYfN4o4PWrKY5ja+gSoiJ5OBiCzP+aJFlNAzUlBNI3sZHEFLlULOMmbC3PwF4VXo'
    'SCkp+ZlHZTFE0tPUFpcjsErd7xaWgp9NiWwEK9GUUamg7V/NooKnV3ZiPqwkekJLc/80+xC7baNF'
    'cRVRNtsi14ILHQ5+o7XYsRKLHcdIU2KKoljvzFf0BymIFOKFNKDFtC3P23vOKAf/mphrBSH0Lx9C'
    'P8fHxwpu6OhYwf+hl2PlUDk8PFSUIusrh8qXWR+reyHLnU5ZOpSkUqkkSXBtOMjFC+IPAhdI1QDH'
    'qOCZSv3cSKKg4eJCEDYuk4FwIJujaDlJHaP0hWdccD3XSJ1xp2F27/ieNd6gPIGEAfBMHk1P/8+1'
    'tCBVKtfRWjk+gNCSlQ+njZ7QUhR9nPPERG9oafWIFsg3dn1yPHN7NbvXmh4Y9IxY9PhQvuBM8uxl'
    'uWnV/GVPTJzNbiTLtWqsGo36VI5wL7GYpFSnnVtJFkSBjxZ9IxkEmkAyKtWKaJdmVKoqQZ5dwGip'
    'TUM6NI5DkzJ6vN9/DwjCxW+/JZOA4sPwJUn5hct6Be914/W4iG8XM2LcsDu7Ica6o6V2SNEibK/W'
    'jC21Xk8kkIhr4qccBpGc+L03tIBq032gJSu03BZjuMyLhLL4Ob39tFrQY1wIWkwHVwNhTrjRom+X'
    'Htht9ZyLlgkAS6fpWfsGlyrzwLIhlrcSceQhYWbvtdHux//+7bdAIBwGW3asiDsfiLAg/vqd+AzO'
    'jCChcqLJC7sBvJ17RtLuXGKJCBu3ypBSj8fPyzVc7KJhHiRu1ele0FIp7/Ek0q3Zf8dTN+jebDXN'
    'Lfe59gI8oBWDGx5vmU1PtKzdXjUwBq6gC0TZKv7tgf2d84KEiYv5ZJk3l2tK8sJClxm6QuAstRMh'
    'HE4BWlCcbzSD4X/9618xpQnmDFKMHI0odrbAzO3WLy/4xqWWMobrOwIPLdvle7ac1s9x2itcBI6k'
    'rf34p4FP5/XmPyT4GHUNZmdRru/2gpbmPgpyYWwUBmkTEN0RQ9GSBVY77j0DMfE0y2YjdLpvd/p8'
    '4glGi0dan1vWYn0b0DLhyYrF4sO6TaB5xMwOUyZzQS5YUpKSnKPsmTKEvClyqCT2Fom9J2j5R00J'
    'h2+DO6H9dru+t2gucQpiG9czgp/5kcsZuMYEEawp0gkzFmh3X4hph3DiM3GxHT5uQwYCqe6neKhe'
    'Pv+EMzI06WWx3QtaJBxjcMxHT3DRdTD0nkmG/D0hA2eYgstuO/YC+MQvhrFpbKJGfjO3yBhGW6Qm'
    'Y4J1laAB04jWIcidIIi3oJrNxttDbMJA6D57Rsh3uQGtCMmiUyh4Kl0kywr3/Bh4SjJVyekIBReR'
    'MDibRaQUgpbjaKO+H9w7RXWq/S3QF8w34ojoPV/V/UtmeKYqkDrVaBPpasLEroXgDbHrFw4koqhk'
    'iXqfe6vUyqc4rM1+iIuinMvqlNdFOfG8J7QI2YlvbiaUdF1oHXm8JHIIFp5MKwTReKrL46W7bX/A'
    'LTUCcz2bdbORZr6coY69s2Unsd6yubiokq8JUyvfdEt3zSsZZCzm/ExkrJb8zPJHwCIpEEzhmoqu'
    'g50TQ6F4HNU0Pwh7UpngiJy9J5G0p9nayDqmKnVF52qihtFiQhdfhca40f3rKaTyHtgQHdAS+aiU'
    'dzAJeJdbhMzMVA8ERt0gJ5haUkQ6Qv2b0GLZlg3vWrm0xwhN1yn1jWSlOygqqjqEmrkONTSkqio6'
    'IL9XRGSy3LMd0AKJpxkvXaadgNaRHD7EQ2bzrHhBH1/8toI3OWjRJ4Sk4mMioxKKW+xxg3o23ioS'
    'BQv6CDREkII3kwCO4llzcoOBaBOy1c4GGxoSf5/GZRuFQYsl3XTaGws4JuJWCK9VAjdhpSQTT6Ij'
    '9lgtQWDU9lePhZZyi1yk6xMPaICWcNG7iOVBC/zeCMb8EtiKOrTyy4qpUnSDzldHw0tV8Thxvm4j'
    'd9kxg9xq/dKJeR0JE+Dy6dOAubRFl72+0mWvr7hHgGVx+BlnGK7eYFSQD9V8pFo9agmW8icwWFql'
    'NjL3VAownz98wBscs4LQOgSwfLAFtdEx6wDl1gU7IckRqPP817bomi2YrtAqd7u1qr0fJ4V3QT78'
    'VfxkazudZSZzNscut/l6ovJlVn8wWsARJZUTt+PYl7O8+Kbjg5bhoRUEEWSgP5H2wWy53Aem5Rym'
    'RcccQ0pkJdAL9W0HWvAZtL6ew6Qc5BD5xcVPnxCIEGDcjKHahmsU2Jb6qeb6aJvhD7QDfqgdQosH'
    'luG2lkw2WofHCRRNWPxul2tWMD9n226rY1QTScgetOCou4tpMV6iYA4xrsvhBAILq21dtw5QYPTy'
    'XrSATHTWBuhuLesmSfyiuxqgpeK5QYhYcKfihG1+1UNFN3OAcf7wwbECRU2lfaRndTdz2A4LslV/'
    '3e3IDjFSDtIM1xZJ+hZ/AMZnIO7enW8gyXCmB2R2PiJVy9s2gzDgCzmsJOIfcmlmLurU6mxDphQa'
    'YNB/0SpL9iYOT7cIaf5oaXbx8BXwXjpxAsiRZTEnumW7LNVmhbg6fR9aqj5Brn5/MkTQEqi5WW2I'
    'wgQHLdzvlSNY+cBqkG0udHKYmJgTrdsIRsrbvTFNiNnkcYoQV9u/OOsAPMlctEox31uoZItFJJlt'
    'kW40YuawOXOCx2j/Qtr20xetIzMnUeqtDa820ORHM9tBCL/IHf/qY7Sxf0rAIgQOUQTFdyFYBrLj'
    'vjsuWkC1OscPZTLz0ObQr5ku7ULYuK0YriwYTJ/uRcsGZ7sCuqMw/imXdclAdx10Uz/MLGvVeLrc'
    'useheqDHGJ0BgMuwM8jl0JoTFd8djXCBzoBl9VcRDy7rRvqFLCptkg5ZaJkbs2ddo7PNmxlZWRZ0'
    'DlpaJd+Aw1BfxAXczdV8AGPNXzKQsJXVrmgxUm2iWjeZ+UgkgBp5eR14/Rq9oBZwtaOo+95DiA55'
    '0TDHu6oqtiu6PxbuR4scrjCp+4ZTL5YV4RsuegIR1oeBUF1lQF/e4zE2559/tG14AVgi4V8TOwOQ'
    '++CtjfMzDrC023ieO9FidQ2o39C91oX4UNfoIMiNHPl6kIoU/0S6mXn6WvADCzGy2eylb7QsWEGu'
    'zkGLfrNawu0QN+VQURS0RAst7GxH7g6Mk84l39+792Y9A+B/ymUflItZZC9kZq5XVzcEDlpcfs1j'
    'bcwcJv6iwYQtZZnnorcPfRfwytQDY7DIY5DznAqCcDG3sd16OxaZMCsJyLIopk/RTSu8LSq2ja52'
    '5jihgO60wabxnXtb7HJzyECWmJbC0w37at07EVEcLfiWXAQzMOSZ24mNMaVaqwI2qtNKVYnWatHd'
    'WCy2Cy22ewJHVfiHfmK7nrvJpmscC47ClrLbCCEvZGElk8k8EC1McfC5Etj41tzOFiHAxb5L29jl'
    'BbkTunzouwZZbmFKGTTag7HDdmdP2JiXk8FwuKQcjllCkUVIh1BqkbG8Nc4YWXdSilxwQnov0tHv'
    'SHjXJ9zYrLyIZ0maPDODrtZ0nUgZeHTKm0Tfe+Xu9xM1gxsc6er31JK7Vu+LBC0ZN1pcLsto7jNg'
    'caBF/4ZYey7Ilj8qq9tC71ebJ2Vw52CLL+0kf7rIDVs23vqGLWpCJrQ0AEvgUCkHk8nA2/BhLRZT'
    'sJciyp0XE+0Eok06pUwIrfJzxq61b+d688PI2q36fSdLo0wlTMGCEJJBYM7MZEzcsM4oe+m3WETR'
    '0t4jqmUvBXrb5QevFkslHLY4bIWOS75O3C7goWQmNA1LTdM0j29g5UWmBkd9znBIuj1gCHlPz/A+'
    'yJhwEURrIVZttwTO5fOrvvmHCikRJqVdHQRKRanc6YDzrtWsqhYe2FwrAbmzQDFqSgmmp/TScSeE'
    'POPhHimavGSYAUKS5lMtlJriAJ2OGQoW0h3KT1xoIQIW9rb4tASaRezhLjNExDoVndY6fPBDbVHV'
    'wZyu1iD1qw1XbW66HcJDgc40jAQNvWImNA8q8N/RrwwrO/zGFQ5VlcC82bOfb8NKQpoj2qJixEct'
    'aylLsfzKhD0aPXOz6rtLbQuZDxgNgCVSKtYUpQ1xX2cfP0y00hEIWoSW+GsiBNOE9mkStiu5NAYK'
    'zOi2k55gz2XhjdAS8UOL2rnE/WCzaY9yYmb+9et5hgGLtJ6ND013QYtS2iYyoWgx4eLZDtd7a3Y2'
    'WLSAhsE061dJ1765YidugoUAQUOnUT0iuNgq18hHuu4ZIZzTcsURtdLbyLzZvZkPudIjDXWGfplE'
    'LLTcWJJPQBY74ZFnJlLy3QaytScQPq8iq4pSrouiuIeqxOixjCe3G6T/bTGRQDNecxHe7jgr4s3V'
    'eYoWLw/Wod61ktt4gYu45oSgwoP3M5Hw+5sr9+iQ5FGcu9nFthC0gM6w0AAzGUR0vtsK031oEamZ'
    'JYYAwSCja1fJo2FXKUuguqdoQdxO4I809yg0/KN7ZZeZmHOvaRq7tXAgMm/DhdsIVkxqJnAALYfP'
    'DLPmscGR56inYMCYA5nQnlkPH5ZFvNL84cMH8p1QtQAODGa2xwAsME0YrGJOL7ZLVZc/j8wgW6Zl'
    'fNFi4azCX0+R4th46+ZVpEcE5reHx+CwvcPTUZzb5fktyioNW4CURTCTOQg+HC3tFkGxZgIQoSFz'
    'lYwlHGX1clz3oAK3mZn5dWjzpMHBOq0QekSF2nzQG5fFFISX+RnbajgkgjnS2L/RT+D/TelngwG9'
    '5+r526PNe9HyNLAX//ThwwAkr7rwW+Dob2BJW7gyciAm0LqzbgOU6vwiWXJFz83b+YyueSeObmuJ'
    'uJVIh/vYlF+KL2hH7KRDE3c+XIrFVm+uvKLH9WKlC1roDKfMm6RvVmMP3uBWNtHCqEKfOXA+5seo'
    'dgSdBxVUFWTLOW/Rv7dv0e0bMzzlR0re2wgM6eQoHI5EbuCieV47QPDLuNGC4BL5+JKyWWpxHB+g'
    'peI7ixKyVRceAKQQe3n19Bam/jCYcB1Bu6Qk4ii6ybhwMH/bdFbYjMrqzYyueeCiUXZtiQW4ldzh'
    '5ov4AMDdPSUBLIFlJZY6anHQkkFxPjeJFujmFqw0zYWW1tHDt1uD0DQOWm4daR5a3Ne9DmfmILB6'
    'qBRrqBWVIhwoinJ8XCseHR6OHeg8tCjcOuZ07KRUOgyHV6E5bhu6pS2yPsP2bAog8pFu1ositHin'
    'dmTVr7aRGgFRYp9KKv2U4kwgHEu9rAJatMx8pFxriwM6M9OtSbJadG/jkCBc90WL/Q7QwuOoqAJY'
    'oCe3kLWZwB3aeFoMHnDQAuzL4omvbQGZZJxoQUejreaDE+jfIY/MuNEyMfN0dZgdVO1o21aG+QLJ'
    'xO2Rn1E7HhM4rmsm0O1GmljlH8WTYrHYrBVrRdIq6KdSO/4Ynp9x9E0OIh+pBz6BYWgctFieysug'
    'KOBAHeVa1uzXZsJHRqpaXN2A0O2mo0AOmvUAAGZJxO2IjFQFXBHfVzuwMx9UOA8mN9o4lPaCDdwk'
    'DuTBFc14yQHaBW7JRSAV5m3WEWWIOc4cjB0/2LSMmNGrpjHzVgg4d1keg/joKTZStYOAL1iwFfQK'
    'bz5cexibFSVwpWmaW542WhA4eWjxjeeklpc/GNJdMWUUlYBwdRVYbRrq/qXuVRFYHcVLthTJaBmt'
    'K2AgivFYJZqYxrM4nXdOBggKA/R8iKK5aMnu+T4vF0I5zWHb0CuK9R7+vV4naIp52HChxSgSR+RE'
    'y9XNquIbFYAV5EgucvjQqtA9aGljv+IexvyYb2VBOdzmomUVjMZuGNACalLwir/3pMz8W870bI8d'
    'cDh0X8pL6YfVessRHVk0CsCFQVN0MC7YOjhwi4xLkYsWHLYQfLAsaVeR0sPv4oBJyXhV02YJzgIO'
    'hC1WCMmgpYtp2S25lIHfzIB2H2hbliNXzqoLoVdMke+bLYqejAhOnfdfwKtBxs1Byw1ahNwNJ4XA'
    'bbGC7gt1owWbj3nefrza6k3GByx24AI+zHvpr/WQoHOTiPXAEQ1LKkeRmYwDjMRu6EKrNMy3LWU6'
    'YU2FkasOAg9fJIL4QmOhSknqrRMWLSer27oD0mQeBhT/W4lcyiAXgQV/6K1JpRtdy7hyE0BDjQ4D'
    'TNmEWzOQMoVjXcpMjDu3Jl4AFcWL4WTy9qhiGFZ5gdW7Birn7ceLlog+u0Yu87dFrwhUkjw7uUEH'
    '6wF7o2Px7fwMa7vokaZvc/YtINsyXG7xih7zYeXhaFEimgOrFC3bJTaDbq4eaBmObfHrd3gXrVBw'
    'wpaHBlhRBTjQ3BN2Zv62apaDEJw1J1xQSuRHcLrNEyUekpF6LgWD+DtHzPKCY0IjtBxWecuzt1if'
    '96DFu6SIdg1leQ6vEHlvo71IXZELLRlwRZ69s9i2VDrbPGZYmt+MlpITLWY0thE8ag+pZqG0eHug'
    'eaoH2hWgemh4M2pMk68624xuGs/QLSNKWw0NcMOWsB3AG0Pc9uyZ8x28/z9DxWLHDoM0q83chIns'
    'jZi44RIicVS+YVKztM1L2Q5uEVogjpCIuS5v664cGLUrXpALTIBxuQcsMzeeBWgjpYbspQUWl5Pr'
    'YcXWrCQFDjK2cbEPhFa5yUNLEWSi8TjY7QctGR5aspfBI/ver+LtleUGbLTofo8KaAyFBgY8KYqG'
    '5vq9VnBkxH0fW6waO+rEB3QPWKglwLUxKhlnmDkT8M3AlI5DlPTw6oAw+IxWhaZdOSjtwHSArvYc'
    'nMW9aOl4HkAcC1E/5IpKtPXAcozdMXJ74DBdJlr0jY7C80QnY6McZgqR0oNrc9MmWiadaNH0gTi6'
    'B4zuKi8Griz+bFOo63uhIXWorKrlcr2Ov6kM2YMQ2ueNJ4sTLzghsM0vPrG3Fo/HBzimRTu4pQHb'
    's3LryloNsPucf9stwXegBS9tQZZXcjyKodrZYNBi4fFglR8oFlcj93iimZvyT4b7zkQ6ExxIh6Ol'
    'yOqJ49zSjYUWpjaL4lzPfd4Cykwio5yViPXww1MiI/URo0WbdKoWFTnRnb+ha4qWmQwjLhtSA3Fv'
    '+/TJNKxutGg2Wop1dCJt6L4kdEvgJ2+jj8pj6rSMaTGNqjEMNgB9Munsc95/QQQl+BnXkgyK20uO'
    'UnORlBfYyjnu1+82guLb0akucNEykL66vkxxWA0JupbxoqWwHj6JuajPZ8yUmD1T3xabXrSclG44'
    'rABajlP9RLnER2oe1QJgDmimWAwUvGhBfGZdD0D8gBbnTKdBTZFN8WDMnJN/I3vTemrQjWvWUeHO'
    'BErk6ZNGDTkiGMSkw6FCtuq3XXoEp1ia07YwxspecXWhhYQtfruJKneRrmgBCbjr9O2QuVyrTU3Z'
    'o5xagnyo6k7RIzOMZSU+gdRzFS9aKqs3btMOp09F7k76QMvJ2Dx2RJSBSdxoPAuTk6ClFpiZxH+z'
    'O6dX8NXL6JWN4W0LPl1uadqU1kOzO5zEDIBMp6bQGxQ+nPzdrJsLFloI/8i1zgQO/YqHJ6s3VEeT'
    '1oiB15tVxyJklAa5GdCj7a40s8rDS6IDS0xezsrLZ/kXrQ/RMU4RoE0S5BReh92ZzvBhoACDn3QE'
    'OEiIesv9De8CBFFjo7ZCKSMaQsuDyy2ojBaeRwBFzE5NTjpoZyYjq78TtITX0TCcfyQi7qJgcgwc'
    'TtJx3Vh1zEp5Gyt3chJkNEUavXpqcjLDUHJ0CBeQUxHLhcBHOqUMpaVTtKBfkxjZvsEoXpFaxWHL'
    'pDlkLPSMu3T2j4RMwYKHQAQE0eetr8BLt6NTmg0WW5xEWFMuH2ZM4+SRDA+PbJKCZWmUUxYBNSxp'
    'mAk0Y6hUprArUv/uQku0hNDi0CgSz2jkY60PtKTCETJ1MK9OQBTM0uvJ3TozEW3dZYiOsLgnJy3r'
    'lNGIutDgGRFcRZafm+oFYzA1mbEF2q1ZPU8ROWHdaYXR2yJ1NCpdZsB/ttHSxUUXUQXbOh0f4PpG'
    '6Sfnzl0BU7XQgs7Wrtbf+2Z2xVUwLlSHzOCItFDg6gwxVeqHJs2BEVlNalOjgeUTDvXXFC0m45Sw'
    'ICpu21IpRRiwmHxoowFluh+0lCIFDFQqE1M0DrQoy5FCL6o1tWrKeMrWLnB6YJcpUHbXM1omTRCb'
    '0wpdOTMaODLdxi4J6AiaKB+grvX3PtPIMEoRXSOWjUXLuuvxE0rLxJQlGaB7tf7R9xEJ1aPAwRW5'
    'RHPxj7zY0phzwbhJgxbax6RG0DJ1tRS4K3rr+ZWjwOgUoWYyPoUxoG//qrjQokDY4hWkNvrwSi4R'
    'SXh9ylTDJI61CFqAqYI5OQ0lMOpS7ZSfZrGnmMyYM0Wboi4uw3KqjCHL0zNaLJRYtmqmEICEyCwe'
    'rh5oU6zuiWrX3/vcimFMI3hNUv3T0eAsxOEnpvH0zJj4JK9TGswh30fVGtXVgxlrpnimEqQ5zp1o'
    'KHm2DCaFDLJBT8NHnE1OxsntwRKBocU4vkAXxtxoQeV3Lwt9pUQYsO8jk1garC0gjNjqLd5FCm7d'
    '+Wt1qVAoaIx28XzILI1acz2K9IAd31RXeDigMsWCuhBhBK/cuiSDVbsU+Tjsg5ZhlC9Muj1RwbWs'
    'tFUiZ9loAXanpgpdFtKN1FGg4CuewjpbqMFFXN2EIx0cuXY9EOZ+qZlyhB0da2hJKKW3EooTLRDk'
    'Zlw8TE1qk5H3/aFlWhlbJ/aP1S5By53JwvRJeL3gmuhTfkqG5O9p4UpzogUUfGMpWAI92Njs0nho'
    'AWAUQLHM4kgpcqVNctCi+OwRmz5BKxlOpWK0OGNQtAUhY+mEHMG/9bFuxrxCBMUVTsERRRvD2A9Z'
    'o6MdwU/htd9aThGosxbZFIkuj7nQUsbzkZhDyvgUQrpS7AstRmw5sm67F0ZHS5FVa3C7HwPrk565'
    'jlhxywXC+UAYgvcpt/5AVk0rgT3QJhk0saiY5GKGOU0D0/U6zJRSXpoBHcUfiZ/AMhZ9t1Z50QKD'
    'KwSO/umYyRBcTTosEG7rd11d//Lrgp+1LASWmRDz13JcYCJcsyNtsvA0XPT5xgf17nVhhhOOQJzr'
    'iJ+F6fKNiRa2rfcZ5CKpvKdwceTQoPZIyZb3idMXuSe/Ayzvj5XXS+Z8xF4cBULr9p7cGszabt6s'
    'i2cCE1BYB7Awk+8fpRvNATWcsk6Nrvp9JcuuguHlGkCh4IgA0W6iUc3FAfrv2WXpdEXF8CgTFDva'
    'eoAJWxS8gcs7wkxh3X/f0LQSKBS80R6EOq0xdrTCyOoNByxT6+FYqt92DHCxbYWF89GxExuIlZO7'
    '15Dts/GBbV8sjwP2/2kgrCjHgVHLtJp6ng/TZQ8j1Y5oU5MuPzQ5NdnNHVnvCgXo4XjX4TFu7CDE'
    'DJNQHcrvAa41EraweEfRljMCNFAobhrbKRutV4GjrtMTptXS1GSG42ULjkKNincSTzq9LbFA4V3f'
    'HlBEwLBuXqNp8hhbMhRi1Hq7WuRut2+0RI/fB8B1OHUHaLk9ZrgeObkLjBYKzgnjlQioMqYYsfC6'
    'rWv6cnBn+bWyZQx6bJO4eoegUgA/d1QcccxmbKkYtEwRP+q7J5fmC64+ltyuCxycmw9NmwG0PO9q'
    'qZcDS1NTvNEtRe6Yb+cst8C0cIYJfujE/96w3eXIEmc6TWmjcdYVCWg/kOc8DTj4Z99oSU1Xj8IQ'
    'mS6hZtZVQXrOOniqshx+vQ76KiD2zAq1xQkSx9ISGBaU2Mbeo0Fhz0mrr9rSzZ1JbgRVcrXM1dS3'
    'wEVDPa9DtrBcHHHdm9FCCXTGkbED+wHfPbkntxsaY8mog786WHXUUYZLHtePb5q+rXS/wQJc0RIy'
    'GpoXLcye3CLeAYQFbQ8S3oAf6hoWnSCrzcicHmhaPHTM2Jbi6s0oSk2dbTSgpP6KNoxuGAw8ffq0'
    'gDCDKCPNO2/GMXZPlsOB10/X7e6XHBytvw7ckf07VSUwirFHz4IDhtPY6o3Zh0mncH8D5oB+qbjr'
    'Toel5MGSm8rSUuTWb/lMjQUOrjz8L10FnHerVlZvrlwsoNMDq5V7wsBlNHYyamt8qI3eWo+kN34u'
    'tzZmeKNcD3RfyKndRUZ51x1sd05YtKD7P+nTwewW+AscEa1YFY/C4TDuIEJI3x4Nux9RN32i4JMC'
    'gdF10p7Shi4Iv1eOiShfnoTxw8xMLtGbOysqqJRu3eOgjzzDBxH6jj4EzXwYWjh8twr03fbCeHlk'
    'UYtEbGrgr/wsaSUcMLmL4Isw/4FwLeoszt3aDNg8hpV7brAwTlYDgYglRsIXamE7f2aZdgmBV8Rl'
    'sVgDyRJ6EfbCyBi7N1ZIFU+Uj8vQPuL/H1FbXlZO1NRf1tDXUSnLy+/v3ofD798vH3FzipHdXdfj'
    'yd6/f49eILa1N2TEdpVl3D7C3+/u7lZLR0VbVkoJ/qIs3929/2gNadkeFvqQ/Eaf0T8qsWmfiVFU'
    'juC096gb4Bq3EgjG/7sldoH/9ybrcB269P3d0YnLTFeKyyY/qOEO7kpHlR62nC/fYbK4mQMsKSwO'
    'itKRQsna7W5ZKd43+6eLyh1zBT4GSnBl1LXn/39RG8EPKovR55VVoqnH1IaHHbs5K5WRf/700+Nh'
    '/5+E1+hPP/3k83xlgX5jB3gDphnGX8wIom8Qyv60DfTn6WlFoVyQA9cFTkahubvprRl268I0OpGe'
    'P42HYHSXjH0FkSh+yxOGk1HjPrqWgAgbJkdcXU1bn5MzCRtGL9QJv9Msd07q/x9JuL31IbC9WwAA'
    'AABJRU5ErkJggg=='
)


# ---------------------------------------------------------------------------
# Command line and window
# ---------------------------------------------------------------------------

def main_cli(argv):
    p = argparse.ArgumentParser(
        description='Turn one Flash game into its own Switch homebrew app (.nro).')
    p.add_argument('nro', help='FlashNX.nro, as released (1.9.0 or later), or "latest" to '
                               'download the latest release first')
    p.add_argument('swf', nargs='?', help="the game's .swf (not with --update)")
    p.add_argument('--update', metavar='APP.nro',
                   help='an app made earlier: write it again with this FlashNX, its game, '
                        'picture and name kept (in place, unless -o)')
    p.add_argument('--name', help='the app name (default: the title in the <game>.swf.meta.json '
                                  'FlashNX keeps beside a downloaded game, else the .swf name)')
    p.add_argument('--author', help='the author shown in the Homebrew Menu')
    p.add_argument('--version', help='the version shown in the Homebrew Menu (default 1.0.0)')
    p.add_argument('--icon', help='a picture for the icon, a file or a web address '
                                  '(default: the FlashNX icon)')
    p.add_argument('--files', help='folder of companion files (default: <game>.files next to the .swf)')
    p.add_argument('--address', metavar='FILE_OR_URL',
                   help="a Flashpoint game's original address: the file FlashNX keeps it in, "
                        "named otherwise than <game>.swf.base, or the address itself "
                        "(default: <game>.swf.base next to the .swf)")
    p.add_argument('-o', '--out', help='output .nro (default: <name>.nro here)')
    args = p.parse_args(argv)
    if bool(args.update) == bool(args.swf):
        p.error('give the game\'s .swf, or --update and an app made earlier (not both)')
    if args.swf:
        name = args.name or meta_title(args.swf) or os.path.basename(args.swf)[:-4]
        out_path = args.out or safe_file_name(name) + '.nro'
    else:
        out_path = args.out or args.update
    try:
        if args.nro == 'latest':
            shown = [-1]

            def progress(done, total):
                tenth = done * 10 // total if total else 0
                if tenth != shown[0]:
                    shown[0] = tenth
                    print('downloading FlashNX.nro... %d %%' % (tenth * 10))

            args.nro, version = download_flashnx(download_dir(), progress)
            print('FlashNX %s downloaded: %s' % (version, args.nro))

        def copying(done, total):
            # Only a big game takes long enough to need it.
            if total < (64 << 20):
                return
            tenth = done * 10 // total
            if tenth != shown_copy[0]:
                shown_copy[0] = tenth
                print('writing and checking %s... %d %%' % (os.path.basename(out_path), tenth * 10))

        shown_copy = [-1]
        if args.update:
            name, had, now = update_app(args.update, args.nro, out_path, progress=copying)
            print('%s: FlashNX %s -> %s, written to %s'
                  % (name, had or '(version not recorded)', now, out_path))
            return 0
        entries = make_standalone(args.nro, args.swf, out_path, name, args.author,
                                  args.version, args.icon, args.files, progress=copying,
                                  address=args.address)
    except StandaloneError as e:
        print('error: %s' % e, file=sys.stderr)
        return 1
    print(describe(out_path, entries, name))
    return 0


def windows_drop(hwnds, on_drop, on_hover=None):
    """Make the windows `hwnds` drop targets for what Explorer and browsers
    drag, through OLE's IDropTarget (the interface browsers drag with; the
    simpler WM_DROPFILES only ever carries files that exist on disk), from
    the standard library alone. `on_drop(items)` gets file paths, or a picture's
    address; `on_hover(True)` hears that something is dragged over the
    window, `on_hover(False)` that it left or was dropped. Returns an object
    to keep alive, or None where this cannot work (not Windows, not 64-bit,
    OLE refused)."""
    if sys.platform != 'win32':
        return None
    import ctypes
    import re
    from ctypes import wintypes
    if ctypes.sizeof(ctypes.c_void_p) != 8:
        return None  # POINTL by value is declared below as one 64-bit register
    ole32, shell32, user32, kernel32 = (ctypes.windll.ole32, ctypes.windll.shell32,
                                        ctypes.windll.user32, ctypes.windll.kernel32)
    if ole32.OleInitialize(None) not in (0, 1):  # S_OK, S_FALSE (already)
        return None

    HRESULT = ctypes.c_long
    S_OK, E_NOINTERFACE = 0, -2147467262
    DROPEFFECT_COPY = 1
    CF_UNICODETEXT, CF_HDROP = 13, 15
    TYMED_HGLOBAL, DVASPECT_CONTENT = 1, 1
    kernel32.GlobalLock.restype = ctypes.c_void_p
    kernel32.GlobalLock.argtypes = [ctypes.c_void_p]
    kernel32.GlobalSize.restype = ctypes.c_size_t
    kernel32.GlobalSize.argtypes = [ctypes.c_void_p]
    kernel32.GlobalUnlock.argtypes = [ctypes.c_void_p]
    shell32.DragQueryFileW.restype = wintypes.UINT
    shell32.DragQueryFileW.argtypes = [ctypes.c_void_p, wintypes.UINT, ctypes.c_wchar_p, wintypes.UINT]
    user32.RegisterClipboardFormatW.restype = wintypes.UINT
    ole32.RegisterDragDrop.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    ole32.RevokeDragDrop.argtypes = [ctypes.c_void_p]
    ole32.ReleaseStgMedium.argtypes = [ctypes.c_void_p]

    class GUID(ctypes.Structure):
        _fields_ = [('Data1', wintypes.DWORD), ('Data2', wintypes.WORD),
                    ('Data3', wintypes.WORD), ('Data4', ctypes.c_ubyte * 8)]

    class FORMATETC(ctypes.Structure):
        _fields_ = [('cfFormat', wintypes.WORD), ('ptd', ctypes.c_void_p),
                    ('dwAspect', wintypes.DWORD), ('lindex', wintypes.LONG),
                    ('tymed', wintypes.DWORD)]

    class STGMEDIUM(ctypes.Structure):
        _fields_ = [('tymed', wintypes.DWORD), ('handle', ctypes.c_void_p),
                    ('pUnkForRelease', ctypes.c_void_p)]

    def guid(text):
        g = GUID()
        ole32.CLSIDFromString(ctypes.c_wchar_p(text), ctypes.byref(g))
        return bytes(g)

    known = {guid('{00000000-0000-0000-C000-000000000046}'),  # IUnknown
             guid('{00000122-0000-0000-C000-000000000046}')}  # IDropTarget
    cf_url = user32.RegisterClipboardFormatW('UniformResourceLocatorW')
    cf_html = user32.RegisterClipboardFormatW('HTML Format')

    # IDataObject::GetData is the fourth entry of its vtable.
    GetData = ctypes.WINFUNCTYPE(HRESULT, ctypes.c_void_p, ctypes.POINTER(FORMATETC),
                                 ctypes.POINTER(STGMEDIUM))

    def get(data_object, fmt, read):
        vtable = ctypes.cast(data_object, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p)))[0]
        fe = FORMATETC(fmt, None, DVASPECT_CONTENT, -1, TYMED_HGLOBAL)
        medium = STGMEDIUM()
        if GetData(vtable[3])(data_object, ctypes.byref(fe), ctypes.byref(medium)) != S_OK:
            return None
        try:
            return read(medium.handle)
        finally:
            ole32.ReleaseStgMedium(ctypes.byref(medium))

    def read_files(hdrop):
        count = shell32.DragQueryFileW(hdrop, 0xFFFFFFFF, None, 0)
        paths = []
        for i in range(count):
            size = shell32.DragQueryFileW(hdrop, i, None, 0) + 1
            buf = ctypes.create_unicode_buffer(size)
            shell32.DragQueryFileW(hdrop, i, buf, size)
            paths.append(buf.value)
        return paths

    def read_bytes(handle):
        ptr = kernel32.GlobalLock(handle)
        if not ptr:
            return None
        try:
            return ctypes.string_at(ptr, kernel32.GlobalSize(handle))
        finally:
            kernel32.GlobalUnlock(handle)

    def items_of(data_object):
        files = get(data_object, CF_HDROP, read_files)
        if files:
            return files
        # A picture dragged from a page: the HTML the browser hands over
        # names the picture itself, where the URL may be the link around it.
        html = get(data_object, cf_html, read_bytes)
        if html:
            m = re.search(rb'<img[^>]*?\ssrc\s*=\s*["\']([^"\']+)', html, re.I)
            if m:
                src = m.group(1).decode('utf-8', 'replace').replace('&amp;', '&')
                if is_web_picture(src):
                    return [src]
        for fmt in (cf_url, CF_UNICODETEXT):
            raw = get(data_object, fmt, read_bytes)
            if raw:
                text = raw.decode('utf-16-le', 'replace').split('\0')[0].strip()
                if text:
                    return [text]
        return []

    QueryInterface = ctypes.WINFUNCTYPE(HRESULT, ctypes.c_void_p, ctypes.c_void_p,
                                        ctypes.POINTER(ctypes.c_void_p))
    Count = ctypes.WINFUNCTYPE(wintypes.ULONG, ctypes.c_void_p)
    # POINTL travels by value; 8 bytes go in one register on 64-bit Windows.
    DragEnter = ctypes.WINFUNCTYPE(HRESULT, ctypes.c_void_p, ctypes.c_void_p, wintypes.DWORD,
                                   ctypes.c_int64, ctypes.POINTER(wintypes.DWORD))
    DragOver = ctypes.WINFUNCTYPE(HRESULT, ctypes.c_void_p, wintypes.DWORD, ctypes.c_int64,
                                  ctypes.POINTER(wintypes.DWORD))
    DragLeave = ctypes.WINFUNCTYPE(HRESULT, ctypes.c_void_p)

    # A Python exception must never leave one of these: OLE would get back a
    # value ctypes leaves undefined, and under pythonw, the double-click,
    # where stderr is None, not even a line saying why. Each method answers
    # `failed` instead.
    E_UNEXPECTED = -2147418113

    def guarded(fn, failed):
        def method(*args):
            try:
                return fn(*args)
            except Exception:
                return failed
        return method

    def query_interface(this, riid, ppv):
        ppv[0] = None
        if ctypes.string_at(riid, 16) in known:
            ppv[0] = this
            return S_OK
        return E_NOINTERFACE

    def hover(over):
        if on_hover:
            on_hover(over)

    def drag_enter(this, data_object, keys, point, effect):
        effect[0] = DROPEFFECT_COPY
        hover(True)
        return S_OK

    def drag_leave(this):
        hover(False)
        return S_OK

    def drag_over(this, keys, point, effect):
        effect[0] = DROPEFFECT_COPY
        return S_OK

    def drop(this, data_object, keys, point, effect):
        effect[0] = DROPEFFECT_COPY
        try:
            items = items_of(data_object)
        except Exception:
            items = []
        hover(False)
        on_drop(items)
        return S_OK

    # The object lives as long as the window: reference counting is moot.
    callbacks = (QueryInterface(guarded(query_interface, E_NOINTERFACE)),
                 Count(lambda this: 1), Count(lambda this: 1),
                 DragEnter(guarded(drag_enter, E_UNEXPECTED)),
                 DragOver(guarded(drag_over, E_UNEXPECTED)),
                 DragLeave(guarded(drag_leave, S_OK)), DragEnter(guarded(drop, E_UNEXPECTED)))
    vtable = (ctypes.c_void_p * len(callbacks))(
        *[ctypes.cast(c, ctypes.c_void_p).value for c in callbacks])
    target = ctypes.pointer(vtable)  # a COM object is a pointer to its vtable
    registered = [h for h in hwnds if ole32.RegisterDragDrop(h, ctypes.addressof(target)) == S_OK]
    if not registered:
        return None

    def revoke():
        """Undo the registration while the windows still exist, as
        RegisterDragDrop asks: OLE keeps a pointer to the callbacks above, and
        they go when this function's objects do."""
        while registered:
            ole32.RevokeDragDrop(registered.pop())

    return (callbacks, vtable, target, registered, revoke)


def main_gui():
    """The window: two steps, the game then its name in the Homebrew Menu,
    beside a preview of the app as the Homebrew Menu will list it.

    It used to lay every input out as a field of its own, seven of them with
    four Browse buttons and a hint beside every label, though most are found
    beside the game. Now what goes in with the game is a list of what was
    found, each line with a link to change it: names do not always match
    (Jonathan, 2026-10-16), so nothing found is beyond reach."""
    import io
    import queue
    import threading
    import time
    import tkinter as tk
    from tkinter import filedialog, font as tkfont, simpledialog, ttk

    if sys.platform == 'win32':
        # Without this Windows scales the window as a bitmap on a scaled
        # display, and every letter comes out blurred.
        try:
            import ctypes
            ctypes.windll.shcore.SetProcessDpiAwareness(1)
        except Exception:
            pass

    # Drops: on Windows through OLE itself (`windows_drop`, nothing to
    # install); elsewhere only if the tkinterdnd2 package happens to be there,
    # Tk shipping no drag and drop of its own.
    can_drop = False
    if sys.platform != 'win32':
        try:
            from tkinterdnd2 import DND_FILES, DND_TEXT, TkinterDnD
            root = TkinterDnD.Tk()
            can_drop = True
        except Exception:
            pass
    if not can_drop:
        root = tk.Tk()
    root.title('FlashNX: make a Switch app')
    # Fonts follow the display's scaling by themselves; sizes in pixels do not.
    scale = max(1.0, root.winfo_fpixels('1i') / 96.0)

    def px(v):
        return int(round(v * scale))

    icon_px = px(168)
    left_w = px(560)          # the steps' column, fixed: nothing it says moves the window
    card_pad = px(18)
    text_w = left_w - 2 * card_pad - px(4)

    bg, card, ink, muted, line, err = '#f5f5f3', '#ffffff', '#1d1d1f', '#6e6e73', '#d6d6d2', '#b3261e'
    family = 'Segoe UI' if sys.platform == 'win32' else tkfont.nametofont('TkDefaultFont').actual('family')
    # Segoe UI has no check mark; Tk on Windows does not fall back for it.
    symbols = 'Segoe UI Symbol' if sys.platform == 'win32' else family
    for named in ('TkDefaultFont', 'TkTextFont', 'TkMenuFont', 'TkHeadingFont'):
        tkfont.nametofont(named).configure(family=family, size=10)
    root.configure(bg=bg)

    style = ttk.Style(root)
    style.theme_use('clam')
    style.configure('.', background=bg, foreground=ink, font=(family, 10))
    style.configure('TFrame', background=bg)
    style.configure('TLabel', background=bg, foreground=ink)
    style.configure('Title.TLabel', font=(family, 17, 'bold'))
    style.configure('Muted.TLabel', foreground=muted)
    style.configure('Error.TLabel', foreground=err)
    style.configure('Step.TLabel', font=(family, 12, 'bold'))
    style.configure('StepNum.TLabel', foreground=muted, font=(family, 12, 'bold'))
    style.configure('Field.TLabel', font=(family, 10, 'bold'))
    style.configure('Link.TLabel', foreground=ink, font=(family, 9, 'underline'))
    for name, size, weight, colour, face in (
            ('Card.TLabel', 10, 'normal', ink, family),
            ('CardTitle.TLabel', 12, 'bold', ink, family),
            ('CardName.TLabel', 12, 'bold', ink, family),
            ('CardMuted.TLabel', 9, 'normal', muted, family),
            ('CardOff.TLabel', 10, 'normal', muted, family),
            ('CardBad.TLabel', 10, 'normal', err, family),
            ('CardLink.TLabel', 9, 'underline', ink, family),
            ('CardMark.TLabel', 10, 'normal', ink, symbols),
            ('CardMarkOff.TLabel', 10, 'normal', muted, symbols),
            ('CardMarkBad.TLabel', 10, 'normal', err, symbols)):
        style.configure(name, background=card, foreground=colour, font=(face, size, weight))
    style.configure('TEntry', fieldbackground=card, bordercolor=line, lightcolor=card,
                    darkcolor=card, insertcolor=ink, padding=(8, 6))
    style.map('TEntry', bordercolor=[('focus', '#8e8e93')], lightcolor=[('focus', card)])
    style.configure('TButton', background=card, foreground=ink, bordercolor=line,
                    lightcolor=card, darkcolor=card, focuscolor=card, padding=(14, 6))
    style.map('TButton', background=[('pressed', '#e4e4e1'), ('active', '#ededea')],
              lightcolor=[('active', '#ededea')], darkcolor=[('active', '#ededea')])
    style.configure('Primary.TButton', background=ink, foreground='#ffffff', bordercolor=ink,
                    lightcolor=ink, darkcolor=ink, focuscolor=ink, padding=(20, 9),
                    font=(family, 10, 'bold'))
    style.map('Primary.TButton',
              background=[('disabled', '#b0b0b4'), ('pressed', '#000000'), ('active', '#3a3a3c')],
              lightcolor=[('disabled', '#b0b0b4'), ('active', '#3a3a3c')],
              darkcolor=[('disabled', '#b0b0b4'), ('active', '#3a3a3c')],
              bordercolor=[('disabled', '#b0b0b4')],
              foreground=[('disabled', '#f5f5f3')])
    row_font = tkfont.Font(root, family=family, size=10)
    name_font = tkfont.Font(root, family=family, size=12, weight='bold')

    def fit(text, font, width):
        """`text` cut to `width` pixels in `font`, its end kept: a file is
        read by its name, a path by its last folders."""
        if font.measure(text) <= width:
            return text
        while text and font.measure('...' + text) > width:
            text = text[1:]
        return '...' + text

    here = os.path.dirname(os.path.abspath(__file__))
    nro_cache = {}

    def nro_facts(path):
        """(icon, version) of an .nro, read once per file and change; the
        version is None when it cannot be read."""
        try:
            key = (path, os.path.getmtime(path))
        except OSError:
            return None, None
        if key not in nro_cache:
            try:
                icon, nacp = nro_assets(path)
                nro_cache[key] = (icon or None, nacp_version(nacp))
            except (OSError, StandaloneError):
                nro_cache[key] = (None, None)
        return nro_cache[key]

    # Next to the script (where "Download the latest" puts it), where the
    # command runs, and the build of a FlashNX checkout: the newest version,
    # then the newest file.
    found = [os.path.normpath(c) for c in dict.fromkeys((
        os.path.join(here, 'FlashNX.nro'), os.path.join(os.getcwd(), 'FlashNX.nro'),
        os.path.join(here, os.pardir, 'cpp', 'FlashNX.nro'))) if os.path.isfile(c)]
    default_nro = max(found, key=lambda p: (nro_facts(p)[1] or (0,), os.path.getmtime(p)),
                      default='')

    # What the app is made of. `choice` holds what was picked by hand for the
    # two things otherwise found beside the game: None takes what is there,
    # False takes nothing, anything else is what was picked.
    fields = {key: tk.StringVar(value=value) for key, value in (
        ('nro', default_nro), ('swf', ''), ('icon', ''),
        ('name', ''), ('author', 'FlashNX'), ('version', '1.0.0'))}
    choice = {'files': None, 'address': None}
    # `web`: a picture given by its address -> its downloaded copy, None while
    # it downloads, or the reason it failed.
    state = {'auto_name': '', 'auto_icon': '', 'icon_key': None, 'photo': None, 'web': {},
             'game_title': '', 'report_key': None, 'report_done': None, 'swf_seen': None}

    def link(parent, text, command, on_card=False):
        label = ttk.Label(parent, text=text, style='CardLink.TLabel' if on_card else 'Link.TLabel',
                          cursor='hand2')
        label.bind('<Button-1>', lambda _e: command())
        return label

    def popup(widget, items):
        """A small menu under `widget`: (label, command), None for a rule."""
        menu = tk.Menu(root, tearoff=0)
        for item in items:
            if item is None:
                menu.add_separator()
            else:
                menu.add_command(label=item[0], command=item[1])
        menu.tk_popup(widget.winfo_rootx(), widget.winfo_rooty() + widget.winfo_height())

    def flashnx_logo(height):
        """The wordmark at `height` pixels: resampled with Pillow, or halved by
        Tk itself (it reads PNG since 8.6) without it."""
        import base64
        try:
            from PIL import Image, ImageTk
            im = Image.open(io.BytesIO(base64.b64decode(LOGO_PNG)))
            width = max(1, int(round(im.width * height / float(im.height))))
            return ImageTk.PhotoImage(im.resize((width, height), Image.LANCZOS))
        except ImportError:
            img = tk.PhotoImage(data=LOGO_PNG)
            return img.subsample(max(1, int(round(img.height() / float(height)))))
        except Exception:
            return None

    outer = ttk.Frame(root, padding=(px(28), px(22), px(28), px(20)))
    outer.grid(sticky='nsew')
    root.columnconfigure(0, weight=1)
    ttk.Label(outer, text='Make a Switch app from a Flash game', style='Title.TLabel').grid(
        row=0, column=0, columnspan=2, sticky='w')
    ttk.Label(outer, style='Muted.TLabel', wraplength=left_w + icon_px + px(60), justify='left',
              text='One .nro with FlashNX and the game inside. It shows in the Homebrew Menu '
                   'and starts straight into the game, without FlashNX installed.').grid(
        row=1, column=0, columnspan=2, sticky='w', pady=(px(4), px(20)))

    left = ttk.Frame(outer)
    left.grid(row=2, column=0, sticky='nw')
    left.columnconfigure(0, minsize=left_w)

    def step(row, number, title, top=0):
        head = ttk.Frame(left)
        head.grid(row=row, column=0, sticky='w', pady=(top, px(8)))
        ttk.Label(head, text=number, style='StepNum.TLabel').pack(side='left', padx=(0, px(10)))
        ttk.Label(head, text=title, style='Step.TLabel').pack(side='left')

    # ── 1. The game ────────────────────────────────────────────────────────
    step(0, '1', 'The game')
    game_card = tk.Frame(left, bg=card, highlightthickness=max(1, px(1)), highlightbackground=line,
                         highlightcolor=line, padx=card_pad, pady=px(14))
    game_card.grid(row=1, column=0, sticky='ew')
    game_card.columnconfigure(0, weight=1)

    def choose_game():
        start = os.path.dirname(fields['swf'].get()) if fields['swf'].get() else None
        path = filedialog.askopenfilename(title='The game', initialdir=start,
                                          filetypes=[('Flash game', '*.swf')])
        if path:
            fields['swf'].set(os.path.normpath(path))

    # Before a game: where it goes, the button that does the same, and what
    # to bring from the card with it, inside the place it is dropped on.
    empty_view = tk.Frame(game_card, bg=card)
    ttk.Label(empty_view, text='Drop the game here', style='CardTitle.TLabel').grid(
        row=0, column=0, sticky='w')
    ttk.Button(empty_view, text='Choose the game...', command=choose_game).grid(
        row=1, column=0, sticky='w', pady=(px(10), px(10)))
    ttk.Label(empty_view, style='CardMuted.TLabel', wraplength=text_w, justify='left',
              text='Its .swf, with everything beside it whose name starts the same: its .files '
                   'folder, its .swf.base. Games downloaded in FlashNX are in sdmc:/flashnx/ on '
                   'the SD card, or in the games folder chosen in FlashNX.').grid(
        row=2, column=0, sticky='w')

    # With a game: its name, then a line for each thing that goes in with it,
    # each with its link, as found or as picked.
    game_view = tk.Frame(game_card, bg=card)
    game_view.columnconfigure(0, weight=1)
    game_name = ttk.Label(game_view, style='CardName.TLabel')
    game_name.grid(row=0, column=0, sticky='w')
    link(game_view, 'Change...', choose_game, on_card=True).grid(row=0, column=1, sticky='e')
    rows_box = tk.Frame(game_view, bg=card)
    rows_box.grid(row=1, column=0, columnspan=2, sticky='ew', pady=(px(10), 0))
    rows_box.columnconfigure(1, weight=1)
    rows = []
    # Made once and filled in (`show_row`): on Windows each widget is a window
    # of its own, registered for drops when the window opens.
    for i in range(3):
        mark = ttk.Label(rows_box, style='CardMark.TLabel', width=2)
        # Up to its link, which takes about 80 px: wrapped at less, a red line
        # broke in the middle of the card with nothing beside it.
        text = ttk.Label(rows_box, style='Card.TLabel', wraplength=text_w - px(110), justify='left')
        act = ttk.Label(rows_box, style='CardLink.TLabel', cursor='hand2')
        mark.grid(row=i, column=0, sticky='nw')
        text.grid(row=i, column=1, sticky='nw', pady=(0, px(4)))
        act.grid(row=i, column=2, sticky='ne', padx=(px(12), 0))
        rows.append((mark, text, act))

    def show_row(i, kind, words, action=None):
        """Line `i`: 'ok' (a check mark), 'off' (nothing there, quiet, a
        dash), 'info' (quiet, no mark), 'bad' (it will go wrong, in red) or
        None (hidden); `action` is (link, what it does)."""
        mark, text, act = rows[i]
        if kind is None:
            for w in rows[i]:
                w.configure(text='')
            return
        mark.configure(text={'ok': '\u2713', 'off': '\u2013', 'info': '', 'bad': '\u2717'}[kind],
                       style={'ok': 'CardMark', 'off': 'CardMarkOff', 'info': 'CardMarkOff',
                              'bad': 'CardMarkBad'}[kind] + '.TLabel')
        text.configure(text=words, style={'ok': 'Card', 'off': 'CardOff', 'info': 'CardOff',
                                          'bad': 'CardBad'}[kind] + '.TLabel')
        if action:
            act.configure(text=action[0])
            act.bind('<Button-1>', lambda _e, w=act, f=action[1]: f(w))
        else:
            act.configure(text='')
            act.unbind('<Button-1>')

    def set_choice(key, value):
        choice[key] = value
        state['report_key'] = None
        refresh()

    def pick_folder(_widget=None):
        path = filedialog.askdirectory(title='The game\'s companion files')
        if path:
            set_choice('files', os.path.normpath(path))

    def files_menu(widget):
        items = [('Another folder...', pick_folder), ('None', lambda: set_choice('files', False))]
        if choice['files'] is not None:
            items.append(('The one beside the game', lambda: set_choice('files', None)))
        popup(widget, items)

    def pick_address_file():
        swf = fields['swf'].get().strip()
        path = filedialog.askopenfilename(
            title='The file holding the game\'s Flashpoint address (its .swf.base)',
            initialdir=os.path.dirname(swf) or None,
            filetypes=[('Flashpoint address', '*.base'), ('All files', '*.*')])
        if path:
            set_choice('address', os.path.normpath(path))

    def type_address():
        url = simpledialog.askstring(
            'Flashpoint address', 'The address the game ran at (http://... or https://...):',
            parent=root)
        if url and url.strip():
            set_choice('address', url.strip())

    def address_menu(widget):
        items = [('From a file...', pick_address_file), ('Type it...', type_address)]
        if choice['address'] is not False:
            items.append(('None', lambda: set_choice('address', False)))
        if choice['address'] is not None:
            items.append(('The one beside the game', lambda: set_choice('address', None)))
        popup(widget, items)

    # ── 2. In the Homebrew Menu ────────────────────────────────────────────
    step(2, '2', 'In the Homebrew Menu', top=px(22))
    names = ttk.Frame(left)
    names.grid(row=3, column=0, sticky='ew')
    names.columnconfigure(0, weight=3)
    names.columnconfigure(1, weight=1)

    def undoable(var, entry):
        """Ctrl+Z / Ctrl+Y in a field, which a Tk entry does not have. A run
        of typing is one step; a value chosen or dropped is one too."""
        done, undone = [var.get()], []
        flags = {'busy': False, 'typed_at': 0.0}

        def changed(*_):
            if flags['busy']:
                return
            value, now = var.get(), time.monotonic()
            if value == done[-1]:
                return
            typing = abs(len(value) - len(done[-1])) == 1
            if typing and len(done) > 1 and now - flags['typed_at'] < 1.0:
                done[-1] = value
            else:
                done.append(value)
            flags['typed_at'] = now if typing else 0.0
            undone.clear()

        def put(value):
            flags['busy'] = True
            var.set(value)
            flags['busy'] = False
            entry.icursor('end')

        def undo(_event=None):
            if len(done) > 1:
                undone.append(done.pop())
                put(done[-1])
            return 'break'

        def redo(_event=None):
            if undone:
                done.append(undone.pop())
                put(done[-1])
            return 'break'

        var.trace_add('write', changed)
        for seq in ('<Control-z>', '<Control-Z>'):
            entry.bind(seq, lambda e: redo() if e.state & 0x1 else undo())
        for seq in ('<Control-y>', '<Control-Y>'):
            entry.bind(seq, redo)

    def text_field(label, key, limit, row, col, span=1):
        pad = (0 if col == 0 else px(12), 0)
        ttk.Label(names, text=label, style='Field.TLabel').grid(
            row=row, column=col, columnspan=span, sticky='w', padx=pad, pady=(0, px(4)))
        entry = ttk.Entry(names, textvariable=fields[key], width=10)
        # What the NACP has room for, in UTF-8 bytes (511, 255, 15, as
        # Sphaira's forwarder editor limits them): typing stops there rather
        # than the name being cut when the app is written.
        check = root.register(lambda text: len(text.encode('utf-8')) <= limit)
        entry.configure(validate='key', validatecommand=(check, '%P'))
        entry.grid(row=row + 1, column=col, columnspan=span, sticky='ew', padx=pad,
                   pady=(0, px(12)))
        undoable(fields[key], entry)

    text_field('Name', 'name', NACP_NAME_CAP - 1, 0, 0, span=2)
    text_field('Author', 'author', NACP_AUTHOR_CAP - 1, 2, 0)
    text_field('Version', 'version', NACP_VERSION_CAP - 1, 2, 1)

    # ── The app as the Homebrew Menu will list it ──────────────────────────
    preview = tk.Frame(outer, bg=card, highlightthickness=max(1, px(1)), highlightbackground=line,
                       padx=px(18), pady=px(16))
    preview.grid(row=2, column=1, sticky='n', padx=(px(28), 0))
    logo = flashnx_logo(px(22))
    if logo is not None:
        tk.Label(preview, image=logo, bg=card, bd=0).pack(anchor='center', pady=(0, px(14)))
        preview.logo = logo  # Tk keeps no reference of its own
    ttk.Label(preview, text='In the Homebrew Menu', style='CardMuted.TLabel').pack(anchor='w')
    # A label sized by an image counts pixels; with text alone it counts
    # characters. A blank square image keeps it a square either way.
    blank = tk.PhotoImage(width=icon_px, height=icon_px)
    icon_box = tk.Label(preview, bg='#e9e9e6', image=blank, compound='center', bd=0,
                        fg=muted, font=(family, 9), cursor='hand2')
    icon_box.pack(pady=(px(10), px(12)))
    shown_name = ttk.Label(preview, style='CardName.TLabel', wraplength=icon_px + px(24),
                           justify='left')
    shown_name.pack(anchor='w')
    shown_author = ttk.Label(preview, style='CardMuted.TLabel', wraplength=icon_px + px(24),
                             justify='left')
    shown_author.pack(anchor='w', pady=(px(2), 0))
    shown_version = ttk.Label(preview, style='CardMuted.TLabel')
    shown_version.pack(anchor='w')

    def pick_picture():
        start = os.path.dirname(fields['swf'].get()) if fields['swf'].get() else None
        path = filedialog.askopenfilename(
            title='Picture for the icon (square, 256x256 or more, is best)', initialdir=start,
            filetypes=[('Pictures', '*.jpg *.jpeg *.png *.webp *.bmp *.gif'), ('All files', '*.*')])
        if path:
            fields['icon'].set(os.path.normpath(path))

    def web_picture():
        src = simpledialog.askstring(
            'Picture from the web',
            'The picture\'s address (a Google Images result works too):', parent=root)
        if src and src.strip():
            fields['icon'].set(picture_source(src))

    def picture_menu(widget):
        items = [('A file...', pick_picture), ('A web address...', web_picture)]
        if fields['icon'].get().strip():
            items.append(('FlashNX\'s icon', lambda: fields['icon'].set('')))
        popup(widget, items)

    picture_link = link(preview, 'Change the picture...', lambda: picture_menu(picture_link),
                        on_card=True)
    picture_link.pack(anchor='w', pady=(px(10), 0))
    try:
        import PIL  # noqa: F401 (only whether it is there)
        picture_note = 'Square, 256x256 or more, is best.'
    except ImportError:
        picture_note = 'A 256x256 JPEG (py -m pip install pillow for any picture).'
    ttk.Label(preview, text=picture_note, style='CardMuted.TLabel',
              wraplength=icon_px + px(24), justify='left').pack(anchor='w')
    icon_box.bind('<Button-1>', lambda _e: picture_menu(icon_box))

    # ── Bottom: the FlashNX inside, what is going on, the two actions ──────
    tk.Frame(outer, bg=line, height=max(1, px(1))).grid(
        row=3, column=0, columnspan=2, sticky='ew', pady=(px(10), 0))
    foot = ttk.Frame(outer)
    foot.grid(row=4, column=0, columnspan=2, sticky='ew', pady=(px(16), 0))
    foot.columnconfigure(0, weight=1)
    nro_line = ttk.Frame(foot)
    nro_line.grid(row=0, column=0, sticky='w')
    nro_text = ttk.Label(nro_line, style='Muted.TLabel')
    nro_text.pack(side='left')

    def choose_nro():
        start = os.path.dirname(fields['nro'].get()) if fields['nro'].get() else None
        path = filedialog.askopenfilename(title='FlashNX.nro, as released', initialdir=start,
                                          filetypes=[('Switch homebrew', '*.nro')])
        if path:
            fields['nro'].set(os.path.normpath(path))

    download_link = link(nro_line, 'Download the latest', lambda: download())
    download_link.pack(side='left', padx=(px(12), 0))
    link(nro_line, 'Another file...', choose_nro).pack(side='left', padx=(px(12), 0))
    # Its wrapping width is what the buttons leave (`fit_status`), never more:
    # a long line here used to widen the window, moving everything above it.
    status = ttk.Label(foot, style='Muted.TLabel', wraplength=px(300), justify='left')
    status.grid(row=1, column=0, sticky='w', pady=(px(6), 0), padx=(0, px(16)))
    open_link = ttk.Label(foot, text='Show in folder', style='Link.TLabel', cursor='hand2')
    update_btn = ttk.Button(foot, text='Update an app...')
    update_btn.grid(row=0, column=1, rowspan=3, sticky='e', padx=(0, px(10)))
    make_btn = ttk.Button(foot, text='Make the .nro', style='Primary.TButton')
    make_btn.grid(row=0, column=2, rowspan=3, sticky='e')

    def fit_status(_event=None):
        buttons = sum(foot.grid_bbox(column=c, row=0)[2] for c in (1, 2))
        status.configure(wraplength=max(px(200), foot.winfo_width() - buttons - px(24)))

    foot.bind('<Configure>', fit_status)

    def say(text, error=False):
        status.configure(text=text, style='Error.TLabel' if error else 'Muted.TLabel')

    def show_folder(path):
        """The link to the folder `path` is in (Windows' Explorer)."""
        if sys.platform != 'win32':
            return
        folder = os.path.dirname(os.path.abspath(path))
        open_link.bind('<Button-1>', lambda _e: os.startfile(folder))
        open_link.grid(row=2, column=0, sticky='w', pady=(px(4), 0))

    def show_nro_line(nro):
        """The FlashNX the app is made with, or what is wrong with it."""
        version = nro_facts(nro)[1] if nro and os.path.isfile(nro) else None
        if not nro:
            text, style_name = 'No FlashNX.nro yet:', 'Error.TLabel'
        elif not os.path.isfile(nro):
            text, style_name = 'No FlashNX.nro at %s:' % fit(nro, row_font, px(260)), 'Error.TLabel'
        elif version is None:
            text, style_name = '%s is not a FlashNX .nro:' % os.path.basename(nro), 'Error.TLabel'
        elif version < MIN_FLASHNX:
            text, style_name = ('FlashNX %s is too old (%s or later):'
                                % (version_text(version), version_text(MIN_FLASHNX)), 'Error.TLabel')
        else:
            text, style_name = 'Made with FlashNX %s' % version_text(version), 'Muted.TLabel'
        nro_text.configure(text=text, style=style_name)

    # ── What goes into the app, worked out off the window's thread ─────────
    # A game's companions can be thousands of files, read off a card in a
    # reader.
    reports = queue.Queue()

    def size_text(n):
        if n < 1e6:
            return '%d KB' % max(1, round(n / 1e3))
        return '%.1f MB' % (n / 1e6) if n < 1e9 else '%.2f GB' % (n / 1e9)

    def show_contents(report):
        """The lines under the game: its companion files, its Flashpoint
        address, the app's size. Each says where it comes from when it was
        picked, and has its link."""
        if isinstance(report, StandaloneError):
            show_row(0, 'bad', str(report))
            show_row(1, None, '')
            show_row(2, None, '')
            return
        n = report['files']
        if n:
            words = '%d companion file%s, %s' % (n, '' if n == 1 else 's', size_text(report['bytes']))
            if choice['files']:
                words += ', from %s' % os.path.basename(report['folder'].rstrip('\\/'))
            show_row(0, 'ok', words, ('Change...', files_menu))
        elif choice['files'] is False:
            show_row(0, 'off', 'No companion files: left out', ('Change...', files_menu))
        else:
            show_row(0, 'off', 'No companion files', ('Add a folder...', pick_folder))
        if report['address']:
            shown = report['address'].split('://', 1)[-1]
            lead = 'Its Flashpoint address, '
            room = text_w - px(130) - row_font.measure(lead)
            show_row(1, 'ok', lead + fit(shown, row_font, room), ('Change...', address_menu))
        elif report['sites']:
            show_row(1, 'bad', 'Its Flashpoint address, %s.base, is missing: the game will not find '
                               'its files, filed by website (%s).'
                     % (report['game'], ', '.join(report['sites'][:2])
                        + (', ...' if len(report['sites']) > 2 else '')),
                     ('Choose...', address_menu))
        else:
            show_row(1, 'off', 'No Flashpoint address (only Flashpoint games have one)',
                     ('Add...', address_menu))
        if report['size'] is None:
            show_row(2, None, '')
        elif fat32_note(report['size']):
            show_row(2, 'bad', 'About %s: too big for a card formatted FAT32, the usual one for '
                               'homebrew. It needs one formatted exFAT.' % size_text(report['size']))
        else:
            show_row(2, 'info', 'About %s in all' % size_text(report['size']))

    def report_job(key):
        swf, files, address, nro = key
        try:
            result = game_report(swf, files, nro or None, address)
        except StandaloneError as e:
            result = e
        except Exception as e:  # an unreadable folder, a file gone: said, not swallowed
            result = StandaloneError('Could not read the game\'s files: %s' % e)
        reports.put((key, result))

    def ask_report():
        swf, nro = fields['swf'].get().strip(), fields['nro'].get().strip()
        key = (swf, choice['files'], choice['address'], nro if nro and nro_facts(nro)[1] else '')
        if key == state['report_key']:
            return
        state['report_key'] = key
        if not swf:
            return
        if not os.path.isfile(swf):
            # On one line: the whole path ran to four and pushed step 2 down.
            lead = 'Not there any more: '
            show_contents(StandaloneError(lead + fit(
                os.path.dirname(swf), row_font, text_w - px(110) - row_font.measure(lead))))
            return
        if not swf.lower().endswith('.swf'):
            show_contents(StandaloneError('%s is not a .swf game.' % os.path.basename(swf)))
            return
        threading.Thread(target=report_job, args=(key,), daemon=True).start()

        def slow():
            # Only a count that takes a while says so; a quick one would blink.
            if state['report_key'] == key and state['report_done'] != key:
                show_row(0, 'off', 'Counting the game\'s files...')

        root.after(300, slow)

    # ── Keeping it all in step ─────────────────────────────────────────────
    pictures = queue.Queue()

    def fetch_web(src):
        try:
            pictures.put((src, fetch_picture(src)[0]))
        except StandaloneError as e:
            pictures.put((src, e))

    def poll():
        """Pictures from the web, and what goes into the app, as they come."""
        try:
            while True:
                src, result = pictures.get_nowait()
                state['web'][src] = result
                if fields['icon'].get().strip() == src:
                    state['icon_key'] = None
                    refresh()
                    if isinstance(result, StandaloneError):
                        say(str(result), error=True)
        except queue.Empty:
            pass
        try:
            while True:
                key, result = reports.get_nowait()
                if key == state['report_key']:
                    state['report_done'] = key
                    show_contents(result)
        except queue.Empty:
            pass
        root.after(150, poll)

    def on_game():
        """A new game: what is beside it is taken again, its title becomes the
        name (until typed over) and its cover the picture (never over one
        picked)."""
        swf = fields['swf'].get().strip()
        if swf == state['swf_seen']:
            return
        state['swf_seen'] = swf
        choice['files'] = choice['address'] = None
        is_swf = swf.lower().endswith('.swf')
        state['game_title'] = (meta_title(swf) or os.path.basename(swf)[:-4]) if is_swf else ''
        if is_swf and fields['icon'].get().strip() in ('', state['auto_icon']):
            # The cover FlashNX itself would show for the game: named like it,
            # beside it (covers.rs, `resolve`), else the one it downloaded,
            # cached in covers/ beside the games or copied out of there.
            folder, base = os.path.dirname(swf), os.path.basename(swf)
            names_ = [n + ext for n in (base[:-4], base) for ext in ('.png', '.jpg', '.jpeg', '.webp')]
            names_ += [base + '.cover.png', os.path.join('covers', base + '.cover.png')]
            cover = next((c for c in (os.path.join(folder, n) for n in names_)
                          if os.path.isfile(c)), '')
            state['auto_icon'] = cover
            fields['icon'].set(cover)

    def refresh(*_):
        on_game()
        swf = fields['swf'].get().strip()
        if swf:
            empty_view.grid_remove()
            game_view.grid(row=0, column=0, sticky='nsew')
            game_name.configure(text=fit(os.path.basename(swf), name_font, text_w - px(90)))
        else:
            game_view.grid_remove()
            empty_view.grid(row=0, column=0, sticky='nsew')
        # The name follows the game until it is typed over: its real title when
        # FlashNX kept one beside it (`meta_title`), else its file name.
        title = state['game_title']
        if title and fields['name'].get() in ('', state['auto_name']):
            state['auto_name'] = title
            fields['name'].set(title)
        shown_name.configure(text=fields['name'].get().strip() or title or 'My game')
        shown_author.configure(text=fields['author'].get().strip() or 'FlashNX')
        shown_version.configure(text='Version ' + (fields['version'].get().strip() or '1.0.0'))
        nro = fields['nro'].get().strip()
        show_nro_line(nro)
        ask_report()
        show_icon(fields['icon'].get().strip(), nro)

    def show_icon(icon, nro):
        key = (icon, nro)
        if key == state['icon_key']:
            return
        state['icon_key'] = key
        try:
            from PIL import Image, ImageTk
        except ImportError:
            icon_box.configure(image=blank, text='Install Pillow\nfor a preview')
            return
        if icon and is_web_picture(icon):
            got = state['web'].get(icon, 'unasked')
            if got == 'unasked':
                state['web'][icon] = None
                threading.Thread(target=fetch_web, args=(icon,), daemon=True).start()
            if got in ('unasked', None):
                state['icon_key'] = None  # look again once it is here
                icon_box.configure(image=blank, text='Downloading\nthe picture...')
                return
            icon = got if isinstance(got, str) else ''
        img = None
        try:
            if icon and os.path.isfile(icon):
                img = cover_square(Image.open(icon))
            elif nro and os.path.isfile(nro):
                data = nro_facts(nro)[0]
                img = Image.open(io.BytesIO(data)).convert('RGB') if data else None
        except Exception:
            img = None
        if img is None:
            state['photo'] = None
            icon_box.configure(image=blank, text='')
            return
        state['photo'] = ImageTk.PhotoImage(img.resize((icon_px, icon_px), Image.LANCZOS))
        icon_box.configure(image=state['photo'], text='')

    for var in fields.values():
        var.trace_add('write', refresh)

    # ── Making the app, updating one ───────────────────────────────────────
    results = queue.Queue()
    # The notes of the job under way ("a card formatted FAT32 cannot hold
    # it"...), kept under the line that ends it: shown one by one, each was
    # replaced by the next, and the last by "is ready", so none stayed read.
    notes = []

    def note(text):
        text = text[len('note: '):] if text.startswith('note: ') else text
        notes.append(text[:1].upper() + text[1:])
        say(notes[-1])

    def with_notes(text):
        return '\n'.join([text] + notes)

    def percent():
        """A `progress` for the job: its percentage, sent on once per change."""
        shown = [-1]

        def progress(done, total):
            pct = done * 100 // total if total else 100
            if pct != shown[0]:
                shown[0] = pct
                results.put(('progress', pct))
        return progress

    def outcome(doing):
        """The job's end as (kind, value), or None while it runs. Everything
        waiting is taken at each look: one message per look left "is ready"
        seconds behind a hundred percentages."""
        while True:
            try:
                kind, value = results.get_nowait()
            except queue.Empty:
                return None
            if kind == 'note':
                note(value)
            elif kind == 'progress':
                say(with_notes('%s %d %%' % (doing, value)))
            else:
                return kind, value

    def work(args, address):
        try:
            results.put(('ok', make_standalone(*args, log=lambda s: results.put(('note', s)),
                                               progress=percent(), address=address)))
        except StandaloneError as e:
            results.put(('error', str(e)))
        except Exception as e:  # a bug, said rather than swallowed
            results.put(('error', 'Unexpected error: %r' % (e,)))

    def poll_make(out_path, name):
        done = outcome('Writing %s...' % os.path.basename(out_path))
        if done is None:
            root.after(100, poll_make, out_path, name)
            return
        kind, value = done
        make_btn.state(['!disabled'])
        update_btn.state(['!disabled'])
        if kind == 'error':
            say(value, error=True)
            return
        size = os.path.getsize(out_path) / 1e6
        say(with_notes('%s is ready (%.1f MB). Copy it to sdmc:/switch/%s/ on the SD card.'
                       % (os.path.basename(out_path), size, safe_file_name(name))))
        show_folder(out_path)

    def nro_ready():
        nro = fields['nro'].get().strip()
        version = nro_facts(nro)[1] if nro else None
        if version is None or version < MIN_FLASHNX:
            say('Choose the FlashNX.nro to put in the app first, or Download the latest.',
                error=True)
            return None
        return nro

    def create():
        swf = fields['swf'].get().strip()
        if not swf:
            say('Choose the game first.', error=True)
            return
        nro = nro_ready()
        if not nro:
            return
        name = fields['name'].get().strip() or state['game_title'] or os.path.basename(swf)[:-4]
        out_path = filedialog.asksaveasfilename(
            title='Save the app as', defaultextension='.nro',
            initialdir=os.path.dirname(swf) or None,
            initialfile=safe_file_name(name) + '.nro', filetypes=[('Switch homebrew', '*.nro')])
        if not out_path:
            return
        open_link.grid_remove()
        make_btn.state(['disabled'])
        update_btn.state(['disabled'])
        del notes[:]
        say('Writing %s...' % os.path.basename(out_path))
        icon = fields['icon'].get().strip()
        # A picture from the web already downloaded for the preview is used as
        # is, rather than fetched again.
        if isinstance(state['web'].get(icon), str):
            icon = state['web'][icon]
        args = (nro, swf, out_path, name, fields['author'].get().strip(),
                fields['version'].get().strip(), icon or None, choice['files'])
        threading.Thread(target=work, args=(args, choice['address']), daemon=True).start()
        root.after(100, poll_make, out_path, name)

    make_btn.configure(command=create)

    def update_work(app, nro, out_path):
        try:
            results.put(('updated', update_app(app, nro, out_path,
                                               log=lambda s: results.put(('note', s)),
                                               progress=percent())))
        except StandaloneError as e:
            results.put(('error', str(e)))
        except Exception as e:  # a bug, said rather than swallowed
            results.put(('error', 'Unexpected error: %r' % (e,)))

    def poll_update(out_path):
        done = outcome('Updating %s...' % os.path.basename(out_path))
        if done is None:
            root.after(100, poll_update, out_path)
            return
        kind, value = done
        make_btn.state(['!disabled'])
        update_btn.state(['!disabled'])
        if kind == 'error':
            say(value, error=True)
            return
        name, had, now = value
        say(with_notes('%s now runs FlashNX %s%s. Copy it over the old one on the SD card: '
                       'saves and settings stay.' % (name, now, ' (it had %s)' % had if had else '')))
        show_folder(out_path)

    def update():
        """An app made earlier, written again with the FlashNX.nro chosen
        below: its game, picture, name, author and version kept."""
        nro = nro_ready()
        if not nro:
            return
        app = filedialog.askopenfilename(title='The app to update',
                                         filetypes=[('Switch homebrew', '*.nro')])
        if not app:
            return
        out_path = filedialog.asksaveasfilename(
            title='Save the updated app as', defaultextension='.nro',
            initialdir=os.path.dirname(app), initialfile=os.path.basename(app),
            filetypes=[('Switch homebrew', '*.nro')])
        if not out_path:
            return
        open_link.grid_remove()
        make_btn.state(['disabled'])
        update_btn.state(['disabled'])
        del notes[:]
        say('Updating %s...' % os.path.basename(app))
        threading.Thread(target=update_work, args=(app, nro, out_path), daemon=True).start()
        root.after(100, poll_update, out_path)

    update_btn.configure(command=update)

    downloads = queue.Queue()
    busy = {'download': False}

    def fetch():
        try:
            downloads.put(('done', download_flashnx(
                download_dir(), lambda done, total: downloads.put(('progress', (done, total))))))
        except StandaloneError as e:
            downloads.put(('error', str(e)))
        except Exception as e:  # a bug, said rather than swallowed
            downloads.put(('error', 'Unexpected error: %r' % (e,)))

    def poll_download():
        final = progress = None
        while True:
            try:
                kind, value = downloads.get_nowait()
            except queue.Empty:
                break
            if kind == 'progress':
                progress = value
            else:
                final = (kind, value)
        if final is None:
            if progress:
                done, total = progress
                say('Downloading FlashNX.nro... %d %%' % (done * 100 // total) if total
                    else 'Downloading FlashNX.nro... %.1f MB' % (done / 1e6))
            root.after(100, poll_download)
            return
        busy['download'] = False
        make_btn.state(['!disabled'])
        kind, value = final
        if kind == 'error':
            say(value, error=True)
            return
        path, version = value
        fields['nro'].set(path)
        say('FlashNX %s is downloaded: %s' % (version, path))

    def download():
        if busy['download']:
            return
        busy['download'] = True
        make_btn.state(['disabled'])
        say('Asking GitHub for the latest FlashNX...')
        threading.Thread(target=fetch, daemon=True).start()
        root.after(100, poll_download)

    # ── Drops ──────────────────────────────────────────────────────────────
    picture_ext = ('.jpg', '.jpeg', '.png', '.webp', '.bmp', '.gif')

    def take(item):
        """Put one dropped thing where it goes, by what it is."""
        item = item.strip()
        low = item.lower()
        if is_web_picture(item):
            fields['icon'].set(picture_source(item))
        elif low.endswith('.swf'):
            fields['swf'].set(os.path.normpath(item))
        elif low.endswith('.nro'):
            fields['nro'].set(os.path.normpath(item))
        elif low.endswith('.base'):
            set_choice('address', os.path.normpath(item))
        elif low.endswith(picture_ext):
            fields['icon'].set(os.path.normpath(item))
        elif os.path.isdir(item):
            set_choice('files', os.path.normpath(item))
        else:
            return False
        return True

    def accept(items):
        # The game first, so that what is dropped with it lands on it rather
        # than being taken again from beside it.
        items = sorted(items, key=lambda i: not i.strip().lower().endswith('.swf'))
        taken = [take(i) for i in items]
        if not any(taken):
            say('Drop a .swf game, its companion folder or its .swf.base, a picture (or one '
                'from a web page), or a FlashNX.nro.', error=True)

    def hover(over):
        game_card.configure(highlightbackground=ink if over else line)

    def on_drop(event):
        hover(False)
        accept(root.tk.splitlist(event.data))
        return event.action

    def walk(w):
        yield w
        for c in w.winfo_children():
            yield from walk(c)

    refresh()
    poll()
    # The card keeps its height between its two views, so step 2 stays put.
    root.update_idletasks()
    game_card.rowconfigure(0, minsize=max(empty_view.winfo_reqheight(), game_view.winfo_reqheight()))

    if can_drop:
        for w in walk(root):
            w.drop_target_register(DND_FILES, DND_TEXT)
            w.dnd_bind('<<Drop:DND_Files>>', on_drop)
            w.dnd_bind('<<Drop:DND_Text>>', on_drop)
            w.dnd_bind('<<DropEnter>>', lambda e: (hover(True), e.action)[1])
            w.dnd_bind('<<DropLeave>>', lambda e: (hover(False), e.action)[1])
    else:
        # Every Tk widget is a window of its own on Windows: all of them, and
        # the frame around the window, take drops.
        hwnds = [int(root.wm_frame(), 16)] + [w.winfo_id() for w in walk(root)]
        root.dnd = windows_drop(hwnds, lambda items: root.after(0, accept, items),
                                lambda over: root.after(0, hover, over))
        can_drop = root.dnd is not None
        if can_drop:
            def close():
                root.dnd[-1]()
                root.destroy()

            root.protocol('WM_DELETE_WINDOW', close)
    say('Your game stays on this computer.' if can_drop else
        'Your game stays on this computer. Dropping files here needs the tkinterdnd2 package.')
    root.update_idletasks()
    w, h = root.winfo_reqwidth(), root.winfo_reqheight()
    root.geometry('+%d+%d' % ((root.winfo_screenwidth() - w) // 2,
                              max(0, (root.winfo_screenheight() - h) // 3)))
    root.mainloop()
    return 0


if __name__ == '__main__':
    if len(sys.argv) > 1:
        sys.exit(main_cli(sys.argv[1:]))
    try:
        sys.exit(main_gui())
    except ImportError:
        sys.exit(main_cli(['--help']))
