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
  -d '{"game":"Test Game","file":"test.swf","size":123,"swf_version":9,"compression":"CWS","as3":true,"app_version":"1.2.0","lang":"fr","applet":false,"description":"sanity check, please close"}'
# -> {"ok":true,"url":"https://github.com/.../issues/NN"}
```

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
