#!/usr/bin/env python3
"""Tests of make_standalone.py.

    python scripts/test_make_standalone.py

Most run anywhere, on a small FlashNX.nro made up here. Two compare with
devkitPro's own tools, byte for byte, and are skipped without them:
build_romfs for the RomFS, and elf2nro for the whole .nro (that one also
needs a build: cpp/FlashNX.elf and cpp/FlashNX.nro).
"""

import importlib.util
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import tracemalloc
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)

_spec = importlib.util.spec_from_file_location('make_standalone', os.path.join(HERE, 'make_standalone.py'))
ms = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ms)


def devkitpro_tool(name):
    """A devkitPro tool's path, or None."""
    found = shutil.which(name)
    if found:
        return found
    exe = name + ('.exe' if sys.platform == 'win32' else '')
    for root in (os.environ.get('DEVKITPRO', ''), r'C:\devkitPro', '/opt/devkitpro'):
        path = os.path.join(root, 'tools', 'bin', exe)
        if root and os.path.isfile(path):
            return path
    return None


def fake_flashnx(path, version, program=b'program'):
    """A FlashNX.nro as far as make_standalone.py reads one: an NRO header
    giving the program part's size, then an asset section with an icon and a
    NACP carrying `version`."""
    prog = bytearray(0x80) + program
    prog[0x10:0x14] = b'NRO0'
    struct.pack_into('<I', prog, 0x18, len(prog))
    nacp = bytearray(ms.NACP_SIZE)
    nacp[0:7] = b'FlashNX'
    v = version.encode('ascii')
    nacp[ms.NACP_VERSION_OFF:ms.NACP_VERSION_OFF + len(v)] = v
    icon = b'\xff\xd8 not really a JPEG'
    aset = b'ASET' + struct.pack('<I6Q', 0, ms.ASSET_HEADER_SIZE, len(icon),
                                 ms.ASSET_HEADER_SIZE + len(icon), len(nacp), 0, 0)
    with open(path, 'wb') as f:
        f.write(bytes(prog) + aset + icon + bytes(nacp))
    return path


def write_tree(root, files):
    """`files`: {path with '/': bytes}."""
    for rel, data in files.items():
        p = os.path.join(root, *rel.split('/'))
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, 'wb') as f:
            f.write(data)


def quiet(_text):
    pass


# A game with what trips a RomFS up: names out of ASCII, spaces, an empty
# file, nested folders, and enough files and folders for the hash tables to
# take their prime-number sizes (over 19 entries).
GAME = dict(
    [('Mon Jeu.swf', b'FWS' + os.urandom(3000))]
    + [('Mon Jeu.files/www.example.com/game/level%d.swf' % i, os.urandom(17 * i)) for i in range(25)]
    + [('Mon Jeu.files/www.example.com/données/été.xml', b'<x/>'),
       ('Mon Jeu.files/a.txt', b''),
       ('Mon Jeu.files/Z/b/c/d.bin', os.urandom(4096)),
       ('Mon Jeu.files/www.example.com/game/sub/x y.png', os.urandom(5))]
    + [('Mon Jeu.files/dir%02d/f.bin' % i, os.urandom(i)) for i in range(22)])


class Work(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix='make_standalone_test_')
        self.addCleanup(shutil.rmtree, self.dir, True)

    def path(self, *parts):
        return os.path.join(self.dir, *parts)

    def game(self):
        write_tree(self.path('tree'), GAME)
        return self.path('tree', 'Mon Jeu.swf')

    def app_files(self, app):
        """{path: bytes} of what an app carries."""
        out = {}
        for rel, src in ms.app_info(app)['files']:
            out[rel] = b''.join(ms.source_chunks(src))
        return out


