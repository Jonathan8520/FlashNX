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
