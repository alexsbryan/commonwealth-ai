// SPDX-License-Identifier: AGPL-3.0-or-later
// Gap Atlas — the persona-QA study report (PERSONA_QA_DESIGN.md §9).
// Post-processes a persona journal into a markdown report: outcome × stratum ×
// persona matrix, posture distribution on gap turns, exemplar transcripts for
// the worst buckets, escape-hatch (web search) effectiveness, strand rate,
// TTFT distribution, abandonment.
//
// Usage: node tests/e2e/scripts/persona-gap-atlas.mjs <journal.jsonl> [out.md]
import fs from "node:fs";
import path from "node:path";
import { reclassifyRow, GAP_FAMILY as GAP_FAMILY_SHARED } from "./lib/classify.mjs";

const journalPath = process.argv[2];
if (!journalPath) {
  console.error("usage: persona-gap-atlas.mjs <persona-journal.jsonl> [out.md]");
  process.exit(1);
}
const outPath =
  process.argv[3] ?? path.join(path.dirname(journalPath), "persona-gap-atlas.md");

const rows = fs
  .readFileSync(journalPath, "utf8")
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

// Reclassify under the CURRENT taxonomy (lib/classify.mjs) so taxonomy fixes
// retroactively correct past runs. The journaled outcome is kept as
// outcome_at_run for drift visibility.
const turns = rows
  .filter((r) => r.kind === "turn")
  .map((r) => ({ ...r, outcome_at_run: r.outcome, outcome: reclassifyRow(r) }));
const reclassified = turns.filter((t) => t.outcome !== t.outcome_at_run).length;
const sessions = rows.filter((r) => r.kind === "session_end");
const runMeta = rows.find((r) => r.kind === "run_start") ?? {};

if (!turns.length) {
  console.error("no turn rows in journal");
  process.exit(1);
}

const OUTCOMES = [
  "answered_grounded",
  "answered_ungrounded",
  "gap_admitted_offered",
  "gap_admitted_no_offer",
  "silent_gap",
  "rescued_by_web",
  "search_futile",
  "search_blocked",
  "canceled_slow",
  "turn_error",
  "turn_timeout",
];
const GAP_FAMILY = GAP_FAMILY_SHARED;

const pct = (n, d) => (d ? `${Math.round((100 * n) / d)}%` : "—");
const count = (arr, f) => arr.filter(f).length;
const quantile = (xs, q) => {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(q * s.length))];
};

function matrix(rowsIn, rowKeyFn, rowKeys) {
  const lines = [];
  lines.push(`| | ${OUTCOMES.join(" | ")} | total |`);
  lines.push(`|---|${OUTCOMES.map(() => "---").join("|")}|---|`);
  for (const rk of rowKeys) {
    const sub = rowsIn.filter((t) => rowKeyFn(t) === rk);
    const cells = OUTCOMES.map((o) => {
      const n = count(sub, (t) => t.outcome === o);
      return n ? String(n) : "·";
    });
    lines.push(`| **${rk}** | ${cells.join(" | ")} | ${sub.length} |`);
  }
  return lines.join("\n");
}

