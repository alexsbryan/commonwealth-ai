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
import { computeMetrics } from "./lib/metrics.mjs";

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
  const runStart = lines.find((r) => r.kind === "run_start") ?? {};
  const runEnd = lines.find((r) => r.kind === "run_end") ?? {};
  const m = computeMetrics(lines);
  if (!m.nTurns) continue;

  rows.push({
    run: f.split("/").pop().replace(/^persona-|\.jsonl$/g, ""),
    sessions: m.nSessions,
    turns: m.nTurns,
    gfr: m.gfr == null ? "—" : `${Math.round(m.gfr * 100)}%`,
    abandon: m.abandon_rate == null ? "—" : `${Math.round(m.abandon_rate * 100)}%`,
    grounded: m.grounded_rate == null ? "—" : `${Math.round(m.grounded_rate * 100)}%`,
    rescued: `${m.rescued}`,
    halluc: `${m.hallucinations}`,
    flips: `${m.flips}`,
    ttft_p50: m.ttft_p50_s == null ? "—" : `${Math.round(m.ttft_p50_s)}s`,
    ttv_med: m.ttv_median_s == null ? "never" : `${Math.round(m.ttv_median_s)}s`,
    cancels: `${m.cancels}`,
    posture: m.grace_mean == null ? "—" : m.grace_mean.toFixed(1),
    rephr_per_sess: m.rephrases_per_session == null ? "—" : m.rephrases_per_session.toFixed(1),
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
