// FlashNX in-app bug-report relay (Cloudflare Worker).
//
// The Switch app POSTs an anonymous JSON report here; this Worker turns it into
// a GitHub issue using a token that lives ONLY here (never in the .nro). See
// README.md for deployment. The app endpoint is `crate::bugreport::BUG_REPORT_ENDPOINT`.
//
// Env (set as Worker secrets / vars):
//   GITHUB_TOKEN  (secret)  fine-grained PAT, "Issues: write" on REPO only
//   GITHUB_REPO   (var)     e.g. "Jonathan8520/FlashNX"   (owner/name)
//
// Request:  POST /report   Content-Type: application/json
//   { game, file, size, swf_version, compression, as3, app_version, lang,
//     applet, description, log_tail }
// Response: 200 { "ok": true, "url": "<issue html_url>" }  on success.

// Headroom for `log_tail`: the app sends up to 6 KB of log, and JSON escaping
// of the newlines inflates it further. Was 16 KB back when a report was <2 KB.
const MAX_BODY = 48 * 1024;

// How much of the log tail reaches the issue. GitHub rejects an issue body over
// 65536 characters and the log is the only unbounded part, so cap it here too
// rather than trusting the client's own limit.
const MAX_LOG = 12 * 1024;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    // Community-profile "most applied" counter (#20, Phase 3), backed by KV.
    if (url.pathname === "/applied" && request.method === "POST") {
      return handleApplied(request, env);
    }
    if (url.pathname === "/counts" && request.method === "GET") {
      return handleCounts(env);
    }

    if (url.pathname !== "/report") {
      return json({ ok: false, error: "not found" }, 404);
    }
    if (request.method !== "POST") {
      return json({ ok: false, error: "POST only" }, 405);
    }

    // Read + size-guard the body.
    const raw = await request.text();
    if (raw.length > MAX_BODY) {
      return json({ ok: false, error: "body too large" }, 413);
    }
    let r;
    try {
      r = JSON.parse(raw);
    } catch {
      return json({ ok: false, error: "bad json" }, 400);
    }

    if (!env.GITHUB_TOKEN || !env.GITHUB_REPO) {
      return json({ ok: false, error: "worker not configured" }, 500);
    }

    // Community profile shares are committed STRAIGHT to the repo (#20 auto-push)
    // instead of opening an issue, so the catalog self-updates with no manual
    // curation — the community sorts quality out via the "most applied" counter.
    // Requires the token to have "Contents: write" on the repo.
    if (r.kind === "profile") {
      return handleProfileShare(r, env);
    }
    // Remove one of the caller's OWN shared profiles (#20). Ownership is proved by
    // the install's secret owner token (see checkOrClaimOwner), so this can't
    // delete someone else's.
    if (r.kind === "profile_delete") {
      return handleProfileDelete(r, env);
    }

    const issue = buildIssue(r);
    const ghUrl = `https://api.github.com/repos/${env.GITHUB_REPO}/issues`;
    const gh = await fetch(ghUrl, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${env.GITHUB_TOKEN}`,
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "FlashNX-bug-report-worker",
        "Content-Type": "application/json",
      },
      body: JSON.stringify(issue),
    });

    if (!gh.ok) {
      const detail = await gh.text();
      return json({ ok: false, error: `github ${gh.status}`, detail }, 502);
    }
    const data = await gh.json();
    return json({ ok: true, url: data.html_url });
  },
};

// Build a GitHub issue {title, body, labels} from the report. `kind` selects a
// bug or a suggestion (different title + label). Everything the player typed is
// fenced so it can't ping users or break the markdown layout.
function buildIssue(r) {
  const s = (v, n = 200) => String(v ?? "").slice(0, n);
  const desc = s(r.description, 4000).trim();
  const descBlock = desc
    ? "```\n" + desc.replace(/```/g, "`​``") + "\n```\n\n"
    : "_(none provided)_\n\n";
  const env = `App version: ${s(r.app_version, 16)} · Language: ${s(r.lang, 8)}`;

  // Suggestion / feature request — no game metadata.
  if (r.kind === "suggestion") {
    const snippet = desc.split("\n")[0].slice(0, 60) || "(idea)";
    return {
      title: `[suggestion] ${snippet}`,
      body:
        `Sent from inside FlashNX (anonymous in-app suggestion).\n\n` +
        `### Suggestion\n\n${descBlock}${env}\n`,
      labels: ["enhancement"],
    };
  }

  // Bug report (default).
  const game = s(r.game, 120) || "(unknown game)";
  const meta = [
    ["Game", game],
    ["File", s(r.file, 200)],
    ["Source URL", s(r.source_url, 300) || "(local file / unknown)"],
    ["Size", typeof r.size === "number" ? `${r.size} bytes` : s(r.size)],
    ["SWF version", s(r.swf_version, 8)],
    ["Compression", s(r.compression, 8)],
    ["ActionScript 3", r.as3 ? "yes" : "no"],
    ["App version", s(r.app_version, 16)],
    ["Language", s(r.lang, 8)],
    ["Applet mode", r.applet ? "yes" : "no"],
  ]
    .map(([k, v]) => `| ${k} | ${v} |`)
    .join("\n");

  // Tail of the app's own log, sent with bug reports only. Its value is that
  // Ruffle's internal warnings go through the same sink, so this is the
  // `[tr/WARN]` list for the reported game. Collapsed by default: it is long,
  // and it should not bury what the player actually wrote.
  const rawLog = String(r.log_tail ?? "");
  const logBlock = rawLog.trim()
    ? `\n<details>\n<summary>Technical log (last ${
        rawLog.length > MAX_LOG ? `${MAX_LOG} of ${rawLog.length}` : rawLog.length
      } bytes of the session)</summary>\n\n` +
      "```\n" +
      rawLog.slice(-MAX_LOG).replace(/```/g, "`​``") +
      "\n```\n\n</details>\n"
    : "";

  const body =
    `Reported from inside FlashNX (anonymous in-app report).\n\n` +
    `### Description\n\n${descBlock}` +
    `### Game info\n\n| Field | Value |\n| --- | --- |\n${meta}\n` +
    logBlock;

  return { title: `[in-app] ${game}`, body, labels: ["bug"] };
}

// Increment the apply count for a profile id (#20, Phase 3). No per-install
// dedup yet — a user re-applying the same profile bumps it slightly, which is
// fine for a low-stakes "most applied" signal. Needs the PROFILES_KV binding
// (see wrangler.toml); without it this no-ops so the app degrades gracefully.
async function handleApplied(request, env) {
  if (!env.PROFILES_KV) return json({ ok: false, error: "kv not configured" });
  let r;
  try {
    r = JSON.parse(await request.text());
  } catch {
    return json({ ok: false, error: "bad json" }, 400);
  }
  const id = String(r.id || "").slice(0, 64);
  if (!id) return json({ ok: false, error: "missing id" }, 400);
  const key = `count:${id}`;
  const cur = parseInt((await env.PROFILES_KV.get(key)) || "0", 10) || 0;
  await env.PROFILES_KV.put(key, String(cur + 1));
  return json({ ok: true, count: cur + 1 });
}

// Return { "<id>": <applyCount>, ... } for all profiles (#20, Phase 3).
async function handleCounts(env) {
  if (!env.PROFILES_KV) return json({});
  const out = {};
  let cursor;
  do {
    const list = await env.PROFILES_KV.list({ prefix: "count:", cursor });
    for (const k of list.keys) {
      const id = k.name.slice("count:".length);
      out[id] = parseInt((await env.PROFILES_KV.get(k.name)) || "0", 10) || 0;
    }
    cursor = list.list_complete ? undefined : list.cursor;
  } while (cursor);
  return json(out);
}

// Dedicated, code-free branch the community profiles live on, so shares never
// pollute `main`'s history. Holds index.json + <id>.profile.json at its root.
// Create it once: git checkout --orphan community-profiles; clear; echo "[]" >
// index.json; commit; push.
const PROFILES_BRANCH = "community-profiles";

// Commit a shared profile + its index entry straight onto PROFILES_BRANCH (#20
// auto-push). No GitHub Action needed: the Worker maintains index.json itself.
// Profiles land `verified: false`; the picker sorts verified + "most applied"
// first, so the community surfaces quality on its own. Needs "Contents: write".
async function handleProfileShare(r, env) {
  const s = (v, n = 200) => String(v ?? "").slice(0, n);
  const title = s(r.title, 120) || "Unknown game";
  const fpUuid = s(r.fp_uuid, 64);
  const swfHash = s(r.swf_hash, 32);
  // Per-INSTALL suffix (the app's `install_id`): this is what lets two people
  // share controls for the SAME game land on DIFFERENT files (they coexist in
  // the catalog) instead of one clobbering the other. The same install
  // re-sharing a game reuses its id, so it UPDATES its own profile rather than
  // accumulating duplicates.
  const install = s(r.install_id, 16)
    .toLowerCase()
    .replace(/[^a-z0-9]/g, "")
    .slice(0, 8);
  // Ownership (#20): claim this install on its first share (TOFU) with a strong
  // server token, else require the stored one. Returns the authoritative token so
  // the app can persist it; a later UPDATE or DELETE must present it. Public
  // install_ids on the branch are already claimed by the time they're visible.
  const owner = await checkOrClaimOwner(env, install, s(r.owner_token, 64));
  if (!owner.ok) return json({ ok: false, error: owner.error }, owner.status || 403);
  const slug =
    title
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 40) || "profile";
  const gameSuffix = (swfHash || fpUuid || "").slice(0, 8);
  // id = slug + game-key + install-key. Joining only the non-empty parts keeps
  // legacy bundled ids (no suffix) clean and tolerates a missing install_id.
  const id = [slug, gameSuffix, install].filter(Boolean).join("-");
  // Group profiles under a per-game folder so the branch is browsable when many
  // pile up. The app fetches by the index's `file` field, so the path is opaque
  // to it — only this folder + the index entry need to agree.
  const file = `${slug}/${id}.profile.json`;

  const obj = (v) => (v && typeof v === "object" ? v : {});
  // Cursor speed preset index: keep it a sane small int, else -1 (unset).
  const cursorSpeed =
    Number.isInteger(r.cursor_speed) && r.cursor_speed >= 0 && r.cursor_speed <= 20
      ? r.cursor_speed
      : -1;
  const profile = {
    schema: 1,
    id,
    game: { title, fp_uuid: fpUuid, swf_hash: swfHash },
    // Optional display nickname (#20) — anonymous label, not an identity.
    author: s(r.author, 40),
    verified: false,
    bindings: obj(r.bindings),
    bindings_p2: obj(r.bindings_p2),
    // Per-modifier combo layers (#57) + per-game cursor speed, so a shared profile
    // carries the whole control setup, not just the base bindings.
    combo_layers: obj(r.combo_layers),
    combo_layers_p2: obj(r.combo_layers_p2),
    cursor_speed: cursorSpeed,
    // Per-game "show cursor" toggle (default true when absent/unspecified), so a
    // shared "hide the pointer" profile survives the round-trip.
    show_cursor: r.show_cursor === false ? false : true,
  };
  const indexEntry = { id, title, fp_uuid: fpUuid, swf_hash: swfHash, file, verified: false };

  // ONE commit carries both the profile file AND the index.json upsert (the old
  // Contents-API path made two separate commits per share). Git Data API.
  const res = await commitShare(
    env,
    PROFILES_BRANCH,
    file,
    JSON.stringify(profile, null, 2) + "\n",
    indexEntry,
    `profile: ${title} (${id})`
  );
  if (!res.ok) return json({ ok: false, error: res.error }, 502);
  // Return the owner token so the app persists it (first share) — needed to
  // update or delete this profile later.
  return json({ ok: true, id, token: owner.token });
}

