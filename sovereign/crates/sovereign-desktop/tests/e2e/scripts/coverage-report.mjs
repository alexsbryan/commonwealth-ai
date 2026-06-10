// SPDX-License-Identifier: AGPL-3.0-or-later
// Joins the Tauri command manifest (parsed from generate_handler! by
// command-manifest.mjs) against one or more invoke ledgers (JSONL rows
// appended by the e2e harness) and reports surface coverage: which of
// the production commands automated testing actually exercised.
//
// Usage:
//   node tests/e2e/scripts/coverage-report.mjs [ledger.jsonl ...]
// Defaults to test-results/ledger-synthetic.jsonl. Pass multiple ledgers
// (e.g. synthetic + real-mode) to get one merged report with per-ledger
// columns. Writes test-results/coverage.json alongside the printout.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { extractManifest } from "./command-manifest.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const DEFAULT_LEDGER = path.join(
  CRATE_ROOT,
  "test-results/ledger-synthetic.jsonl",
);
const OUT_PATH = path.join(CRATE_ROOT, "test-results/coverage.json");

function loadLedger(file) {
  // counts: cmd → { calls, errors, specs:Set }
  const counts = new Map();
  if (!fs.existsSync(file)) {
    console.error(`coverage-report: ledger missing, treating as empty: ${file}`);
    return counts;
  }
  for (const line of fs.readFileSync(file, "utf8").split("\n")) {
    if (!line.trim()) continue;
    let row;
    try {
      row = JSON.parse(line);
    } catch {
      console.error(`coverage-report: skipping malformed row: ${line.slice(0, 120)}`);
      continue;
    }
    const entry = counts.get(row.cmd) ?? { calls: 0, errors: 0, specs: new Set() };
    entry.calls += 1;
    if (row.ok === false) entry.errors += 1;
    if (row.spec) entry.specs.add(row.spec);
    counts.set(row.cmd, entry);
  }
  return counts;
}

const ledgerFiles = process.argv.slice(2).length
  ? process.argv.slice(2).map((p) => path.resolve(p))
  : [DEFAULT_LEDGER];
const ledgers = ledgerFiles.map((file) => ({
  label: path.basename(file).replace(/^ledger-|\.jsonl$/g, ""),
  counts: loadLedger(file),
}));

const { commands, total } = extractManifest();

const rows = commands.map(({ name, module }) => {
  const per = {};
  let calls = 0;
  let errors = 0;
  for (const { label, counts } of ledgers) {
    const c = counts.get(name);
    per[label] = c ? c.calls : 0;
    calls += c?.calls ?? 0;
    errors += c?.errors ?? 0;
  }
  return { name, module, calls, errors, per };
});

// Ledger rows whose command isn't in the manifest mean either parse
// drift or a stale ledger — surface them, never silently drop.
const known = new Set(commands.map((c) => c.name));
const unknown = [
  ...new Set(
    ledgers.flatMap(({ counts }) => [...counts.keys()].filter((c) => !known.has(c))),
  ),
];

const exercised = rows.filter((r) => r.calls > 0);
const never = rows.filter((r) => r.calls === 0);

// ── Printout: per-module summary, then the never-exercised burn-down ──
const byModule = new Map();
for (const r of rows) {
  const m = byModule.get(r.module) ?? { total: 0, exercised: 0 };
  m.total += 1;
  if (r.calls > 0) m.exercised += 1;
  byModule.set(r.module, m);
}

const pct = (a, b) => (b === 0 ? "0%" : `${Math.round((100 * a) / b)}%`);
console.log(`\nCommand coverage — ${exercised.length}/${total} exercised (${pct(exercised.length, total)})`);
console.log(`Ledgers: ${ledgerFiles.map((f) => path.relative(CRATE_ROOT, f)).join(", ")}\n`);
console.log("By module:");
for (const [module, m] of [...byModule.entries()].sort()) {
  console.log(`  ${module.padEnd(28)} ${String(m.exercised).padStart(3)}/${String(m.total).padEnd(3)} ${pct(m.exercised, m.total)}`);
}

const erroring = exercised.filter((r) => r.errors > 0);
if (erroring.length) {
  console.log("\nExercised with errors (expected for negative-path specs — review):");
  for (const r of erroring) console.log(`  ${r.name} (${r.errors}/${r.calls} errored)`);
}
if (unknown.length) {
  console.log(`\nLedger commands NOT in manifest (drift or stale ledger): ${unknown.join(", ")}`);
}
console.log(`\nNever exercised (${never.length}):`);
for (const [module] of [...byModule.entries()].sort()) {
  const misses = never.filter((r) => r.module === module);
  if (misses.length) console.log(`  ${module}: ${misses.map((r) => r.name).join(", ")}`);
}

fs.mkdirSync(path.dirname(OUT_PATH), { recursive: true });
fs.writeFileSync(
  OUT_PATH,
  JSON.stringify(
    {
      generated_at: new Date().toISOString(),
      total,
      exercised: exercised.length,
      ledgers: ledgerFiles.map((f) => path.relative(CRATE_ROOT, f)),
      unknown_commands: unknown,
      commands: rows.map(({ name, module, calls, errors, per }) => ({
        name,
        module,
        calls,
        errors,
        ...Object.fromEntries(Object.entries(per).map(([k, v]) => [`calls_${k}`, v])),
      })),
    },
    null,
    2,
  ),
);
console.log(`\nWrote ${path.relative(CRATE_ROOT, OUT_PATH)}`);
