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
//     applet, description }
// Response: 200 { "ok": true, "url": "<issue html_url>" }  on success.

const MAX_BODY = 16 * 1024; // generous cap; the app sends <2 KB

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

  const body =
    `Reported from inside FlashNX (anonymous in-app report).\n\n` +
    `### Description\n\n${descBlock}` +
    `### Game info\n\n| Field | Value |\n| --- | --- |\n${meta}\n`;

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
  // Stable id: a slug of the title + a short hash/uuid suffix for uniqueness.
  const slug =
    title
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 40) || "profile";
  const suffix = (swfHash || fpUuid || "").slice(0, 8);
  const id = suffix ? `${slug}-${suffix}` : slug;
  const file = `${id}.profile.json`;

  const profile = {
    schema: 1,
    id,
    game: { title, fp_uuid: fpUuid, swf_hash: swfHash },
    author: "",
    verified: false,
    bindings: r.bindings && typeof r.bindings === "object" ? r.bindings : {},
    bindings_p2:
      r.bindings_p2 && typeof r.bindings_p2 === "object" ? r.bindings_p2 : {},
  };

  // 1. The profile file.
  const put = await putFile(
    env,
    PROFILES_BRANCH,
    file,
    JSON.stringify(profile, null, 2) + "\n",
    `profile: ${title} (${id})`
  );
  if (!put.ok) return json({ ok: false, error: put.error }, 502);

  // 2. Upsert its entry into index.json (same branch).
  await upsertIndex(env, PROFILES_BRANCH, {
    id,
    title,
    fp_uuid: fpUuid,
    swf_hash: swfHash,
    file,
    verified: false,
  });

  return json({ ok: true, id });
}

// PUT a UTF-8 file onto `branch` (create or update). Returns {ok} / {ok,error}.
async function putFile(env, branch, path, content, message) {
  const url = `https://api.github.com/repos/${env.GITHUB_REPO}/contents/${path}`;
  const headers = ghHeaders(env);
  let sha;
  const head = await fetch(`${url}?ref=${encodeURIComponent(branch)}`, { headers });
  if (head.ok) sha = (await head.json()).sha;
  const put = await fetch(url, {
    method: "PUT",
    headers,
    body: JSON.stringify({ message, content: b64utf8(content), branch, ...(sha ? { sha } : {}) }),
  });
  if (!put.ok) {
    return { ok: false, error: `github ${put.status}: ${(await put.text()).slice(0, 200)}` };
  }
  return { ok: true };
}

// Read index.json on `branch`, upsert `entry` (keyed by id), write it back.
// One retry on a 409 (a concurrent share updated it between our read + write).
async function upsertIndex(env, branch, entry) {
  const url = `https://api.github.com/repos/${env.GITHUB_REPO}/contents/index.json`;
  const headers = ghHeaders(env);
  for (let attempt = 0; attempt < 2; attempt++) {
    let list = [];
    let sha;
    const head = await fetch(`${url}?ref=${encodeURIComponent(branch)}`, { headers });
    if (head.ok) {
      const j = await head.json();
      sha = j.sha;
      try {
        list = JSON.parse(atobUtf8(j.content));
      } catch {
        list = [];
      }
      if (!Array.isArray(list)) list = [];
    }
    list = list.filter((e) => e && e.id !== entry.id);
    list.push(entry);
    list.sort((a, b) => String(a.id).localeCompare(String(b.id)));
    const put = await fetch(url, {
      method: "PUT",
      headers,
      body: JSON.stringify({
        message: `index: ${entry.id}`,
        content: b64utf8(JSON.stringify(list, null, 2) + "\n"),
        branch,
        ...(sha ? { sha } : {}),
      }),
    });
    if (put.ok || put.status !== 409) return;
  }
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

// Base64 of a UTF-8 string (btoa is Latin1-only; game titles can be CJK).
function b64utf8(str) {
  const bytes = new TextEncoder().encode(str);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
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
