# libdrm_nouveau (FlashNX copy)

The layer between Mesa's nouveau driver and the Switch GPU services, built into
FlashNX in place of devkitPro's `-ldrm_nouveau`.

- Upstream: https://github.com/devkitPro/libdrm_nouveau at commit
  `137265185c95cf58ce94c7afecba14f895dac2a7` (2020-05-11). That is the source of
  the installed `switch-libdrm_nouveau 1.0.1-2` package: its four objects define
  the same symbols, and its headers match the ones in
  `$DEVKITPRO/portlibs/switch/include`, which this copy still compiles against so
  Mesa's view of every structure stays the same.
- License: MIT, in each source file's header.
- `config.h` is empty on purpose: `bufctx.c` includes it and upstream's build
  provides none.
- Built with `-DNDEBUG`, like the package.

## What FlashNX changes

Only `nouveau.c` and `private.h`, and only to add a buffer-object cache: freed
colour-tiled BOs of at most 8 MiB are kept and handed back to the next request
of the same alignment, kind and coherence whose size they cover by no more than
25 % (the smallest such, an exact size first). The block comment above
`nouveau_bo_del` in `nouveau.c` explains why and what makes a recycled BO
indistinguishable from a fresh one: a larger one is still a whole BO, zeroed
entirely, and returns to the cache at its own size.

If a fresh allocation fails, the cache is emptied and the allocation retried
once, so parked blocks can never be why one fails. The same holds outside
libdrm: the Rust global allocator (`rust/src/lib.rs`, `reclaim_gpu_cache`) calls
`flashnx_boc_trim` and retries when newlib refuses a block, since the parked
blocks live in that same heap.

Three functions are exported for the Rust side:

- `flashnx_boc_set(int on)`. The cache is on by default. Turning it off empties
  it. Nothing calls it in the shipped code; it is what the 2026-09-23
  measurement build used to alternate on and off within one session.
- `uint64_t flashnx_boc_trim(void)` empties it, keeps it on, and returns the
  bytes given back. The renderer calls it each time a backend is created (a
  game starting, restarting, or leaving), and the Rust allocator when the heap
  is full.
- `flashnx_boc_stats(uint64_t *out, int n)` returns running counters, then the
  number of entries and the bytes held.
- `flashnx_boc_set_limits(max_bo_bytes, max_bytes, slack_pct)` changes the
  largest BO kept, the most bytes held and the slack, evicting what no longer
  fits. Unused in the shipped code; the profiling build's A/B used it to put
  1 MiB / 16 MiB / exact size (what shipped first) against the current values.

The limits moved on 2026-09-24, after the sampling profiler (`cpp/src/prof.cpp`)
showed texture creation still at 7 to 15 % of Mario 63's heavy frames, nearly
all IPC to nvservices: the cache was holding ~66 entries, far below its limits,
while ~6.4 requests a frame missed on sizes a few percent off every entry and
~4.8 entries a frame aged out unused.

Every other line is upstream's.
