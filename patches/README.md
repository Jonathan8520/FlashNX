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
