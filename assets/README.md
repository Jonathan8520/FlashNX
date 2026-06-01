# assets

Bundled into the final `.nro` / consumed by the build:

- `icon.jpg` — 256×256 JPEG icon shown by hbmenu (wired via `--icon=` in `cpp/Makefile`).
- `banner.png` — 720×144 RGBA logo, embedded via `include_bytes!` in
  [../rust/src/library.rs](../rust/src/library.rs) and drawn at the top of the library UI.
- `cacert.pem` — Mozilla CA bundle for libcurl HTTPS (archive.org import).
- `FlashNX.nacp` — application metadata (title, author, version), generated at
  build time by `switch_rules` from the `APP_*` vars in `cpp/Makefile`.

## screenshots/

Captures used in the project README only — not shipped in the `.nro`.
