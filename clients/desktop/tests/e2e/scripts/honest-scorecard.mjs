// Honest scorecard — the decontaminated composite.
//
// Reads a chaos journal + its length-blind re-judge sidecar
// (rejudge-length-blind.mjs). Where the sidecar has a verdict for a turn, its
// length-blind `broken` REPLACES the live UX judge's — so a correct concise
// answer the live judge dinged only for word-count no longer counts against the
// composite, and a padded fabrication the live judge waved through (because it
// hit the word count) now does. Turns with no sidecar verdict fall back to the
// live judge. Prints the honest POSITIVE-experience rate + the failure-category
// breakdown so the remaining real bugs are legible.
//
// Usage: node honest-scorecard.mjs <journal.jsonl> [--label X]
import fs from "node:fs";

const argv = process.argv.slice(2);
const journalF = argv.find((a) => !a.startsWith("--") && a.endsWith(".jsonl"));
const label = (() => {
  const i = argv.indexOf("--label");
  return i >= 0 ? argv[i + 1] : (journalF ?? "").split("/").pop();
})();
if (!journalF) {
  console.error("usage: node honest-scorecard.mjs <journal.jsonl> [--label X]");
  process.exit(2);
}
const sidecarF = journalF.replace(/\.jsonl$/, ".rejudge.jsonl");

const load = (f) =>
  fs
    .readFileSync(f, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => {
      try {
        return JSON.parse(l);
      } catch {
        return null;
      }
    })
    .filter(Boolean);

const rows = load(journalF);
const chats = rows.filter((r) => r.cmd === "send_message_stream" && r.aligned);

const sidecar = new Map();
if (fs.existsSync(sidecarF)) for (const r of load(sidecarF)) sidecar.set(r.step, r);

const liveBroke = (c) =>
  !!c.userJudge && (c.userJudge.broken === true || (typeof c.userJudge.score === "number" && c.userJudge.score >= 6));

// Honest broke: prefer the length-blind re-judge; fall back to the live judge.
const honestBroke = (c) => {
  const rj = sidecar.get(c.step);
  return rj ? rj.broken === true : liveBroke(c);
};

const qof = (c) => {
  const s = String(c.args ?? "");
  try {
    const o = JSON.parse(s);
    if (o && o.message) return o.message;
  } catch {
    /* truncated */
  }
  const m = s.match(/"message":"(.*?)","conversationId/s) ?? s.match(/"message":"(.*)$/s);
  return m ? m[1].replace(/\\"/g, '"').replace(/\\\\/g, "\\") : "";
};
const isLong = (q) => /1500 words|exhaustive, comprehensive/i.test(q ?? "");

const n = chats.length;
const coveredBySidecar = chats.filter((c) => sidecar.has(c.step)).length;
const liveBrokeN = chats.filter(liveBroke).length;
const honestBrokeN = chats.filter(honestBroke).length;

// Category breakdown over the re-judged turns (the real-failure taxonomy).
const cats = {};
for (const c of chats) {
  const rj = sidecar.get(c.step);
  if (!rj) continue;
  cats[rj.category] = (cats[rj.category] ?? 0) + 1;
}

// Turns the two judges DISAGREE on — the contamination the re-judge removed
// (live broke, honest good) and the ones it ADDED (live good, honest broke —
// padded fabrications the length-judge waved through).
const removed = chats.filter((c) => liveBroke(c) && !honestBroke(c));
const added = chats.filter((c) => !liveBroke(c) && honestBroke(c));

const longChats = chats.filter((c) => isLong(qof(c)));
const longLiveBroke = longChats.filter(liveBroke).length;
const longHonestBroke = longChats.filter(honestBroke).length;

const pct = (x, d = n) => (d ? `${((100 * x) / d).toFixed(1)}%` : "—");

console.log(`\n════════ HONEST SCORECARD — ${label} ════════`);
console.log(`scored chats: ${n}   re-judged: ${coveredBySidecar}/${n}`);
console.log(`\n── composite POSITIVE experience ──`);
console.log(`  LIVE  (length-penalizing judge): ${pct(n - liveBrokeN)}   (broke ${liveBrokeN})`);
console.log(`  HONEST (length-blind re-judge):  ${pct(n - honestBrokeN)}   (broke ${honestBrokeN})`);
console.log(`\n── long-prompt ("1500 words") broke ──`);
console.log(`  LIVE:   ${longLiveBroke}/${longChats.length}   HONEST: ${longHonestBroke}/${longChats.length}`);
console.log(`\n── re-judge failure categories ──`);
for (const [k, v] of Object.entries(cats).sort((a, b) => b[1] - a[1])) console.log(`  ${k.padEnd(20)} ${v}`);
console.log(`\n── judge disagreement ──`);
console.log(`  contamination REMOVED (live-broke → honest-good): ${removed.length}`);
for (const c of removed) console.log(`     len=${String(c.answerLen).padStart(5)} ${qof(c).replace(/ Answer in exhaustive.*/, "").slice(0, 60)}`);
console.log(`  real bugs ADDED (live-good → honest-broke): ${added.length}`);
for (const c of added) {
  const rj = sidecar.get(c.step);
  console.log(`     len=${String(c.answerLen).padStart(5)} [${rj?.category}] ${qof(c).replace(/ Answer in exhaustive.*/, "").slice(0, 55)}`);
}
console.log("");