function transcriptBlock(t) {
  const parts = [
    `- **s${t.session}t${t.turn} · ${t.persona} · ${t.stratum} · corpus=${t.goalCorpus ?? "-"}**`,
    `  - goal: ${t.goal}`,
    `  - Q: ${t.question}`,
    `  - A (${t.answerLen ?? 0} chars${t.retrieved != null ? `, ${t.retrieved} chunks` : ""}): ${String(
      t.answer ?? "(none)",
    ).slice(0, 700)}`,
  ];
  if (t.judge) parts.push(`  - user-judge: ${t.judge.score}/10 — ${t.judge.why}`);
  if (t.aligned) parts.push(`  - bench: ${t.aligned.verdict}${t.aligned.value ? ` ("${t.aligned.value}")` : ""}`);
  if (t.evidencePresence != null) parts.push(`  - answer present in retrieved evidence: ${t.evidencePresence}`);
  if (t.probe) parts.push(`  - goal-corpus probe: hits=${t.probe.hits}, answerable=${t.probe.answerable}`);
  if (t.card) parts.push(`  - gap card: "${t.card.gap}"`);
  if (t.search) parts.push(`  - search: ${JSON.stringify(t.search)}`);
  if (t.refined && t.refinedChanged)
    parts.push(`  - refined (${t.refined.length} chars): ${t.refined.slice(0, 400)}`);
  if (t.refined && !t.refinedChanged) parts.push(`  - refined: REVERTED (re-gate kept original)`);
  if (t.posture) parts.push(`  - posture ${t.posture.score}/3 (admits=${t.posture.admits} agency=${t.posture.agency} clean=${t.posture.clean}) — ${t.posture.why}`);
  return parts.join("\n");
}

const personas = [...new Set(turns.map((t) => t.persona))];
const strata = ["in_corpus", "adjacent", "out_of_corpus"];
const gapTurns = turns.filter((t) => GAP_FAMILY.has(t.outcome));
const postured = gapTurns.filter((t) => t.posture);
const ttfts = turns.map((t) => t.ttftMs).filter((x) => x != null);
const searched = turns.filter((t) => t.search?.clicked);
const cardsShown = turns.filter((t) => t.card);
const cardsIgnored = cardsShown.filter((t) => !t.search);
const flips = turns.filter((t) => t.flip);

const md = [];
md.push(`# Persona-QA Gap Atlas`);
md.push(``);
md.push(
  `Run: ${new Date(runMeta.ts ?? turns[0].ts).toISOString()} · ${sessions.length} sessions · ${
    turns.length
  } turns · personas: ${(runMeta.personas ?? personas).join(", ")} · corpora scope: ${
    runMeta.scoped ? "goal-corpus" : "app default"
  } · brain: ${runMeta.brain ?? "?"}`,
);
md.push(``);
md.push(`## Outcome overview`);
md.push(``);
if (reclassified)
  md.push(`_(${reclassified}/${turns.length} turns reclassified under the current taxonomy vs. what the run recorded)_`, ``);
for (const o of OUTCOMES) {
  const n = count(turns, (t) => t.outcome === o);
  if (n) md.push(`- **${o}**: ${n} (${pct(n, turns.length)})`);
}
md.push(``);
md.push(`## Outcome × stratum`);
md.push(``);
md.push(matrix(turns, (t) => t.stratum, strata));
md.push(``);
md.push(`## Outcome × persona`);
md.push(``);
md.push(matrix(turns, (t) => t.persona, personas));
md.push(``);

md.push(`## Gap posture (${postured.length} scored gap turns)`);
md.push(``);
if (postured.length) {
  const avg = (f) => (postured.reduce((s, t) => s + f(t.posture), 0) / postured.length).toFixed(2);
  md.push(`- mean posture: ${avg((p) => p.score)}/3 (admits ${avg((p) => p.admits)}, agency ${avg((p) => p.agency)}, clean ${avg((p) => p.clean)})`);
  const leaky = count(turns, (t) => t.leakageFlag);
  md.push(`- architecture-leakage flag (regex, all turns): ${leaky} (${pct(leaky, turns.length)})`);
  const worst = postured.filter((t) => t.posture.score <= 1).slice(0, 5);
  if (worst.length) {
    md.push(``);
    md.push(`### Worst-posture exemplars`);
    md.push(``);
    for (const t of worst) md.push(transcriptBlock(t), ``);
  }
} else md.push(`(none scored)`);
md.push(``);

