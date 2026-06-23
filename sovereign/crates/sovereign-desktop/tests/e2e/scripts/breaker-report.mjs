// SPDX-License-Identifier: AGPL-3.0-or-later
// Breaker triage report — the QA-team bug list for an adversarial run.
// The breaker shares the soak's findings pipeline (soak-findings.jsonl);
// this report isolates the latest session and ranks findings by SEVERITY
// then user-impact TIER, worst-first, each with a repro seed. That
// ranking is the whole point: a Tier-1 crash is read before a Tier-3
// cosmetic, so triage spends attention where users feel it.
//
// Exit code is 1 if any crash / hang / data_corruption finding exists, so
// this can gate a nightly run; lower-severity findings are advisory.
//
// Usage: node tests/e2e/scripts/breaker-report.mjs
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const FINDINGS = path.join(CRATE_ROOT, "test-artifacts/soak-findings.jsonl");

// Worst-first. Mirrors SEVERITY_RANK in soak.mjs.
const SEVERITY_ORDER = [
  "crash",
  "hang",
  "data_corruption",
  "wrong_output",
  "degraded",
  "cosmetic",
];
const SEVERITY_RANK = Object.fromEntries(SEVERITY_ORDER.map((s, i) => [s, i]));
const GATING = new Set(["crash", "hang", "data_corruption"]);

function readJsonl(file) {
  if (!fs.existsSync(file)) return [];
  return fs
    .readFileSync(file, "utf8")
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
}

const rows = readJsonl(FINDINGS);
const starts = rows.filter((r) => r.kind === "soak_start");
const last = starts.at(-1);
const session = last ? rows.filter((r) => r.seed === last.seed) : rows;
const meta = new Set(["soak_start", "soak_end", "seed_failed"]);
const findings = session.filter((r) => !meta.has(r.kind));

console.log(`\n══ breaker triage ══`);
if (last) {
  const end = session.find((r) => r.kind === "soak_end");
  console.log(
    `session seed=${last.seed}` +
      (end
        ? ` — ${end.actions} actions / ${end.ticks} ticks`
        : " (no soak_end — run crashed or still in progress)"),
  );
}
console.log(`findings: ${findings.length}`);

if (findings.length === 0) {
  console.log("\n(clean — the breaker surfaced nothing this session)\n");
  process.exit(0);
}

// Worst-first: severity rank, then user-impact tier.
const sorted = [...findings].sort((a, b) => {
  const sa = SEVERITY_RANK[a.severity] ?? 99;
  const sb = SEVERITY_RANK[b.severity] ?? 99;
  if (sa !== sb) return sa - sb;
  return (a.tier ?? 9) - (b.tier ?? 9);
});

let currentSev = null;
for (const f of sorted) {
  if (f.severity !== currentSev) {
    currentSev = f.severity;
    const n = findings.filter((x) => x.severity === f.severity).length;
    console.log(`\n${String(f.severity ?? "?").toUpperCase()}  (${n})`);
  }
  console.log(
    `  T${f.tier} ${f.persona}/${f.action} [${f.kind}] — ${JSON.stringify(f.detail).slice(0, 120)}`,
  );
  console.log(
    `     repro: node tests/e2e/scripts/soak.mjs --breaker --spawn --seed ${f.seed}  (→ tick ${f.tick})`,
  );
}

console.log(`\n── by severity ──`);
for (const s of SEVERITY_ORDER) {
  const n = findings.filter((f) => f.severity === s).length;
  if (n) console.log(`  ${s.padEnd(16)} ${n}`);
}
console.log(`── by user-impact tier ──`);
for (const t of [1, 2, 3, 4, 5]) {
  const n = findings.filter((f) => f.tier === t).length;
  if (n) console.log(`  T${t}  ${n}`);
}

const severe = findings.filter((f) => GATING.has(f.severity)).length;
console.log(
  `\n${severe ? `GATING: ${severe} crash/hang/corruption finding(s)` : "no gating-severity findings"}`,
);
process.exitCode = severe > 0 ? 1 : 0;
