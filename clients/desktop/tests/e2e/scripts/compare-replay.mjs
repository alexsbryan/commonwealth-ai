// Paired A/B comparison for a replay run vs its baseline journal.
// Matches turns by normalized question text (the replay sends the exact same
// questions), so we compare NEW answer quality vs BASELINE on identical inputs
// — the question-mix variance that makes single-run composites noisy is
// cancelled. Reports the long-prompt ("1500 words") broke rate, the overall
// broke rate, and every question whose broke-status FLIPPED between runs.
//
// Usage: node compare-replay.mjs <baseline.jsonl> <new-replay.jsonl>
import fs from "node:fs";

const [baseF, newF] = process.argv.slice(2);
if (!baseF || !newF) {
  console.error("usage: node compare-replay.mjs <baseline.jsonl> <new.jsonl>");
  process.exit(2);
}

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

const LONG_SUFFIX = " Answer in exhaustive, comprehensive detail — at least 1500 words.";
const normSuffix = (q) => {
  const i = q.indexOf("Answer in exhaustive");
  return i >= 0 ? q.slice(0, i).trimEnd() + LONG_SUFFIX : q;
};
const qof = (c) => {
  const s = String(c.args ?? "");
  try {
    const o = JSON.parse(s);
    if (o && o.message) return normSuffix(o.message);
  } catch {
    /* truncated */
  }
  const m = s.match(/"message":"(.*?)","conversationId/s) ?? s.match(/"message":"(.*)$/s);
  if (m) return normSuffix(m[1].replace(/\\"/g, '"').replace(/\\\\/g, "\\"));
  return null;
};
const broke = (c) =>
  !!c.userJudge && (c.userJudge.broken === true || (typeof c.userJudge.score === "number" && c.userJudge.score >= 6));
const isLong = (q) => /1500 words|exhaustive, comprehensive/i.test(q ?? "");

// Index baseline by normalized question (last occurrence wins).
const baseChats = load(baseF).filter((r) => r.cmd === "send_message_stream" && r.aligned);
const baseByQ = new Map();
for (const c of baseChats) {
  const q = qof(c);
  if (q) baseByQ.set(q, c);
}

const newChats = load(newF).filter((r) => r.cmd === "send_message_stream" && r.aligned);

let matched = 0;
const flips = [];
const longNew = [];
const longBaseMatched = [];
let newBrokeN = 0;
let baseBrokeMatchedN = 0;

for (const nc of newChats) {
  const q = qof(nc);
  if (!q) continue;
  const bc = baseByQ.get(q);
  const nb = broke(nc);
  if (nb) newBrokeN += 1;
  if (isLong(q)) longNew.push({ q, broke: nb, verdict: nc.aligned.verdict, ansLen: nc.answerLen, why: nc.userJudge?.why });
  if (!bc) continue;
  matched += 1;
  const bb = broke(bc);
  if (bb) baseBrokeMatchedN += 1;
  if (isLong(q)) longBaseMatched.push({ q, baseBroke: bb, newBroke: nb });
  if (bb !== nb) flips.push({ q, baseBroke: bb, newBroke: nb, newVerdict: nc.aligned.verdict, newLen: nc.answerLen });
}

const rate = (n, d) => (d ? `${((100 * n) / d).toFixed(0)}% (${n}/${d})` : "—");

console.log("\n══════════ PAIRED REPLAY A/B ══════════");
console.log(`baseline: ${baseF.split("/").pop()}  (${baseChats.length} scored)`);
console.log(`new     : ${newF.split("/").pop()}  (${newChats.length} scored)`);
console.log(`matched by question: ${matched}`);

console.log("\n── LONG-PROMPT (\"1500 words\") broke rate — the target lever ──");
const longNewBroke = longNew.filter((x) => x.broke).length;
console.log(`  NEW run long prompts:      ${rate(longNewBroke, longNew.length)} broke`);
const lbm = longBaseMatched;
console.log(`  BASELINE (matched subset): ${rate(lbm.filter((x) => x.baseBroke).length, lbm.length)} broke`);

console.log("\n── overall broke rate (matched) ──");
console.log(`  NEW:      ${rate(newBrokeN, newChats.length)} (all new)`);
console.log(`  matched → base ${rate(baseBrokeMatchedN, matched)}  vs  new ${rate(newChats.filter((nc)=>{const q=qof(nc);return q&&baseByQ.has(q)&&broke(nc)}).length, matched)}`);

console.log(`\n── broke-status FLIPS (${flips.length}) ──`);
for (const f of flips) {
  const dir = f.baseBroke && !f.newBroke ? "FIXED  ✓" : "REGRESSED ✗";
  console.log(`  [${dir}] len=${f.newLen} verdict=${f.newVerdict}  ${f.q.slice(0, 75)}`);
}

console.log("\n── every long prompt in NEW run ──");
for (const x of longNew) {
  console.log(`  ${x.broke ? "BROKE" : "ok   "} len=${String(x.ansLen).padStart(5)} ${x.verdict.padEnd(18)} ${x.q.slice(0, 60)}`);
  if (x.broke && x.why) console.log(`        why: ${x.why.slice(0, 110)}`);
}
console.log("");
