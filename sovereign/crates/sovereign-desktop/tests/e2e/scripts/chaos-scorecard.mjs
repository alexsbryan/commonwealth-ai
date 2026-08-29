// SPDX-License-Identifier: AGPL-3.0-or-later
// Chaos QA scorecard — the measure-iterate loop's read surface.
//
// Reads a chaos field journal (chaos-journal.jsonl) and prints ONE comparable
// scorecard per run: the bench-grounding verdict distribution, the DISENTANGLED
// hallucination breakdown (evidence-present real fabrication vs empty-evidence
// measurement artifact vs synthesis/theme), the responsiveness/latency axis,
// raw-error leaks, abrasive declines, true stalls, and a single composite
// POSITIVE-EXPERIENCE rate.
//
// POSITIVE = the user got a good answer: ANSWERED and the UX judge did not call
// it broken (broken || score>=6). Two things are deliberately NOT in this binary:
//   - LATENCY. A slow-but-correct answer is a latency problem, reported on its
//     own axis — not a non-positive experience. Folding a 60s threshold in here
//     was the "hang" mislabel: every "hang" last run returned an answer. A
//     genuine STALL (no answer at all, not a user cancel) IS negative and sits
//     in the denominator (nAttempts).
//   - The raw `hallucination` verdict. Fabrication is confirmedFabrication —
//     where BOTH oracles agree (bench verdict + UX judge), which is ⊆ broke. The
//     5-of-6 faithful syntheses the bench flagged but the UX judge approved are
//     NOT counted against the experience; the confirmed-fab count is the headline.
// The composite is still not gameable by one lever: abstaining on everything
// would NOT raise positiveRate past the honest-decline ceiling and would SINK
// groundedRate (reported alongside with a DEGENERATE flag). The agent, the
// question stream, and the oracle are frozen; this only reads.
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

