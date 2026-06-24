// SPDX-License-Identifier: AGPL-3.0-or-later
// Chaos QA scorecard — the measure-iterate loop's read surface.
//
// Reads a chaos field journal (chaos-journal.jsonl) and prints ONE comparable
// scorecard per run: the bench-grounding verdict distribution, the DISENTANGLED
// hallucination breakdown (evidence-present real fabrication vs empty-evidence
// measurement artifact vs synthesis/theme), latency by verdict (the slow-
// abstention signal), raw-error leaks, abrasive declines, hangs, and a single
// composite POSITIVE-EXPERIENCE rate.
//
// The composite is deliberately NOT gameable by one lever: a turn is POSITIVE
// only if the bench did not flag a fabrication AND the UX judge did not call it
// broken AND it did not hang. Driving the app to abstain on everything would
// cut hallucinations but would NOT raise positiveRate beyond the honest-decline
// ceiling and would SINK groundedRate — which this card reports alongside, with
// a DEGENERATE flag, so an all-declines "win" is visible as the gaming it is.
// The agent, the question stream, and the oracle are frozen; this only reads.
//
// Usage: node chaos-scorecard.mjs [journal.jsonl] [--label "iter-1"] [--json]
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ARTIFACTS = path.resolve(__dirname, "../../../test-artifacts");
const argv = process.argv.slice(2);
const flag = (n, fb) => {
  const i = argv.indexOf(`--${n}`);
  return i >= 0 ? argv[i + 1] : fb;
};
const JOURNAL = argv.find((a) => !a.startsWith("--") && a.endsWith(".jsonl")) ?? path.join(ARTIFACTS, "chaos-journal.jsonl");
const LABEL = flag("label", path.basename(JOURNAL));
const AS_JSON = argv.includes("--json");
const HANG_MS = 60_000;

if (!fs.existsSync(JOURNAL)) {
  console.error(`scorecard: no journal at ${JOURNAL}`);
  process.exit(2);
}

const rows = fs
  .readFileSync(JOURNAL, "utf8")
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

// A scored chat turn carries an `aligned` verdict.
const chats = rows.filter((r) => r.aligned && r.aligned.verdict);
const n = chats.length;
const median = (xs) => {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? s[m] : Math.round((s[m - 1] + s[m]) / 2);
};
const pct = (x) => (n ? `${((100 * x) / n).toFixed(1)}%` : "—");

// ── verdict distribution ──────────────────────────────────────────
const verdicts = {};
for (const c of chats) verdicts[c.aligned.verdict] = (verdicts[c.aligned.verdict] ?? 0) + 1;
const get = (v) => verdicts[v] ?? 0;
const grounded = get("grounded");
const hallucination = get("hallucination");
const honestAbst = get("honest_abstention");
const caveatedOod = get("caveated_ood");
const answeredNoval = get("answered_novalue");
const declines = honestAbst + caveatedOod;

// ── hallucination disentangling (the trustworthy-signal step) ──────
// evidence: {retrieved, resolved, chars}. broke: userJudge said broken.
// Buckets:
//   empty_retrieval   retrieved==0 — app answered with NO evidence surfaced
//                     (parametric/lucky, or a retrieval miss). A fabrication
//                     here is the parametric-path gap; a correct answer here
//                     is "ungrounded-but-right" (grounding discipline flags it).
//   resolve_failed    retrieved>0 but resolved==0 — the HARNESS could not
//                     resolve the chunks to text → oracle scored against empty
//                     evidence. MEASUREMENT ARTIFACT, not an app fabrication.
//   had_evidence      resolved>0 — the oracle saw real evidence and still found
//                     the asserted value absent. Either a gate-miss fabrication
//                     (userJudge broke=true) or strict-oracle-on-synthesis
//                     (userJudge broke=false, e.g. a themed summary).
const broke = (c) => c.userJudge && c.userJudge.broken === true;
const hbuckets = { empty_retrieval: [], resolve_failed: [], had_evidence: [], no_evidence_field: [] };
const hallRows = chats.filter((c) => c.aligned.verdict === "hallucination");
for (const c of hallRows) {
  const e = c.evidence;
  if (!e) hbuckets.no_evidence_field.push(c);
  else if ((e.retrieved ?? 0) === 0) hbuckets.empty_retrieval.push(c);
  else if ((e.resolved ?? 0) === 0) hbuckets.resolve_failed.push(c);
  else hbuckets.had_evidence.push(c);
}
// Primary split is the userJudge `broke` cross-check (present on every journal,
// instrumented or not): broke=true is a real bad answer the UX judge confirmed.
// Among broke=false ("userJudge OK"), refine WITH the evidence field when it
// exists: the oracle saw real evidence (resolved>0) → theme/synthesis
// strictness; the oracle saw NO evidence (resolved==0, or no field) → the
// verdict is unreliable (measurement/retrieval), not a confirmed fabrication.
let confirmedFab = 0;
let themeStrict = 0;
let measurementArtifact = 0;
for (const c of hallRows) {
  if (broke(c)) {
    confirmedFab += 1;
    continue;
  }
  const resolved = c.evidence ? (c.evidence.resolved ?? 0) : null;
  if (resolved && resolved > 0) themeStrict += 1;
  else measurementArtifact += 1; // resolved==0, or pre-instrumentation journal
}

