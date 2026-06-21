# Community control profiles

Ready-made control layouts ("profiles") that FlashNX can apply to a game, so
players don't have to configure controls by hand (issue #20).

## How it works

- The app fetches `index.json` (the match keys + file names), then a single
  `<id>.profile.json` when it matches the game the player opened.
- A game is matched by, in priority order: Flashpoint UUID → a content hash of
  the `.swf` → a normalized title (title is fuzzy: only ever *suggested*).
- `index.json` is generated from the `*.profile.json` files by
  `tools/build-profiles-index.mjs` (run automatically by
  `.github/workflows/community-profiles.yml` on push). **Don't edit it by hand.**

## Adding a profile

Profiles come from players sharing their controls in the app
(Settings/OPTIONS → "Share controls"), which opens a `profile`-labelled GitHub
issue containing a ready-to-paste JSON. A maintainer reviews it and commits it
here as `community-profiles/<id>.profile.json` (set a unique `id`, a real
`author`, and `verified: true` once tested). The index rebuilds on push.

## Profile format

```json
{
  "schema": 1,
  "id": "unique-id",
  "game": { "title": "Game Title", "fp_uuid": "", "swf_hash": "" },
  "author": "contributor",
  "verified": true,
  "notes": "short description of the layout",
  "bindings": { "A": "Space", "Left": "Left", "ZR": "Left click" },
  "bindings_p2": {}
}
```

- Set `game.fp_uuid` for a Flashpoint game (covers every copy) and/or
  `game.swf_hash` for an exact file match. At least one is recommended;
  title-only profiles are shown as suggestions, never auto-applied.
- Key names are the canonical FlashNX identifiers (`Space`, `A`..`Z`, `0`..`9`,
  `Left`/`Right`/`Up`/`Down`, `Left click`, `Right click`, etc.). An empty
  string unbinds a button.
