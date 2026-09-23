#!/usr/bin/env python3
"""Turn a FlashNX sampling-profiler capture into profiles.

    python scripts/prof_report.py prof.bin cpp/FlashNX.elf [options]

prof.bin comes from `sdmc:/switch/FlashNX/prof.bin` (cpp/src/prof.cpp). The ELF
must be the one of the build that ran: addresses are looked up in it. Every
sample is one pause of the Ruffle worker: its PC, its LR, then the return
addresses of its frame-pointer chain.

Prints, for one game of the session (the last one by default):
  - self: the function the worker was in (flat profile);
  - self by source line, when the ELF has line tables (the --prof build);
  - total: every function on the stack, counted once per sample (inclusive);
  - with --focus REGEX: who calls the matching functions and what they call.

With line tables, every address is expanded into the functions inlined at that
point (addr2line -i), innermost first, so an action handler inlined into
`do_action` is still named. --no-inline uses the symbol table only.

Share = samples / all samples of the selection. At ~1 kHz a 35 ms frame holds
~35 samples, so --min-frame-samples 25 keeps only the heavy frames. The
profiling build alternates 240-frame A/B windows and stamps each sample with
its regime: --regime 1 / --regime 0 profile each side.
"""

import argparse
import bisect
import collections
import os
import re
import shutil
import struct
import subprocess
import sys


def find_tool(tool):
    for c in (
        "aarch64-none-elf-" + tool,
        "/c/devkitPro/devkitA64/bin/aarch64-none-elf-" + tool,
        r"C:\devkitPro\devkitA64\bin\aarch64-none-elf-" + tool + ".exe",
    ):
        if shutil.which(c) or os.path.exists(c):
            return c
    sys.exit("aarch64-none-elf-%s not found (devkitA64)" % tool)


class Symbols:
    def __init__(self, elf):
        out = subprocess.run(
            [find_tool("nm"), "-n", "-C", "-S", "--defined-only", elf],
            capture_output=True, text=True, errors="replace", check=True,
        ).stdout
        rows = []
        for line in out.splitlines():
            parts = line.split(" ", 3)
            if len(parts) == 4 and len(parts[2]) == 1:
                addr, size, kind, name = parts
            else:
                parts = line.split(" ", 2)
                if len(parts) != 3 or len(parts[1]) != 1:
                    continue
                addr, kind, name = parts
                size = None
            if kind not in "TtWw":
                continue
            rows.append((int(addr, 16), int(size, 16) if size else None, name))
        rows.sort(key=lambda r: r[0])
        self.starts = []
        self.ends = []
        self.names = []
        for i, (a, sz, name) in enumerate(rows):
            if self.starts and self.starts[-1] == a:
                # Aliases (identical code folded): keep the shorter name.
                if len(name) < len(self.names[-1]):
                    self.names[-1] = name
                continue
            nxt = rows[i + 1][0] if i + 1 < len(rows) else a + (sz or 4)
            self.starts.append(a)
            self.ends.append(a + sz if sz else nxt)
            self.names.append(name)
        self.by_name = {}
        for a, n in zip(self.starts, self.names):
            self.by_name.setdefault(n, a)

    def lookup(self, addr):
        i = bisect.bisect_right(self.starts, addr) - 1
        if i < 0 or addr >= self.ends[i]:
            return None
        return self.names[i]


GENERIC = re.compile(r"::<[^<>]*>")
PATH = re.compile(r"\b(?:[A-Za-z_][A-Za-z0-9_]*::)+([A-Za-z_][A-Za-z0-9_]*::[A-Za-z_][A-Za-z0-9_]*)")


def short(name):
    s = name
    for _ in range(8):
        t = GENERIC.sub("", s)
        if t == s:
            break
        s = t
    s = PATH.sub(r"\1", s)
    return s if len(s) <= 110 else s[:107] + "..."


def short_path(loc):
    """'/long/path/core/src/avm1/activation.rs:123 (discriminator 2)' -> 'avm1/activation.rs:123'."""
    loc = loc.split(" (")[0].replace("\\", "/")
    parts = loc.split("/")
    return "/".join(parts[-2:]) if len(parts) > 1 else loc