// Delete one of the caller's OWN shared profiles (#20). Two checks: the id must
// carry this install's suffix (defense in depth) AND the secret owner token must
// match the one claimed for this install (no TOFU on delete — an unclaimed
// install can't delete anything). Removes the file + index entry in one commit.
async function handleProfileDelete(r, env) {
  if (!env.GITHUB_TOKEN || !env.GITHUB_REPO) {
    return json({ ok: false, error: "worker not configured" }, 500);
  }
  if (!env.PROFILES_KV) {
    return json({ ok: false, error: "ownership store unavailable" }, 500);
  }
  const s = (v, n = 200) => String(v ?? "").slice(0, n);
  const id = s(r.id, 80);
  const install = s(r.install_id, 16).toLowerCase().replace(/[^a-z0-9]/g, "").slice(0, 8);
  const token = s(r.owner_token, 64);
  if (!id || !install) return json({ ok: false, error: "missing id / install_id" }, 400);
  if (!id.endsWith(`-${install}`)) return json({ ok: false, error: "not your profile" }, 403);
  const stored = await env.PROFILES_KV.get(`owner:${install}`);
  if (!stored || !ctEq(token, stored)) {
    return json({ ok: false, error: "not the owner" }, 403);
  }
  const res = await commitDelete(env, PROFILES_BRANCH, id, `profile delete (${id})`);
  if (!res.ok) return json({ ok: false, error: res.error }, 502);
  // Best-effort: drop the now-orphaned apply counter.
  try { await env.PROFILES_KV.delete(`count:${id}`); } catch {}
  // `deleted` distinguishes a real removal from "that id was not in the catalog".
  // Older clients read `ok` alone and are unaffected.
  return json({ ok: true, deleted: res.deleted !== false });
}