class RomfsAgainstSwitchTools(Work):
    def test_romfs_matches_build_romfs(self):
        tool = devkitpro_tool('build_romfs')
        if not tool:
            self.skipTest('devkitPro build_romfs not found')
        entries = ms.game_entries(self.game())
        ours = self.path('ours.romfs')
        with open(ours, 'wb') as out:
            ms.write_romfs(out, ms.RomfsPlan(entries))
        ref = self.path('ref.romfs')
        subprocess.run([tool, self.path('tree'), ref], check=True, stdout=subprocess.DEVNULL)
        with open(ours, 'rb') as a, open(ref, 'rb') as b:
            self.assertTrue(a.read() == b.read(), 'RomFS differs from build_romfs')

    def test_nro_matches_elf2nro(self):
        tool = devkitpro_tool('elf2nro')
        elf = os.path.join(REPO, 'cpp', 'FlashNX.elf')
        nro = os.path.join(REPO, 'cpp', 'FlashNX.nro')
        if not tool or not (os.path.isfile(elf) and os.path.isfile(nro)):
            self.skipTest('needs devkitPro elf2nro and a build (cpp/FlashNX.elf, .nro)')
        if os.path.getmtime(nro) < os.path.getmtime(elf):
            self.skipTest('cpp/FlashNX.nro is older than cpp/FlashNX.elf')
        swf = self.game()
        ours = self.path('ours.nro')
        ms.make_standalone(nro, swf, ours, name='Mon Jeu', author='Moi', version='1.2.3', log=quiet)
        # The same icon, NACP and files, handed to elf2nro.
        _size, icon, nacp, _romfs = ms.nro_layout(nro)
        found = ms.nacp_version(nacp)
        with open(self.path('icon.jpg'), 'wb') as f:
            f.write(icon)
        with open(self.path('app.nacp'), 'wb') as f:
            f.write(ms.make_nacp(nacp, 'Mon Jeu', 'Moi', '1.2.3'))
        rel, data = ms.marker(found)
        write_tree(self.path('tree'), {rel: data})
        ref = self.path('ref.nro')
        subprocess.run([tool, elf, ref, '--icon=' + self.path('icon.jpg'),
                        '--nacp=' + self.path('app.nacp'), '--romfsdir=' + self.path('tree')],
                       check=True, stdout=subprocess.DEVNULL)
        with open(ours, 'rb') as a, open(ref, 'rb') as b:
            self.assertTrue(a.read() == b.read(), '.nro differs from elf2nro')