// ── latency by verdict (the slow-abstention signal) ────────────────
const lat = (pred) => median(chats.filter(pred).map((c) => c.latencyMs).filter((x) => typeof x === "number"));
const latGrounded = lat((c) => c.aligned.verdict === "grounded");
const latDecline = lat((c) => c.aligned.verdict === "honest_abstention" || c.aligned.verdict === "caveated_ood");
const latHall = lat((c) => c.aligned.verdict === "hallucination");
const latAll = median(chats.map((c) => c.latencyMs).filter((x) => typeof x === "number"));

// ── UX / robustness signals ────────────────────────────────────────
const rawErrors = chats.filter((c) => (c.signals ?? []).some((s) => /raw error shown to user/i.test(s))).length;
const abrasiveDeclines = chats.filter(
  (c) => (c.aligned.verdict === "honest_abstention" || c.aligned.verdict === "caveated_ood") && broke(c),
).length;
const hangs = chats.filter((c) => typeof c.latencyMs === "number" && c.latencyMs >= HANG_MS).length;
const brokeCount = chats.filter(broke).length;
const appDown = rows.some((r) => r.kind === "app_down");

// ── subset-conditional rates (the mix-robust per-iteration signal) ──
// The whole-sample composite is noise-dominated at feasible run lengths AND
// confounded by question-MIX variance (one run draws more empty-retrieval
// questions than another). Conditioning on evidence-present vs evidence-empty
// separates "empty-retrieval handling quality" from "grounded-answer quality"
// — each is mix-independent, so a fix's effect on its target subset is legible
// at ~10 examples without waiting for whole-sample significance. Needs the
// evidence field (instrumented runs only).
const withEv = chats.filter((c) => c.evidence);
const emptyChats = withEv.filter((c) => (c.evidence.retrieved ?? 0) === 0);
const groundedPath = withEv.filter((c) => (c.evidence.retrieved ?? 0) > 0);
const brokeRate = (xs) => (xs.length ? xs.filter(broke).length / xs.length : null);
// Cut-off census: long real answers (retrieved>0, >200 chars) ending mid-
// sentence (no terminal punctuation in the last 80 chars) — an early stream-end
// on the GOOD answers, invisible to finish_reason=Length.
const isTerminal = (t) => /[.!?"”)\]}:*][ \t*_"]*$/.test(t);
const longReal = groundedPath.filter((c) => (c.answerLen ?? 0) > 200 && c.answerTail);
const cutoff = { n: longReal.length, midSentence: longReal.filter((c) => !isTerminal(c.answerTail)).length };

// ── composite positive-experience rate ─────────────────────────────
// POSITIVE = not a fabrication AND UX-judge not broken AND not a hang.
const positive = chats.filter(
  (c) => c.aligned.verdict !== "hallucination" && !broke(c) && (typeof c.latencyMs !== "number" || c.latencyMs < HANG_MS),
).length;
const positiveRate = n ? positive / n : 0;
const groundedRate = n ? grounded / n : 0;
const declineRate = n ? declines / n : 0;
// Degenerate-improvement guard: a high positiveRate built on a collapsed
// groundedRate + inflated declineRate is the app abstaining its way to a
// "good" score — the gaming the user warned against. Flag it, don't hide it.
const degenerate = declineRate > 0.5 && groundedRate < 0.25;

