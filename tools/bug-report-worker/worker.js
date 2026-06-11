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
    if (request.method !== "POST") {
      return json({ ok: false, error: "POST only" }, 405);
    }
    const url = new URL(request.url);
    if (url.pathname !== "/report") {
      return json({ ok: false, error: "not found" }, 404);
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

function json(obj, status = 200) {
  return new Response(JSON.stringify(obj), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
