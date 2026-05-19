# third_party

External dependencies pulled in as git submodules.

## Adding Ruffle (Phase 1)

```
git submodule add https://github.com/ruffle-rs/ruffle.git third_party/ruffle
git -C third_party/ruffle checkout <tag-or-commit-pin>
```

Then uncomment the `ruffle_core` / `ruffle_render` lines in `../rust/Cargo.toml`.