const card = {
  label: LABEL,
  journal: JOURNAL,
  scoredChats: n,
  totalRows: rows.length,
  appDown,
  verdicts,
  rates: {
    positive: positiveRate,
    grounded: groundedRate,
    hallucination: n ? hallucination / n : 0,
    decline: declineRate,
  },
  hallucinationBreakdown: {
    total: hallucination,
    confirmedFabrication: confirmedFab,
    themeOrSynthesisStrict: themeStrict,
    measurementArtifact,
    buckets: Object.fromEntries(Object.entries(hbuckets).map(([k, v]) => [k, v.length])),
  },
  latencyMs: { all: latAll, grounded: latGrounded, decline: latDecline, hallucination: latHall },
  ux: { rawErrors, abrasiveDeclines, hangs, brokeTurns: brokeCount },
  conditional: {
    haveEvidenceField: withEv.length,
    emptyRetrieval: { n: emptyChats.length, brokeRate: brokeRate(emptyChats) },
    groundedPath: { n: groundedPath.length, brokeRate: brokeRate(groundedPath) },
    cutoffLongReal: cutoff,
  },
  degenerate,
};

if (AS_JSON) {
  console.log(JSON.stringify(card, null, 2));
  process.exit(0);
}

// ── human scorecard ────────────────────────────────────────────────
const bar = "═".repeat(64);
console.log(`\n${bar}\n  CHAOS QA SCORECARD — ${LABEL}\n${bar}`);
console.log(`  scored chats: ${n}   (journal rows: ${rows.length})${appDown ? "   ⚠ APP WENT DOWN" : ""}`);
console.log(`\n  ── verdict distribution ──`);
for (const [v, c] of Object.entries(verdicts).sort((a, b) => b[1] - a[1]))
  console.log(`    ${v.padEnd(18)} ${String(c).padStart(4)}   ${pct(c)}`);

console.log(`\n  ── hallucination disentangled (total ${hallucination}) ──`);
console.log(`    confirmed fabrication (real bug)      ${String(confirmedFab).padStart(3)}   ← the app-quality target`);
console.log(`    theme/synthesis strictness (frozen)   ${String(themeStrict).padStart(3)}   ← oracle strict on summaries; userJudge OK`);
console.log(`    measurement artifact (empty evidence) ${String(measurementArtifact).padStart(3)}   ← harness/retrieval, not a fabrication`);
console.log(`      buckets: empty_retrieval=${hbuckets.empty_retrieval.length} resolve_failed=${hbuckets.resolve_failed.length} had_evidence=${hbuckets.had_evidence.length} no_evidence_field=${hbuckets.no_evidence_field.length}`);

console.log(`\n  ── latency (median ms) — slow-abstention signal ──`);
console.log(`    grounded ${latGrounded ?? "—"}   decline ${latDecline ?? "—"}   hallucination ${latHall ?? "—"}   all ${latAll ?? "—"}`);
if (latDecline && latGrounded && latDecline > latGrounded * 1.3)
  console.log(`    ⚠ declines are ${(latDecline / latGrounded).toFixed(1)}× slower than grounded answers (slow-abstention)`);

console.log(`\n  ── UX / robustness ──`);
console.log(`    raw errors shown: ${rawErrors}   abrasive declines: ${abrasiveDeclines}   hangs(>${HANG_MS / 1000}s): ${hangs}   broke turns: ${brokeCount}`);

const fmtRate = (r) => (r == null ? "—" : `${(100 * r).toFixed(0)}%`);
console.log(`\n  ── subset-conditional (mix-robust — the per-iteration lever) ──`);
if (withEv.length) {
  console.log(`    empty-retrieval:  ${String(emptyChats.length).padStart(3)} chats, broke ${fmtRate(brokeRate(emptyChats))}   ← decline-quality lever`);
  console.log(`    grounded-path:    ${String(groundedPath.length).padStart(3)} chats, broke ${fmtRate(brokeRate(groundedPath))}   ← answer-quality lever`);
  console.log(`    cut-off (long grounded answers ending mid-sentence): ${cutoff.midSentence}/${cutoff.n}`);
} else {
  console.log(`    (no evidence field — pre-instrumentation journal)`);
}

console.log(`\n  ── composite (anti-gaming: all must move TOGETHER) ──`);
console.log(`    POSITIVE EXPERIENCE   ${(100 * positiveRate).toFixed(1)}%   (not-fabrication ∧ not-broken ∧ not-hang)`);
console.log(`    grounded rate         ${(100 * groundedRate).toFixed(1)}%   (must hold/rise — not collapse into declines)`);
console.log(`    hallucination rate    ${(100 * (n ? hallucination / n : 0)).toFixed(1)}%   confirmed-only ${(100 * (n ? confirmedFab / n : 0)).toFixed(1)}%`);
console.log(`    decline rate          ${(100 * declineRate).toFixed(1)}%`);
if (degenerate) console.log(`    ⚠ DEGENERATE: high positive built on collapsed grounding + inflated declines — this is GAMING, not improvement.`);
console.log(`${bar}\n`);
