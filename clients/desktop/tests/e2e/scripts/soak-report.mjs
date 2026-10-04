// SPDX-License-Identifier: AGPL-3.0-or-later
// Morning summary for a soak run: findings grouped by kind (with repro
// pointers), command-coverage burn-down vs the generate_handler!
// manifest, and turn-latency percentiles. Each confirmed finding's
// next step is a deterministic regression spec (the ratchet).
//
// Usage: node tests/e2e/scripts/soak-report.mjs
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { extractManifest } from "./command-manifest.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const ARTIFACTS = path.join(CRATE_ROOT, "test-artifacts");
const FINDINGS = path.join(ARTIFACTS, "soak-findings.jsonl");
const LATENCY = path.join(ARTIFACTS, "soak-latency.jsonl");
const LEDGERS = ["ledger-soak.jsonl", "ledger-real.jsonl"].map((f) =>
  path.join(ARTIFACTS, f),
);

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

const findings = readJsonl(FINDINGS);
const sessions = findings.filter((f) => f.kind === "soak_start");
const lastStart = sessions.at(-1);
const sessionRows = lastStart ? findings.filter((f) => f.seed === lastStart.seed) : findings;
const violations = sessionRows.filter(
  (f) => f.kind !== "soak_start" && f.kind !== "soak_end",
);

console.log(`\n══ soak report ══`);
if (lastStart) {
  const end = sessionRows.find((f) => f.kind === "soak_end");
  console.log(
    `session seed=${lastStart.seed} planned=${lastStart.minutes}min ` +
      (end ? `completed: ${end.actions} actions / ${end.ticks} ticks` : "(no soak_end — crashed or still running)"),
  );
}

console.log(`\nfindings: ${violations.length}`);
const byKind = new Map();
for (const v of violations) {
  byKind.set(v.kind, (byKind.get(v.kind) ?? []).concat(v));
}
for (const [kind, rows] of [...byKind.entries()].sort((a, b) => b[1].length - a[1].length)) {
  console.log(`  ${kind.padEnd(24)} ×${rows.length}`);
  for (const r of rows.slice(0, 3)) {
    console.log(
      `    tick=${r.tick} ${r.persona}/${r.action} → ${JSON.stringify(r.detail).slice(0, 110)}`,
    );
    console.log(`      repro: node tests/e2e/scripts/soak.mjs --seed ${r.seed}  (stop after tick ${r.tick})`);
  }
  if (rows.length > 3) console.log(`    … ${rows.length - 3} more`);
}

// ── latency percentiles ──
const lat = readJsonl(LATENCY).filter((r) => !lastStart || r.seed === lastStart.seed);
if (lat.length) {
  const ms = lat.map((r) => r.ms).sort((a, b) => a - b);
  const q = (p) => ms[Math.min(ms.length - 1, Math.floor(p * ms.length))];
  console.log(
    `\nturn latency (${ms.length} turns): p50=${q(0.5)}ms p95=${q(0.95)}ms max=${ms[ms.length - 1]}ms`,
  );
}

// ── coverage burn-down ──
const { commands, total } = extractManifest();
const seen = new Map();
for (const ledger of LEDGERS) {
  for (const row of readJsonl(ledger)) {
    seen.set(row.cmd, (seen.get(row.cmd) ?? 0) + 1);
  }
}
const exercised = commands.filter((c) => seen.has(c.name));
const never = commands.filter((c) => !seen.has(c.name));
console.log(`\ncoverage: ${exercised.length}/${total} commands exercised (ledgers: soak + real)`);
const byModule = new Map();
for (const c of never) {
  byModule.set(c.module, (byModule.get(c.module) ?? []).concat(c.name));
}
console.log(`never exercised (${never.length}) — next burn-down targets:`);
for (const [m, names] of [...byModule.entries()].sort()) {
  console.log(`  ${m}: ${names.slice(0, 8).join(", ")}${names.length > 8 ? ` … +${names.length - 8}` : ""}`);
}
