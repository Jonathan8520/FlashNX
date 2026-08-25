# Ruffle patches (third_party/ruffle)

Local patches applied to the `third_party/ruffle/` submodule. Must be
re-applied after any `git submodule update --remote`.

## Application

```bash
# From the project root:
cd third_party/ruffle
for p in ../../patches/*.patch; do
    git apply "$p"
done
```

## List

### 0001-mario63-zero-scale-hit-test.patch

**Fix Phase 2.4.a — missing Toad castle (issue #6906).**

Adds a zero-determinant matrix guard in `hit_test_bounds` and
`hit_test_shape` of `core/src/display_object.rs`. Without this patch, Mario 63
treats a zero-scale placeholder MC of the castle as "hittable", which
breaks the logic chain: Toad NPC not instantiated, Mario floating in
the void, progression blocked.

To be submitted as an upstream PR to revive and close #6906 for good.
The patch is also useful to any other Ruffle frontend (Web, desktop) — the
Adobe Flash Player parity is correct.

### 0002-pixelbender-shaderjob-run-noop.patch

**Keep PixelBender games alive when the renderer can't run shaders.**

The Switch GL backend (like Ruffle's webgl backend) does not implement
PixelBender. Our `compile_pixelbender_shader` returns a handle that just
carries the parsed shader (see `rust/src/backend/render.rs`), so AVM2
`Shader` / `ShaderData` / `ShaderFilter` construction succeeds and games
keep their input and frame logic. `run_pixelbender_shader` still fails, so
this patch turns `ShaderJob.start`'s `.expect("Failed to run shader")` into
a warn + no-op (the job leaves its target untouched) instead of aborting
the app. The renderer already skips `Filter::ShaderFilter`, so the only
visible difference is the absent shader effect. Validated on The Terminal
(failsafegames): enemies, shooting and the pause menu work; before, the
game built a `ShaderFilter` every frame in its enterFrame/click handlers,
so a hard error there silently broke all input.

### 0003-amf-cycle-serialize-crash.patch

**Stop the AMF serializer crashing/freezing on a cyclic SharedObject.**

`serialize_value` (`core/src/avm2/amf.rs`) fills its reference table
(`object_table`) only AFTER recursing into an object, so a SharedObject with a
circular reference recurses forever: a stack overflow (crash) for a simple
cycle, or exponential re-serialization (hang) for a branching one. This patch
detects cycles up front — a thread-local set of the objects currently on the
serialization stack, keyed by `as_ptr` — and returns `None` for a back-reference
instead of recursing, plus a depth backstop for pathological acyclic nesting.
The `.sol` save drops the cyclic back-pointer (slightly lossy) but is finite and
valid. Validated on Hemp Tycoon, which crashed when planting (it flushes a
cyclic save on every action): the game now plays and saves. Upstream master
still has no guard (checked 2026-06).

### Not carried as numbered patches

`ruffle-local.diff` is the full snapshot of `third_party/ruffle` and is the
only complete record. Several fixes live there without a numbered patch,
because their file already carries unrelated local changes that a standalone
`.patch` would duplicate. The notable ones:

**`core/src/character.rs` — decoded-bitmap budget, and `decode_or_stand_in`.**

The budget refuses a bitmap once the movie's decoded-bitmap allowance is
spent, so a huge SWF loses sprites instead of the process losing its heap.
`reset_bitmap_cache` re-sizes it per movie; FlashNX calls it from
`ensure_swf_loaded` on both the cached and uncached paths (a RESTART that
skipped it left the counter full and refused nearly every bitmap: 5163
refusals on Super Smash Flash 2, against 45 once fixed).

`decode_or_stand_in` exists because decoding allocates width x height x 4
bytes and is therefore among the first things to fail on an exhausted heap,
while three upstream call sites unwrap it: `library.rs`
(`instantiate_display_object`), `avm2/globals/flash/display/bitmap_data.rs`
(`fill_bitmap_data_from_symbol`) and `avm1/globals/bitmap_data.rs`. All three
now take a 1x1 transparent stand-in instead of killing the process. The
stand-in is deliberate and `None` is not an option: `clone_sprite` in
`avm1/globals/movie_clip.rs` unwraps that `Option`, so returning `None` would
merely move the panic to `duplicateMovieClip`.

**`core/src/player.rs` — GC and frame-pacing probes.**

`flashnx_gc_probe` publishes, per host frame, the number of SWF frames the
tick actually ran, the collector phase, the arena's total allocation and the
microseconds spent inside the collector. Added to settle whether a periodic
28-frame stall was the collector or frame catch-up; it was neither, it was
newlib's `free`. Diagnostic only, and a candidate for removal once the
allocator work is finished.
