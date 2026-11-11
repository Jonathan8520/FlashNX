# FlashNX in-app bug-report relay

This tiny Cloudflare Worker turns the anonymous bug reports the FlashNX app
sends into GitHub issues on the FlashNX repo. The player never logs in: they
pick the broken `.swf`, type a short note, press send.

## Why this exists (don't put the token in the app)

Creating a GitHub issue needs a credential. The `.nro` is a **public** homebrew
binary, so a token baked into it could be extracted and used to spam or vandalise
the repo, and GitHub's secret-scanning auto-revokes tokens it finds in published
files. So the token has to live somewhere the public can't read it. This Worker
holds a token scoped to "create issues on one repo" and nothing else.

## Deploy (about 5 minutes, free tier)

1. **Create a fine-grained Personal Access Token**
   GitHub → Settings → Developer settings → *Fine-grained tokens* → Generate.
   - Resource owner: your account.
   - Repository access: *Only select repositories* → `Jonathan8520/FlashNX`.
   - Repository permissions: **Issues → Read and write** (leave everything else
     "No access").
   - Copy the token (starts with `github_pat_...`).

2. **Create the Worker**
   - With Wrangler:
     ```sh
     npm i -g wrangler
     wrangler login
     cd tools/bug-report-worker
     wrangler deploy            # uses wrangler.toml below
     wrangler secret put GITHUB_TOKEN   # paste the PAT
     ```
   - Or in the Cloudflare dashboard: Workers & Pages → Create → paste
     `worker.js`, add the variables below, Deploy.

3. **Set the variables**
   - `GITHUB_TOKEN` — *secret* — the fine-grained PAT from step 1.
   - `GITHUB_REPO`  — *plain var* — `Jonathan8520/FlashNX`.

4. **Wire the app to the Worker**
   Copy the deployed URL (e.g.
   `https://flashnx-bug-report.<your-subdomain>.workers.dev`) and set, in
   `rust/src/bugreport.rs`:
   ```rust
   pub const BUG_REPORT_ENDPOINT: &str =
       "https://flashnx-bug-report.<your-subdomain>.workers.dev/report";
   ```
   (Until this points at a real deployment, the app shows a clear
   "endpoint not configured" message instead of sending.)

## Test

```sh
curl -X POST "$WORKER_URL/report" \
  -H "Content-Type: application/json" \
  -d '{"game":"Test Game","file":"test.swf","size":123,"disk_size":0,"swf_version":9,"compression":"CWS","as3":true,"app_version":"1.2.0","lang":"fr","applet":false,"companions":-1,"companion_dirs":-1,"power_mode":1,"docked":false,"description":"sanity check, please close"}'
# -> {"ok":true,"url":"https://github.com/.../issues/NN"}
```

## Translating Flashpoint descriptions (`/translate`)

X in a Flashpoint game's details window asks this Worker for the description
in the app's language. It runs on **Workers AI** (`@cf/google/gemma-4-26b-a4b-it`,
`@cf/meta/m2m100-1.2b` as a fallback) and keeps each translation in
`PROFILES_KV` under `tr:` keys for 180 days, so a description is translated once
for everybody.

- **Only real Flashpoint descriptions.** The app sends the game's id with its
  description; before a new translation the Worker reads that game's
  description from Flashpoint's search and answers 403 if the text differs.
  So the route cannot be used as a free LLM. A cached translation is served
  without the check (it was checked when it was made).
- The Worker needs the **`AI` binding**. `wrangler deploy` reads it from
  `wrangler.toml`. From the dashboard: the Worker → Settings → Bindings → Add →
  Workers AI, variable name `AI`.
- **Rate limit** (`TRANSLATE_LIMIT`): 20 new translations a minute per address.
  Only `wrangler deploy` sets it: the dashboard does not show rate-limit
  bindings. Without it the route still works, with no limit.
- Cost: about 15 Neurons per new translation. The free plan gives 10,000 a day
  (reset at 00:00 UTC), so roughly 650 new descriptions a day; past that the
  route answers 502 until the reset, and the app says the translation is
  unavailable. Cached ones cost nothing.
- To change the model (Cloudflare retires them), edit `TRANSLATE_MODEL`: no new
  app build is needed. Bump `TRANSLATE_CACHE` to drop the old translations.

```sh
curl -X POST "$WORKER_URL/translate" -H "Content-Type: application/json" \
  -d '{"id":"1a17d483-7bc8-412e-bfad-f2d4ae272856","text":"<the game'"'"'s originalDescription>","lang":"fr"}'
# -> {"ok":true,"text":"..."}; any other text -> 403
```

## Download badge (`/downloads`)

`GET /downloads` answers the README's badge in shields.io's endpoint format:
the downloads of every GitHub release asset plus the Homebrew App Store's
`app_dls` for FlashNX (from its `repo.json`, which shields.io cannot filter
itself). Kept an hour in the Worker's cache; a source that fails keeps its last
good number in KV (`downloads:github`, `downloads:hbas`).

```sh
curl "$WORKER_URL/downloads"
# -> {"schemaVersion":1,"label":"downloads","message":"4.9k","color":"blue","cacheSeconds":3600}
```

## Deploy order (Worker first)

The Worker tolerates a report that omits `disk_size`, `companions`,
`companion_dirs`, `power_mode` and `docked`: a copy of the app already installed
on someone's card keeps posting the 1.8.0 shape forever, and those rows are simply
left off its issues rather than filled with defaults nobody measured. So deploy
this Worker **before** releasing the app build that sends them: the reverse order
silently drops the new fields on the floor and the issue looks unchanged.

## Abuse notes

- The token can only open issues on the one repo — worst case is issue spam, not
  data loss or code access. Revoke/rotate it any time from GitHub.
- The Worker caps the body size and fences the free-text so reports can't inject
  markdown or @-mention people.
- A shared secret in the app wouldn't help (the binary is public, so it'd be
  extractable). If spam ever becomes a problem, add a Cloudflare **Rate Limiting**
  rule on the Worker route, or require a header the app sends and rotate it.

## wrangler.toml

```toml
name = "flashnx-bug-report"
main = "worker.js"
compatibility_date = "2024-11-01"

[vars]
GITHUB_REPO = "Jonathan8520/FlashNX"
# GITHUB_TOKEN is a secret: `wrangler secret put GITHUB_TOKEN`
```