// Ownership store for #20 profiles, in PROFILES_KV under `owner:<install_id>`.
// First share for an install CLAIMS it (trust-on-first-use) with a strong token
// (reuse the app's if it already has one — KV-loss recovery — else mint one with
// the runtime CSPRNG); later shares/deletes must present it. The install_id is a
// PUBLIC dedup key (it's in every shared id); this token is the real secret and
// never appears in any public artifact. Returns { ok, token, error?, status? }.
// Degrades to allow (no token) when KV isn't bound, so shares still work.
async function checkOrClaimOwner(env, install, providedToken) {
  if (!env.PROFILES_KV || !install) return { ok: true, token: "" };
  const key = `owner:${install}`;
  const stored = await env.PROFILES_KV.get(key);
  if (!stored) {
    const token =
      providedToken && providedToken.length >= 16
        ? providedToken
        : crypto.randomUUID().replace(/-/g, "");
    await env.PROFILES_KV.put(key, token);
    return { ok: true, token };
  }
  if (ctEq(providedToken, stored)) return { ok: true, token: stored };
  return { ok: false, status: 403, error: "not the owner of this install id" };
}

// Constant-time string compare. The tokens are high-entropy so a timing oracle
// is impractical anyway, but this costs nothing and avoids the early-out.
function ctEq(a, b) {
  a = String(a);
  b = String(b);
  if (a.length === 0 || a.length !== b.length) return false;
  let d = 0;
  for (let i = 0; i < a.length; i++) d |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return d === 0;
}

