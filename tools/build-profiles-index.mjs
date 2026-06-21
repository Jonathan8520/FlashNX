// Regenerate community-profiles/index.json from the *.profile.json files.
//
// The app fetches index.json to know which profiles exist + their match keys
// (Flashpoint UUID / .swf hash / title) without downloading every file. Run on
// push by .github/workflows/community-profiles.yml; also runnable by hand:
//   node tools/build-profiles-index.mjs
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const DIR = "community-profiles";
const files = readdirSync(DIR)
  .filter((f) => f.endsWith(".profile.json"))
  .sort();

const index = [];
for (const file of files) {
  let p;
  try {
    p = JSON.parse(readFileSync(join(DIR, file), "utf8"));
  } catch (e) {
    console.error(`skip ${file}: ${e.message}`);
    continue;
  }
  const game = p.game || {};
  index.push({
    id: p.id || file.replace(/\.profile\.json$/, ""),
    title: game.title || "",
    fp_uuid: game.fp_uuid || "",
    swf_hash: game.swf_hash || "",
    file,
    verified: !!p.verified,
  });
}
index.sort((a, b) => a.id.localeCompare(b.id));
writeFileSync(join(DIR, "index.json"), JSON.stringify(index, null, 2) + "\n");
console.log(`Wrote ${index.length} entries to ${DIR}/index.json`);
