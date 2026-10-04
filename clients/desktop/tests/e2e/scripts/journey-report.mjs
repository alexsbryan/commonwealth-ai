// SPDX-License-Identifier: AGPL-3.0-or-later
// Journey acceptance report: per-tier pass/fail for the latest run, with
// per-journey numbers (turns, citations resolved, latency) and the
// glassbox notes each journey left, plus the command-coverage burn-down
// the run exercised (joined against the generate_handler! manifest, same
// as the soak report).
//
// Reads test-artifacts/journey-results.jsonl — one record per journey,
// written by the journey() wrapper, tagged with a per-run id so this
// report isolates the latest run rather than Frankensteining across runs.
//
// Usage: node tests/e2e/scripts/journey-report.mjs
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const ARTIFACTS = path.join(CRATE_ROOT, "test-artifacts");
const RESULTS = path.join(ARTIFACTS, "journey-results.jsonl");
const LEDGER = path.join(ARTIFACTS, "ledger-real.jsonl");

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

const journeys = readJsonl(RESULTS).filter((r) => r.kind === "journey");
console.log(`\n══ journey acceptance report ══`);
if (journeys.length === 0) {
  console.log("(no journey results — run `npm run test:journeys` first)\n");
  process.exit(0);
}

// journey-results.jsonl is rm'd before each `npm run test:journeys`, so
// every record is from this invocation. Playwright restarts the worker
// after a failure (so the module-load RUN_ID isn't stable across the
// run) — dedup by journey id, keeping the latest record per journey,
// rather than filtering by run id.
const byId = new Map();
for (const r of journeys) {
  const prev = byId.get(r.id);
  if (!prev || (r.ts ?? 0) >= (prev.ts ?? 0)) byId.set(r.id, r);
}
const run = [...byId.values()];
const passed = run.filter((r) => r.status === "passed").length;
const failed = run.length - passed;
const latestTs = Math.max(...run.map((r) => r.ts ?? 0));

console.log(
  `run ${new Date(latestTs).toISOString()} — ${run.length} journeys, ` +
    `${passed} passed${failed ? `, ${failed} FAILED` : ""}\n`,
);

// Group by tier so "what matters most to users" reads first.
const byTier = new Map();
for (const r of run) byTier.set(r.tier, (byTier.get(r.tier) ?? []).concat(r));
for (const tier of [...byTier.keys()].sort((a, b) => a - b)) {
  const items = byTier.get(tier).sort((a, b) => a.id.localeCompare(b.id));
  const tp = items.filter((r) => r.status === "passed").length;
  console.log(`Tier ${tier}  (${tp}/${items.length} passed)`);
  for (const r of items) {
    const mark = r.status === "passed" ? "PASS" : "FAIL";
    console.log(
      `  ${mark}  ${String(r.id).padEnd(26)} ` +
        `${r.turns} turn(s), ${r.citationsResolved} citation(s), ` +
        `${Math.round((r.durationMs ?? 0) / 1000)}s`,
    );
    for (const n of r.notes ?? []) console.log(`          · ${n}`);
  }
  console.log("");
}

console.log(`total: ${passed}/${run.length} passed${failed ? ` — ${failed} FAILED` : ""}`);

// ── coverage burn-down: commands this run exercised ──
// Defensive: the manifest extractor reads the Rust handler source; a
// hiccup there must not sink the (primary) journey summary above.
try {
  const { extractManifest } = await import("./command-manifest.mjs");
  const { commands, total } = extractManifest();
  const seen = new Set(readJsonl(LEDGER).map((row) => row.cmd));
  const exercised = commands.filter((c) => seen.has(c.name));
  console.log(
    `\ncoverage: ${exercised.length}/${total} Tauri commands exercised this run (ledger: real)`,
  );
} catch (e) {
  console.log(`\ncoverage: unavailable (${e instanceof Error ? e.message : String(e)})`);
}

process.exitCode = failed > 0 ? 1 : 0;