def inline_chains(elf, addrs):
    """{addr: [(function, location), ...]} innermost first, via addr2line -i.

    None when the ELF has no line tables (no location is ever known)."""
    addrs = sorted(addrs)
    proc = subprocess.run(
        [find_tool("addr2line"), "-a", "-f", "-i", "-C", "-e", elf],
        input="\n".join("0x%x" % a for a in addrs) + "\n",
        capture_output=True, text=True, errors="replace",
    )
    hexdigits = set("0123456789abcdef")
    chains = {}
    cur = None
    pending = None
    known = 0
    for line in proc.stdout.splitlines():
        if line.startswith("0x") and len(line) >= 10 and set(line[2:]) <= hexdigits:
            cur = int(line, 16)
            chains[cur] = []
            pending = None
        elif cur is not None:
            if pending is None:
                pending = line
            else:
                if not line.startswith("??"):
                    known += 1
                chains[cur].append((pending, line))
                pending = None
    return chains if known else None


def read_capture(path):
    data = open(path, "rb").read()
    if data[:8] != b"FNXPROF1":
        sys.exit("not a FlashNX profile (bad magic)")
    base, self_addr, interval_us, max_frames = struct.unpack_from("<QQII", data, 8)
    pos = 32
    game = 0
    samples = []  # (game, frame, [addresses], regime)
    events = []
    while pos + 8 <= len(data):
        kind = data[pos]
        if kind == 1:
            n = data[pos + 1]
            regime = struct.unpack_from("<H", data, pos + 2)[0]
            frame = struct.unpack_from("<I", data, pos + 4)[0]
            end = pos + 8 + 4 * n
            if end > len(data):
                break
            addrs = list(struct.unpack_from("<%dI" % n, data, pos + 8))
            samples.append((game, frame, addrs, regime))
            pos = end
        elif kind == 2:
            ev = data[pos + 1]
            g = struct.unpack_from("<H", data, pos + 2)[0]
            frame = struct.unpack_from("<I", data, pos + 4)[0]
            if ev == 1:
                game = g
            events.append((ev, g, frame))
            pos += 8
        else:
            print("warning: unknown record %d at %d, stopping" % (kind, pos), file=sys.stderr)
            break
    return base, self_addr, interval_us, samples, events


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("capture")
    ap.add_argument("elf")
    ap.add_argument("--game", type=int, default=-1, help="game number in the session (1 = first; default last)")
    ap.add_argument("--frames", help="host frame range A-B")
    ap.add_argument("--min-frame-samples", type=int, default=0,
                    help="keep only frames with at least this many samples (heavy frames)")
    ap.add_argument("--require", help="keep only samples whose stack matches this regex")
    ap.add_argument("--regime", type=int, help="keep only samples of this A/B regime")
    ap.add_argument("--top", type=int, default=40)
    ap.add_argument("--focus", action="append", default=[], help="regex: callers and callees")
    ap.add_argument("--folded", help="write folded stacks (flame graph input) to this file")
    ap.add_argument("--no-inline", action="store_true", help="symbol table only, no addr2line")
    args = ap.parse_args()

    base, self_addr, interval_us, samples, events = read_capture(args.capture)
    syms = Symbols(args.elf)
    boot = syms.by_name.get("prof_boot")
    if boot is not None and self_addr - base != boot:
        print("WARNING: prof_boot is at 0x%x in the capture, 0x%x in this ELF: wrong ELF?"
              % (self_addr - base, boot), file=sys.stderr)

    games = sorted({g for g, _, _, _ in samples})
    if not games:
        sys.exit("no samples")
    game = games[-1] if args.game < 0 else args.game
    sel = [s for s in samples if s[0] == game]
    print("capture: %d samples in %d game(s) %s, interval %d us; game %d: %d samples"
          % (len(samples), len(games), games, interval_us, game, len(sel)))
    regimes = collections.Counter(s[3] for s in sel)
    if len(regimes) > 1:
        print("regimes: %s" % ", ".join("%d: %d samples" % kv for kv in sorted(regimes.items())))
    if args.regime is not None:
        sel = [s for s in sel if s[3] == args.regime]

    if args.frames:
        a, b = (int(x) for x in args.frames.split("-"))
        sel = [s for s in sel if a <= s[1] <= b]
    per_frame = collections.Counter(s[1] for s in sel)
    if args.min_frame_samples:
        sel = [s for s in sel if per_frame[s[1]] >= args.min_frame_samples]
    if not sel:
        sys.exit("no samples left after filtering")
    frames = collections.Counter(s[1] for s in sel)
    depth = sum(len(s[2]) for s in sel) / len(sel)
    print("selection: %d samples over %d frames (%.1f samples/frame ~ %.1f ms/frame), mean depth %.1f"
          % (len(sel), len(frames), len(sel) / len(frames),
             len(sel) / len(frames) * interval_us / 1000.0, depth))

    # Return addresses (everything after the PC) are looked up at addr - 4,
    # the call instruction, so a call that ends a function is not charged to
    # the next one. Consecutive repeats (PC and LR in the same function)
    # collapse into one frame.
    cache = {}

    def name_at(off):
        n = cache.get(off)
        if n is None:
            n = syms.lookup(off)
            n = short(n) if n else "?0x%x" % off
            cache[off] = n
        return n

    wanted = set()
    for _, _, addrs, _ in sel:
        for i, a in enumerate(addrs):
            wanted.add(a if i == 0 else a - 4)
    chains = None if args.no_inline else inline_chains(args.elf, wanted)
    if chains is None and not args.no_inline:
        print("(no line tables in this ELF: symbol table only)")

    def expand(off):
        """Function names at this address, innermost first."""
        if chains is not None:
            ch = chains.get(off)
            if ch:
                return [short(f) if f != "??" else name_at(off) for f, _ in ch]
        return [name_at(off)]

    stacks = []
    lines = []
    for _, _, addrs, _ in sel:
        names = []
        for i, a in enumerate(addrs):
            for n in expand(a if i == 0 else a - 4):
                if not names or names[-1] != n:
                    names.append(n)
        stacks.append(names)
        loc = "?"
        if chains is not None and chains.get(addrs[0]):
            loc = short_path(chains[addrs[0]][0][1])
        lines.append(loc)

    if args.require:
        rx = re.compile(args.require)
        keep = [i for i, st in enumerate(stacks) if any(rx.search(n) for n in st)]
        stacks = [stacks[i] for i in keep]
        lines = [lines[i] for i in keep]
        print("require /%s/: %d samples" % (args.require, len(stacks)))
        if not stacks:
            sys.exit("no samples match --require")

    total = len(stacks)
    self_c = collections.Counter(st[0] for st in stacks)
    incl_c = collections.Counter()
    for st in stacks:
        for n in set(st):
            incl_c[n] += 1

    def table(title, counter):
        print("\n== %s ==" % title)
        for n, c in counter.most_common(args.top):
            print("%6.2f%% %7d  %s" % (100.0 * c / total, c, n))

    table("self (where the worker was)", self_c)
    if chains is not None:
        table("self by source line",
              collections.Counter("%s  [%s]" % (lines[i], stacks[i][0]) for i in range(total)))
    table("total (on the stack, once per sample)", incl_c)

    for pat in args.focus:
        rx = re.compile(pat)
        callers = collections.Counter()
        callees = collections.Counter()
        selfc = 0
        hit = 0
        for st in stacks:
            idx = [i for i, n in enumerate(st) if rx.search(n)]
            if not idx:
                continue
            hit += 1
            if idx[0] == 0:
                selfc += 1
            seen_r, seen_e = set(), set()
            for i in idx:
                if i + 1 < len(st) and not rx.search(st[i + 1]):
                    seen_r.add(st[i + 1])
                if i > 0 and not rx.search(st[i - 1]):
                    seen_e.add(st[i - 1])
            for n in seen_r:
                callers[n] += 1
            for n in seen_e:
                callees[n] += 1
        print("\n== focus /%s/: on the stack in %.2f%%, itself running in %.2f%% =="
              % (pat, 100.0 * hit / total, 100.0 * selfc / total))
        print("-- called from:")
        for n, c in callers.most_common(15):
            print("%6.2f%% %7d  %s" % (100.0 * c / total, c, n))
        print("-- calls (time spent below it):")
        for n, c in callees.most_common(25):
            print("%6.2f%% %7d  %s" % (100.0 * c / total, c, n))

    if args.folded:
        folded = collections.Counter(";".join(reversed(st)) for st in stacks)
        with open(args.folded, "w", encoding="utf-8") as f:
            for k, c in folded.most_common():
                f.write("%s %d\n" % (k, c))
        print("\nfolded stacks: %s (%d distinct)" % (args.folded, len(folded)))


if __name__ == "__main__":
    main()