// ── chat attempts vs answered vs true stalls ───────────────────────
// `chats` is ANSWERED+scored turns. A chat attempt that returned NOTHING (the
// app hung, the invoke aborted) carries no `aligned` verdict, so it was silently
// dropped from the denominator — a "user got nothing" failure made invisible.
// Recover it here, while EXCLUDING the two non-failures that also yield a null
// answer: deliberate user cancels (the breaker spams cancel_stream) and harness
// arg-validation rejects (a bad conversationId before a conversation exists).
// What remains is a genuine STALL — the worst outcome, and it belongs in the
// composite denominator as a negative.
const CHAT_CMDS = new Set(["send_message_stream", "ask_document"]);
const chatAttempts = rows.filter((r) => CHAT_CMDS.has(r.cmd));
const cancelledNear = (r) => rows.some((x) => x.cmd === "cancel_stream" && x.step > r.step && x.step <= r.step + 2);
// A genuine STALL = the app accepted the request but the user never got an
// answer. Two shapes: the call aborted/timed out, or it returned ok but no
// answer ever streamed (and the user did not cancel). A fast, specific rejection
// (bad conversationId, "Document not found", an oversize-message notice) is the
// app correctly handling a bad request — NOT a stall, so it is excluded.
const isStall = (r) => {
  if (r.aligned && r.aligned.verdict) return false; // answered + scored
  if (cancelledNear(r)) return false; // deliberate user cancel
  if (/abort|timed?\s*out/i.test(String(r.error ?? ""))) return true; // aborted / timed out
  return r.ok === true && (r.answer == null || r.answer.length === 0); // accepted but silent
};
const trueStalls = chatAttempts.filter(isStall);
const nAttempts = n + trueStalls.length; // answered + genuine no-answer stalls
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
// A turn is "broken" if the UX judge flagged it OR scored it >=6 — matching the
// harness's own scoreSurprise threshold. Reading only `.broken` missed
// inconsistent judge outputs (broken:false, score:10) — a real bad answer
// mislabeled as fine (e.g. the run's step-99 fabrication). This makes
// confirmedFabrication STRICTER, not looser.
const broke = (c) =>
  !!c.userJudge &&
  (c.userJudge.broken === true || (typeof c.userJudge.score === "number" && c.userJudge.score >= 6));
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
// Slow COMPLETIONS: answers that arrived but took >=HANG_MS. A latency problem,
// NOT a hang — the user got a real (often correct) answer. Reported on the
// latency axis; they no longer sink the composite (that was the "14 hangs"
// mislabel — every one returned an answer). True stalls (no answer at all) are
// counted separately, above.
const slowCompletions = chats.filter((c) => typeof c.latencyMs === "number" && c.latencyMs >= HANG_MS).length;
const brokeCount = chats.filter(broke).length;
const appDown = rows.some((r) => r.kind === "app_down");
// Responsiveness percentiles over ALL answered turns (the latency axis the
// composite no longer folds in).
const pctl = (xs, p) => {
  const s = xs.filter((x) => typeof x === "number").sort((a, b) => a - b);
  if (!s.length) return null;
  return s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))];
};
const latsAll = chats.map((c) => c.latencyMs);
const latP90 = pctl(latsAll, 90);
const over30 = chats.filter((c) => c.latencyMs >= 30_000).length;
const over120 = chats.filter((c) => c.latencyMs >= 120_000).length;

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
// POSITIVE = the user got a good answer: ANSWERED and the UX judge did not call
// it broken. Two corrections from the prior definition, both about accuracy not
// inflation:
//   - Latency is NOT in this binary. A slow-but-correct answer is a latency
//     problem (reported on its own axis), not a non-positive experience —
//     folding a 60s threshold in here was the "14 hangs" mislabel (every one
//     returned an answer). A genuine STALL (no answer) IS negative: it sits in
//     the denominator (nAttempts) but never in `positive`.
//   - Fabrication is judged by confirmedFabrication (both oracles agree), which
//     is ⊆ broke, so `!broke(c)` already excludes it. The raw `hallucination`
//     verdict is NOT used here — 5 of 6 last run were faithful syntheses the UX
//     judge approved. The confirmed-fab count is still reported as the headline.
const positive = chats.filter((c) => !broke(c)).length;
const positiveRate = nAttempts ? positive / nAttempts : 0;
// COULD-NOT-JUDGE is its own verdict, not a decline and not a failure (ARCH
// 18.1). answered_novalue means assess_asserted_value extracted no checkable
// value, so the grounding question was never answered for that turn. Rating it
// against `n` silently substitutes "we did not measure" for "the app did badly":
// the 2026-08-28 soak reported grounded 8.3% / decline 91.7% off 24 chats of
// which 22 were unjudgeable, and 18 of those 22 had cited their evidence.
// Grounding rates are therefore computed over the JUDGEABLE subset and the
// coverage is printed next to them, so a thin run cannot masquerade as a bad one.
const couldNotJudge = answeredNoval;
const judgeable = n - couldNotJudge;
const judgeCoverage = n ? judgeable / n : 0;
const groundedRate = judgeable ? grounded / judgeable : null;
const declineRate = judgeable ? declines / judgeable : null;
// Degenerate-improvement guard: a high positiveRate built on a collapsed
// groundedRate + inflated declineRate is the app abstaining its way to a
// "good" score — the gaming the user warned against. Flag it, don't hide it.
// It can only fire on a run that actually MEASURED something: below half
// coverage the rates are too thin to carry a verdict, and the run reports
// LOW COVERAGE instead — a could-not-judge about the scorecard itself.
const MIN_JUDGE_COVERAGE = 0.5;
const lowCoverage = n > 0 && judgeCoverage < MIN_JUDGE_COVERAGE;
const degenerate =
  !lowCoverage && declineRate !== null && groundedRate !== null && declineRate > 0.5 && groundedRate < 0.25;

