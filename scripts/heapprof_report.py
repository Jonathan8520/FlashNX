"""Turn a FlashNX heap profile into a ranking of who holds the live memory.

    python scripts/heapprof_report.py heapprof.bin cpp/FlashNX.elf [--top 40]
    python scripts/heapprof_report.py heapprof.bin cpp/FlashNX.elf --focus REGEX

heapprof.bin comes from `sdmc:/switch/FlashNX/heapprof.bin`, written every 1200
frames by the `--memprof` build (rust/src/heapprof.rs). The ELF must be the one
of the build that ran. Each record is a call stack with the live bytes sampled
under it; a sample stands for max(its size, the sampling interval) bytes, so
the figures are estimates, good to a few percent on anything that matters.

Prints the live total, then the memory by:
  - owner: the first frame that is not the allocator or a container
    (Vec, HashMap, Box, gc_arena's allocation...): the code that asked;
  - module of that owner (ruffle_core::avm2, swf, ruffle_render...);
  - full stacks, the biggest ones, for the details.
With --focus, only the stacks with a frame matching REGEX, grouped by each
frame ABOVE the match (who called it), level by level.
"""
import argparse
import collections
import os
import re
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prof_report import inline_chains, short  # noqa: E402

# Frames that only pass a request along. gc_arena's `Gc::new::<T>` is kept: its
# type parameter names what was allocated.
PLUMBING = re.compile(
    r"^(?:__rust_alloc|__rdl_alloc|__rust_realloc|__rg_alloc|__rg_realloc|"
    r"core::|alloc::|std::|<alloc::|<core::|<std::|hashbrown::|<hashbrown::|"
    r"smallvec::|<smallvec::|ruffle_switch::counting_alloc|<ruffle_switch::counting_alloc|"
    r"indexmap::|<indexmap::|fnv::|bitflags::)"
)


def read(path):
    data = open(path, "rb").read()
    if data[:8] != b"FNXHEAP1":
        sys.exit("not a FlashNX heap profile (bad magic)")
    interval, live, dropped, total, other = struct.unpack_from("<QQQQQ", data, 8)
    (nb,) = struct.unpack_from("<I", data, 48)
    pos = 52
    buckets = []
    for _ in range(nb):
        nbytes, count, depth = struct.unpack_from("<QIB", data, pos)
        pos += 13
        frames = list(struct.unpack_from("<%dI" % depth, data, pos))
        pos += 4 * depth
        buckets.append((nbytes, count, frames))
    return interval, live, dropped, total, other, buckets


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("profile")
    ap.add_argument("elf")
    ap.add_argument("--top", type=int, default=40)
    ap.add_argument("--focus", help="regex on a frame name: show who calls it")
    ap.add_argument("--levels", type=int, default=6, help="caller levels shown with --focus")
    args = ap.parse_args()
    interval, live, dropped, total, other, buckets = read(args.profile)
    mb = lambda b: b / (1024 * 1024)
    print("live samples %d (dropped %d), interval %d KB" % (live, dropped, interval // 1024))
    print("live memory sampled: %.0f MB (%.0f MB in stacks that did not fit the table)" % (mb(total), mb(other)))

    addrs = set()
    for _, _, frames in buckets:
        addrs.update(frames)
    chains = inline_chains(args.elf, addrs) or {}

    def expand(frames):
        out = []
        for a in frames:
            for fn, loc in chains.get(a, [("??", "??")]):
                out.append(short(fn))
        return out

    if args.focus:
        rx = re.compile(args.focus)
        hit = 0
        levels = [collections.Counter() for _ in range(args.levels)]
        for nbytes, count, frames in buckets:
            names = [n for n in expand(frames) if not PLUMBING.match(n)]
            idx = next((i for i, n in enumerate(names) if rx.search(n)), None)
            if idx is None:
                continue
            hit += nbytes
            for k in range(args.levels):
                j = idx + 1 + k
                levels[k][names[j] if j < len(names) else "(end of stack)"] += nbytes
        print("\n%.1f MB under frames matching %r" % (mb(hit), args.focus))
        for k, c in enumerate(levels):
            print("\n-- caller level %d --" % (k + 1))
            for name, nbytes in c.most_common(12):
                print("%7.1f MB  %s" % (mb(nbytes), name))
        return

    by_owner = collections.Counter()
    by_module = collections.Counter()
    owner_count = collections.Counter()
    expanded = []
    for nbytes, count, frames in buckets:
        names = expand(frames)
        meaningful = [n for n in names if not PLUMBING.match(n)]
        owner = meaningful[0] if meaningful else (names[0] if names else "??")
        by_owner[owner] += nbytes
        owner_count[owner] += count
        mod = re.sub(r"<", "", owner).split("::")
        by_module["::".join(mod[:2]) if len(mod) > 1 else mod[0]] += nbytes
        expanded.append((nbytes, count, meaningful[:8] or names[:8]))

    print("\n== by owner (the code that asked) ==")
    for name, nbytes in by_owner.most_common(args.top):
        print("%7.1f MB %5.1f%%  %6d samples  %s" % (mb(nbytes), 100 * nbytes / max(total, 1), owner_count[name], name))
    print("\n== by module of the owner ==")
    for name, nbytes in by_module.most_common(25):
        print("%7.1f MB %5.1f%%  %s" % (mb(nbytes), 100 * nbytes / max(total, 1), name))
    print("\n== biggest stacks ==")
    expanded.sort(key=lambda e: -e[0])
    for nbytes, count, names in expanded[: args.top // 2]:
        print("%7.1f MB  %6d samples" % (mb(nbytes), count))
        for n in names:
            print("            %s" % n)


if __name__ == "__main__":
    main()