class MakeAndUpdate(Work):
    def test_app_carries_the_game(self):
        flashnx = fake_flashnx(self.path('FlashNX.nro'), '2.0.0')
        app = self.path('Mon_Jeu.nro')
        ms.make_standalone(flashnx, self.game(), app, name='Mon Jeu', author='Moi', log=quiet)
        info = ms.app_info(app)
        self.assertEqual(info['name'], 'Mon Jeu')
        self.assertEqual(info['author'], 'Moi')
        self.assertEqual(info['flashnx'], '2.0.0')
        self.assertEqual(self.app_files(app), GAME)

    def test_update_in_place(self):
        app = self.path('Mon_Jeu.nro')
        ms.make_standalone(fake_flashnx(self.path('old.nro'), '2.0.0', b'old program'),
                           self.game(), app, name='Mon Jeu', log=quiet)
        newer = fake_flashnx(self.path('new.nro'), '2.1.0', b'new program, longer than the old one')
        name, had, now = ms.update_app(app, newer, app, log=quiet)
        self.assertEqual((name, had, now), ('Mon Jeu', '2.0.0', '2.1.0'))
        size, _icon, _nacp, _romfs = ms.nro_layout(app)
        self.assertTrue(ms.read_program(app, size).endswith(b'new program, longer than the old one'))
        self.assertEqual(ms.app_info(app)['flashnx'], '2.1.0')
        self.assertEqual(self.app_files(app), GAME)

    def test_flashpoint_address_goes_in(self):
        # FlashNX reads `romfs:/<game>.swf.base` for the URL to run the game
        # under; without it a host-pathed game finds none of its files.
        swf = self.game()
        with open(swf + '.base', 'wb') as f:
            f.write(b'http://www.example.com/game/Mon Jeu.swf')
        app = self.path('Mon_Jeu.nro')
        ms.make_standalone(fake_flashnx(self.path('FlashNX.nro'), '2.0.0'), swf, app, log=quiet)
        files = self.app_files(app)
        self.assertEqual(files['Mon Jeu.swf.base'], b'http://www.example.com/game/Mon Jeu.swf')

    def test_progress_goes_to_the_end(self):
        seen = []
        ms.make_standalone(fake_flashnx(self.path('FlashNX.nro'), '2.0.0'), self.game(),
                           self.path('x.nro'), log=quiet, progress=lambda d, t: seen.append((d, t)))
        self.assertTrue(seen)
        dones = [d for d, _t in seen]
        self.assertEqual(dones, sorted(dones))
        self.assertEqual(seen[-1][0], seen[-1][1])  # written and read back: all of it

    def test_update_refuses_same_or_older(self):
        app = self.path('Mon_Jeu.nro')
        ms.make_standalone(fake_flashnx(self.path('a.nro'), '2.1.0'), self.game(), app, log=quiet)
        for version in ('2.1.0', '2.0.0'):
            with self.assertRaises(ms.StandaloneError):
                ms.update_app(app, fake_flashnx(self.path('b.nro'), version), app, log=quiet)

    def test_refuses_old_flashnx(self):
        with self.assertRaises(ms.StandaloneError):
            ms.make_standalone(fake_flashnx(self.path('FlashNX.nro'), '1.8.1'), self.game(),
                               self.path('x.nro'), log=quiet)

    def test_cut_nro_refused(self):
        app = self.path('Mon_Jeu.nro')
        ms.make_standalone(fake_flashnx(self.path('FlashNX.nro'), '2.0.0'), self.game(), app, log=quiet)
        with open(app, 'r+b') as f:
            f.truncate(os.path.getsize(app) - 100)
        with self.assertRaises(ms.StandaloneError):
            ms.app_info(app)


class Memory(Work):
    def test_big_game_is_never_held_whole(self):
        # 96 MB of game: making the app, reading it back and updating it must
        # each stay around a few copy chunks, not the size of the game.
        size = 96 << 20
        write_tree(self.path('tree'), {'Big.swf': b'FWS' + os.urandom(1000)})
        big = self.path('tree', 'Big.files', 'data.bin')
        os.makedirs(os.path.dirname(big))
        with open(big, 'wb') as f:
            for _ in range(size // (8 << 20)):
                f.write(os.urandom(8 << 20))
        app = self.path('Big.nro')
        limit = 4 * ms.COPY_CHUNK
        tracemalloc.start()
        try:
            ms.make_standalone(fake_flashnx(self.path('a.nro'), '2.0.0'),
                               self.path('tree', 'Big.swf'), app, log=quiet)
            _now, peak = tracemalloc.get_traced_memory()
            self.assertLess(peak, limit, 'making the app held %d MB' % (peak >> 20))
            tracemalloc.reset_peak()
            ms.update_app(app, fake_flashnx(self.path('b.nro'), '2.1.0'), app, log=quiet)
            _now, peak = tracemalloc.get_traced_memory()
            self.assertLess(peak, limit, 'updating the app held %d MB' % (peak >> 20))
        finally:
            tracemalloc.stop()


class Small(unittest.TestCase):
    def test_fat32_note(self):
        self.assertIsNone(ms.fat32_note(ms.FAT32_MAX_FILE))
        self.assertIn('exFAT', ms.fat32_note(ms.FAT32_MAX_FILE + 1))

    def test_safe_file_name(self):
        self.assertEqual(ms.safe_file_name("Garfield's Scary Scavenger Hunt"),
                         'Garfields_Scary_Scavenger_Hunt')
        self.assertEqual(ms.safe_file_name('  3D Mario Racing! '), '3D_Mario_Racing')
        self.assertEqual(ms.safe_file_name('???'), 'game')


if __name__ == '__main__':
    unittest.main(verbosity=2)