// Apply a change to `branch` in a SINGLE commit via the Git Data API
// (ref -> base tree -> new tree -> commit -> move ref). `mutate(list)` gets the
// current index.json array and returns { list, tree } — the new index plus the
// file tree ops (a blob with `content` writes/overwrites; a blob with `sha: null`
// deletes). Return { done: true } to no-op. Retries on a non-fast-forward (422)
// when a concurrent share moved the tip between our read and our ref update.
async function commitTreeChange(env, branch, message, mutate) {
  const headers = ghHeaders(env);
  const base = `https://api.github.com/repos/${env.GITHUB_REPO}`;
  const ref = `heads/${encodeURIComponent(branch)}`;
  for (let attempt = 0; attempt < 3; attempt++) {
    // 1. Current branch tip.
    const refRes = await fetch(`${base}/git/ref/${ref}`, { headers });
    if (!refRes.ok) {
      return { ok: false, error: `ref ${refRes.status}: ${(await refRes.text()).slice(0, 200)}` };
    }
    const tipSha = (await refRes.json()).object.sha;
    // 2. The tree that tip points at (so our new tree only changes the named files).
    const tipCommitRes = await fetch(`${base}/git/commits/${tipSha}`, { headers });
    if (!tipCommitRes.ok) {
      return { ok: false, error: `commit ${tipCommitRes.status}` };
    }
    const baseTree = (await tipCommitRes.json()).tree.sha;
    // 3. Read index.json AT THE TIP commit (pinned to tipSha so our edit matches
    //    base_tree exactly).
    //    Every failure here is FATAL. It used to fall through with `list = []`,
    //    and the share then committed a one-element array as the new index — the
    //    sharer's own profile published fine while every other profile in the
    //    catalog became unlisted for everyone, since the app only reads
    //    index.json. The parse branch is the one that actually fires: past 1 MB
    //    the contents API returns an empty `content` string. Only a real 404 may
    //    legitimately start from an empty list (first profile ever shared).
    let list = [];
    const idxRes = await fetch(`${base}/contents/index.json?ref=${tipSha}`, { headers });
    if (idxRes.ok) {
      try {
        list = JSON.parse(atobUtf8((await idxRes.json()).content));
      } catch (e) {
        return { ok: false, error: `index parse: ${e && e.message ? e.message : e}` };
      }
      if (!Array.isArray(list)) {
        return { ok: false, error: "index is not an array" };
      }
    } else if (idxRes.status !== 404) {
      return { ok: false, error: `index read ${idxRes.status}` };
    }
    const m = mutate(list);
    // Nothing to change (e.g. delete of an absent profile). `deleted` carries
    // through so the caller does not read "we did nothing" as "we did it".
    if (m.done) return { ok: true, deleted: m.deleted !== false };
    // 4. New tree: the caller's file ops + the rewritten index (inline content =
    //    blob created server-side; sha:null removes a path).
    const treeRes = await fetch(`${base}/git/trees`, {
      method: "POST",
      headers,
      body: JSON.stringify({
        base_tree: baseTree,
        tree: [
          ...m.tree,
          { path: "index.json", mode: "100644", type: "blob", content: JSON.stringify(m.list, null, 2) + "\n" },
        ],
      }),
    });
    if (!treeRes.ok) {
      return { ok: false, error: `tree ${treeRes.status}: ${(await treeRes.text()).slice(0, 200)}` };
    }
    const newTree = (await treeRes.json()).sha;
    // 5. The single commit.
    const commitRes = await fetch(`${base}/git/commits`, {
      method: "POST",
      headers,
      body: JSON.stringify({ message, tree: newTree, parents: [tipSha] }),
    });
    if (!commitRes.ok) {
      return { ok: false, error: `mkcommit ${commitRes.status}` };
    }
    const newCommit = (await commitRes.json()).sha;
    // 6. Fast-forward the branch. 422 = the tip moved under us (another share);
    //    rebuild on the new tip and retry.
    const upd = await fetch(`${base}/git/refs/${ref}`, {
      method: "PATCH",
      headers,
      body: JSON.stringify({ sha: newCommit, force: false }),
    });
    if (upd.ok) return { ok: true };
    if (upd.status !== 422) {
      return { ok: false, error: `updateref ${upd.status}: ${(await upd.text()).slice(0, 200)}` };
    }
  }
  return { ok: false, error: "updateref conflicted after retries" };
}