const card = {
  label: LABEL,
  journal: JOURNAL,
  scoredChats: n,
  judgeable,
  couldNotJudge,
  judgeCoverage,
  lowCoverage,
  chatAttempts: nAttempts,
  trueStalls: trueStalls.length,
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
  latencyMs: { all: latAll, grounded: latGrounded, decline: latDecline, hallucination: latHall, p90: latP90, over30s: over30, over120s: over120 },
  ux: { rawErrors, abrasiveDeclines, slowCompletions, trueStalls: trueStalls.length, brokeTurns: brokeCount },
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

console.log(
  `    ${"— of which UNJUDGEABLE".padEnd(18)} ${String(couldNotJudge).padStart(4)}   ${pct(couldNotJudge)}   ← answered_novalue: no checkable value extracted`,
);
console.log(
  `    judgeable for grounding: ${judgeable}/${n} (${(100 * judgeCoverage).toFixed(1)}% coverage)${lowCoverage ? "   ⚠ TOO THIN FOR A GROUNDING VERDICT" : ""}`,
);

console.log(`\n  ── hallucination disentangled (total ${hallucination}) ──`);
console.log(`    confirmed fabrication (real bug)      ${String(confirmedFab).padStart(3)}   ← the app-quality target`);
console.log(`    theme/synthesis strictness (frozen)   ${String(themeStrict).padStart(3)}   ← oracle strict on summaries; userJudge OK`);
console.log(`    measurement artifact (empty evidence) ${String(measurementArtifact).padStart(3)}   ← harness/retrieval, not a fabrication`);
console.log(`      buckets: empty_retrieval=${hbuckets.empty_retrieval.length} resolve_failed=${hbuckets.resolve_failed.length} had_evidence=${hbuckets.had_evidence.length} no_evidence_field=${hbuckets.no_evidence_field.length}`);

console.log(`\n  ── responsiveness (latency axis — its OWN signal, not in the composite) ──`);
console.log(`    median ${latAll ?? "—"}ms   p90 ${latP90 ?? "—"}ms   |   grounded ${latGrounded ?? "—"}   decline ${latDecline ?? "—"}   hallucination ${latHall ?? "—"}`);
console.log(`    slow answers: ${over30}/${n} >=30s   ${slowCompletions}/${n} >=60s   ${over120}/${n} >=120s   (all returned an answer)`);
if (latDecline && latGrounded && latDecline > latGrounded * 1.3)
  console.log(`    ⚠ declines are ${(latDecline / latGrounded).toFixed(1)}× slower than grounded answers (slow-abstention)`);

console.log(`\n  ── UX / robustness ──`);
console.log(`    raw errors shown: ${rawErrors}   abrasive declines: ${abrasiveDeclines}   broke turns: ${brokeCount}`);
console.log(`    true stalls (no answer, not cancelled): ${trueStalls.length}   slow completions (>=${HANG_MS / 1000}s, answered): ${slowCompletions}`);

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
console.log(`    POSITIVE EXPERIENCE   ${(100 * positiveRate).toFixed(1)}%   (answered ∧ not-broken; of ${nAttempts} attempts incl. ${trueStalls.length} stall)`);
const rate = (r) => (r === null ? "n/j" : `${(100 * r).toFixed(1)}%`);
console.log(`    grounded rate         ${rate(groundedRate)}   (of ${judgeable} judgeable — must hold/rise, not collapse into declines)`);
console.log(`    hallucination rate    ${(100 * (n ? hallucination / n : 0)).toFixed(1)}%   confirmed-only ${(100 * (n ? confirmedFab / n : 0)).toFixed(1)}%`);
console.log(`    decline rate          ${rate(declineRate)}   (of ${judgeable} judgeable)`);
if (lowCoverage)
  console.log(
    `    ⚠ LOW COVERAGE: only ${judgeable}/${n} turns yielded a checkable value, so this run carries NO grounding verdict.\n      Fix the question source before reading the rates above (see chaos.mjs questionSource counts).`,
  );
if (degenerate) console.log(`    ⚠ DEGENERATE: high positive built on collapsed grounding + inflated declines — this is GAMING, not improvement.`);
console.log(`${bar}\n`);
