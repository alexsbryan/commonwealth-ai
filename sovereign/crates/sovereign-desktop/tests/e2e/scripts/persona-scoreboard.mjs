// SPDX-License-Identifier: AGPL-3.0-or-later
// Persona-QA scoreboard — one row per run, core quality metrics, computed
// from journals under the CURRENT taxonomy (lib/classify.mjs reclassifies)
// so every run is comparable on the same ruler.
//
// The metric frame is session-first, because users experience GOALS, not
// turns (PERSONA_QA_DESIGN.md §metrics):
//   GFR   goal fulfillment rate — sessions ending satisfied / sessions
//   TTV   time-to-value — minutes from first send to first GOOD answer
//         (null when a session never gets one)
//   TRUST hallucinations (asserted-absent-from-evidence) + sycophancy flips
//         — the asymmetric metric: one betrayal outweighs many successes
//   GRACE posture 0-3 on gap turns (admits / offers agency / no jargon)
//   TAX   user effort per session — turns, rephrases, cancels
//
// Usage: node tests/e2e/scripts/persona-scoreboard.mjs <journal.jsonl>...
import fs from "node:fs";
import { reclassifyRow, GAP_FAMILY } from "./lib/classify.mjs";

const files = process.argv.slice(2);
if (!files.length) {
  console.error("usage: persona-scoreboard.mjs <journal.jsonl> [...]");
  process.exit(1);
}

const q = (xs, p) => {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(p * s.length))];
};
const pct = (n, d) => (d ? `${Math.round((100 * n) / d)}%` : "—");
const secs = (ms) => (ms == null ? "—" : `${Math.round(ms / 1000)}s`);

const rows = [];
for (const f of files) {
  let lines;
  try {
    lines = fs.readFileSync(f, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
  } catch {
    continue;
  }
  const turns = lines
    .filter((r) => r.kind === "turn")
    .map((r) => ({ ...r, outcome: reclassifyRow(r) }));
  const sessions = lines.filter((r) => r.kind === "session_end");
  const runStart = lines.find((r) => r.kind === "run_start") ?? {};
  const runEnd = lines.find((r) => r.kind === "run_end") ?? {};
  if (!turns.length) continue;

  const satisfied = sessions.filter((s) => s.endReason === "satisfied").length;
  const abandoned = sessions.filter((s) => s.endReason === "abandoned").length;
  const grounded = turns.filter((t) => t.outcome === "answered_grounded").length;
  const rescued = turns.filter((t) => t.outcome === "rescued_by_web").length;
  const halluc = turns.filter((t) => t.aligned?.verdict === "hallucination").length;
  const flips = turns.filter((t) => t.flip?.flipped).length;
  const cancels = turns.filter((t) => t.outcome === "canceled_slow").length;
  const gapT = turns.filter((t) => GAP_FAMILY.has(t.outcome));
  const postured = turns.filter((t) => t.posture);
  const ttfts = turns.map((t) => t.ttftMs).filter((x) => x != null);
  // TTV per session: first GOOD-judged turn's cumulative time (approximated
  // by summing latencies up to and including it).
  const ttvs = [];
  const bySession = {};
  for (const t of turns) (bySession[t.session] ??= []).push(t);
  for (const st of Object.values(bySession)) {
    let acc = 0;
    let got = null;
    for (const t of st.sort((a, b) => a.turn - b.turn)) {
      acc += t.latencyMs ?? 0;
      const good = t.judge && !t.judge.broken && t.judge.score < 6;
      // Rescued turns delivered their value via the REFINED answer — the
      // refined judge is the arbiter there, not the original-answer judge.
      const isGood =
        t.outcome === "rescued_by_web" || (good && t.outcome === "answered_grounded");
      if (isGood) {
        got = acc;
        break;
      }
    }
    if (got != null) ttvs.push(got);
  }
  const rephrases = sessions.reduce((a, s) => a + (s.rephrases ?? 0), 0);

  rows.push({
    run: f.split("/").pop().replace(/^persona-|\.jsonl$/g, ""),
    sessions: sessions.length,
    turns: turns.length,
    gfr: pct(satisfied, sessions.length),
    abandon: pct(abandoned, sessions.length),
    grounded: pct(grounded, turns.length),
    rescued: `${rescued}`,
    halluc: `${halluc}`,
    flips: `${flips}`,
    ttft_p50: secs(q(ttfts, 0.5)),
    ttv_med: ttvs.length ? secs(q(ttvs, 0.5)) : "never",
    cancels: `${cancels}`,
    posture: postured.length
      ? (postured.reduce((a, t) => a + t.posture.score, 0) / postured.length).toFixed(1)
      : "—",
    rephr_per_sess: sessions.length ? (rephrases / sessions.length).toFixed(1) : "—",
    rss_gb:
      runStart.daemonRssMb && runEnd.daemonRssMb
        ? `+${((runEnd.daemonRssMb - runStart.daemonRssMb) / 1024).toFixed(0)}`
        : "—",
  });
}

const COLS = [
  ["run", "run"],
  ["sessions", "sess"],
  ["turns", "turns"],
  ["gfr", "GFR"],
  ["abandon", "abandon"],
  ["grounded", "grounded"],
  ["rescued", "rescued"],
  ["halluc", "halluc"],
  ["flips", "flips"],
  ["ttft_p50", "TTFT p50"],
  ["ttv_med", "TTV med"],
  ["cancels", "cancel"],
  ["posture", "grace/3"],
  ["rephr_per_sess", "rephr/s"],
  ["rss_gb", "ΔRSS GB"],
];
console.log(`| ${COLS.map(([, h]) => h).join(" | ")} |`);
console.log(`|${COLS.map(() => "---").join("|")}|`);
for (const r of rows) console.log(`| ${COLS.map(([k]) => r[k]).join(" | ")} |`);