// Write `file` + upsert its index entry in one commit.
function commitShare(env, branch, file, fileContent, indexEntry, message) {
  return commitTreeChange(env, branch, message, (list) => {
    const next = list.filter((e) => e && e.id !== indexEntry.id);
    next.push(indexEntry);
    next.sort((a, b) => String(a.id).localeCompare(String(b.id)));
    return { list: next, tree: [{ path: file, mode: "100644", type: "blob", content: fileContent }] };
  });
}

// Remove a profile (by id) + its index entry in one commit. Idempotent: a
// no-op success if the id isn't in the catalog. Looks the file path up from the
// index so it stays correct whatever the per-game folder layout.
function commitDelete(env, branch, id, message) {
  return commitTreeChange(env, branch, message, (list) => {
    const entry = list.find((e) => e && e.id === id);
    // `deleted: false` so the caller can tell "already gone" from "removed". The
    // bare `{done:true}` came back as a plain success, so the app reported "your
    // shared profile was deleted", dropped the row and demoted the local tag while
    // the profile was still published — and it reappeared in the picker the same
    // session. Reachable without any GitHub failure, since an index that failed
    // to load used to arrive here empty.
    if (!entry) return { done: true, deleted: false };
    const next = list.filter((e) => e && e.id !== id);
    const tree = entry.file
      ? [{ path: entry.file, mode: "100644", type: "blob", sha: null }]
      : [];
    return { list: next, tree };
  });
}

function ghHeaders(env) {
  return {
    Authorization: `Bearer ${env.GITHUB_TOKEN}`,
    Accept: "application/vnd.github+json",
    "X-GitHub-Api-Version": "2022-11-28",
    "User-Agent": "FlashNX-bug-report-worker",
    "Content-Type": "application/json",
  };
}

// Decode GitHub's base64 (newline-wrapped) file content as UTF-8.
function atobUtf8(b64) {
  const bin = atob(String(b64).replace(/\s/g, ""));
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

function json(obj, status = 200) {
  return new Response(JSON.stringify(obj), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