md.push(`## The escape hatch (web search)`);
md.push(``);
const cardsUnseen = cardsShown.filter((t) => t.card?.sawCard === false);
md.push(`- cards fired: ${cardsShown.length}; not clicked: ${cardsIgnored.length} (strand rate ${pct(cardsIgnored.length, cardsShown.length)}); arrived after the user stopped waiting: ${cardsUnseen.length}`);
md.push(`- searches run: ${searched.length}`);
md.push(`- rescued_by_web: ${count(turns, (t) => t.outcome === "rescued_by_web")}`);
const reverted = count(searched, (t) => t.refined && !t.refinedChanged);
md.push(`- search_futile: ${count(turns, (t) => t.outcome === "search_futile")} (of which re-gate REVERTED the refinement: ${reverted})`);
md.push(`- search_blocked: ${count(turns, (t) => t.outcome === "search_blocked")}`);
md.push(``);

md.push(`## Gap-check component`);
md.push(``);
const ranOn = count(turns, (t) => t.gapCheckRan);
md.push(`- gap check observed running (chip): ${ranOn}/${turns.length} turns`);
const silentPreskip = count(turns, (t) => t.outcome === "silent_gap" && !t.gapCheckRan);
md.push(`- silent_gap WITHOUT a gap-check chip (pre-skip suspect cell): ${silentPreskip}`);
const missedRetrieval = count(
  turns,
  (t) => t.outcome === "silent_gap" && t.evidencePresence === true,
);
md.push(`- silent_gap where the answer WAS in retrieved evidence (extraction miss, not corpus gap): ${missedRetrieval}`);
md.push(``);

md.push(`## Latency from the user's seat`);
md.push(``);
md.push(`- TTFT p50: ${quantile(ttfts, 0.5)}ms · p95: ${quantile(ttfts, 0.95)}ms · max: ${quantile(ttfts, 1)}ms (${ttfts.length} streamed turns)`);
md.push(`- canceled_slow: ${count(turns, (t) => t.outcome === "canceled_slow")}`);
md.push(``);

if (flips.length) {
  md.push(`## Skeptic pressure (sycophancy)`);
  md.push(``);
  const flipped = flips.filter((t) => t.flip.flipped);
  md.push(`- challenges judged: ${flips.length}; substance flipped: ${flipped.length} (${pct(flipped.length, flips.length)})`);
  for (const t of flipped.slice(0, 3)) md.push(``, transcriptBlock(t));
  md.push(``);
}

md.push(`## Sessions`);
md.push(``);
const byEnd = {};
for (const s of sessions) byEnd[s.endReason] = (byEnd[s.endReason] ?? 0) + 1;
md.push(`- endings: ${Object.entries(byEnd).map(([k, n]) => `${k}=${n}`).join(", ")}`);
const frustrated = sessions.filter((s) => s.frustrationNote);
if (frustrated.length) {
  md.push(`- what abandoning users would tell a friend:`);
  for (const s of frustrated.slice(0, 8)) md.push(`  - (${s.persona}) "${s.frustrationNote}"`);
}
md.push(``);

md.push(`## Exemplars — the trust-breaking buckets, in full`);
md.push(``);
for (const bucket of ["silent_gap", "answered_ungrounded"]) {
  const ex = turns.filter((t) => t.outcome === bucket).slice(0, 6);
  md.push(`### ${bucket} (${count(turns, (t) => t.outcome === bucket)} total, showing ${ex.length})`);
  md.push(``);
  for (const t of ex) md.push(transcriptBlock(t), ``);
}

fs.writeFileSync(outPath, md.join("\n"));
console.log(`gap atlas → ${outPath}`);
// Console tl;dr
const tl = (o) => count(turns, (t) => t.outcome === o);
console.log(
  `turns=${turns.length} grounded=${tl("answered_grounded")} silent_gap=${tl("silent_gap")} ` +
    `ungrounded=${tl("answered_ungrounded")} rescued=${tl("rescued_by_web")} futile=${tl("search_futile")} ` +
    `posture_mean=${postured.length ? (postured.reduce((s, t) => s + t.posture.score, 0) / postured.length).toFixed(2) : "—"}/3`,
);
