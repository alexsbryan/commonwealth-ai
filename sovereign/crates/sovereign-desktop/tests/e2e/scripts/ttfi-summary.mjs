#!/usr/bin/env node
// TTFI report pretty-printer.
//
// Reads tests/e2e/.ttfi-report.json and prints a markdown table to
// stdout. If tests/e2e/.ttfi-baseline.json is present, renders both
// columns side-by-side with a delta column so before/after UI tweaks
// can be compared at a glance.
//
// Usage:
//   node tests/e2e/scripts/ttfi-summary.mjs
//   npm run report:ttfi

import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, "..");
const REPORT_PATH = path.join(ROOT, ".ttfi-report.json");
const BASELINE_PATH = path.join(ROOT, ".ttfi-baseline.json");

function readJsonOrNull(p) {
  if (!fs.existsSync(p)) return null;
  try {
    return JSON.parse(fs.readFileSync(p, "utf8"));
  } catch (e) {
    console.error(`failed to parse ${p}: ${e.message}`);
    process.exit(1);
  }
}

function fmt(ms) {
  if (ms == null) return "—";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(2)}s`;
}

function fmtDelta(curr, base) {
  if (curr == null && base == null) return "—";
  if (curr == null) return "(gone)";
  if (base == null) return "(new)";
  const d = curr - base;
  if (Math.abs(d) < 5) return "≈";
  const sign = d < 0 ? "−" : "+";
  return `${sign}${fmt(Math.abs(d))}`;
}

function pad(s, n) {
  return s + " ".repeat(Math.max(0, n - s.length));
}

const report = readJsonOrNull(REPORT_PATH);
if (!report) {
  console.error(`no report found at ${REPORT_PATH}`);
  console.error(`run: npm run test:ttfi`);
  process.exit(1);
}
const baseline = readJsonOrNull(BASELINE_PATH);
const baseRows = new Map(
  (baseline?.rows ?? []).map((r) => [r.scenario, r.ttfi]),
);

const TIERS = ["generic", "specific", "aux", "content"];

console.log("");
console.log(`# TTFI report`);
console.log(`generated: ${report.generated_at}`);
if (baseline) {
  console.log(`baseline:  ${baseline.generated_at}`);
}
console.log("");

if (baseline) {
  // Side-by-side: scenario | tier | base | curr | delta
  const headers = ["Scenario", "Tier", "Baseline", "Current", "Δ"];
  const widths = headers.map((h) => h.length);
  const lines = [];
  for (const row of report.rows) {
    const base = baseRows.get(row.scenario);
    for (const tier of TIERS) {
      const curr = row.ttfi[tier];
      const baseVal = base?.[tier] ?? null;
      const cells = [
        row.scenario,
        tier,
        fmt(baseVal),
        fmt(curr),
        fmtDelta(curr, baseVal),
      ];
      cells.forEach((c, i) => (widths[i] = Math.max(widths[i], c.length)));
      lines.push(cells);
    }
  }
  const sep = widths.map((w) => "-".repeat(w)).join(" | ");
  console.log(headers.map((h, i) => pad(h, widths[i])).join(" | "));
  console.log(sep);
  for (const cells of lines) {
    console.log(cells.map((c, i) => pad(c, widths[i])).join(" | "));
  }
} else {
  // Single-run: scenario | tier | value
  const headers = ["Scenario", "Generic", "Specific", "Aux", "Content"];
  const widths = headers.map((h) => h.length);
  const rows = report.rows.map((r) => [
    r.scenario,
    fmt(r.ttfi.generic),
    fmt(r.ttfi.specific),
    fmt(r.ttfi.aux),
    fmt(r.ttfi.content),
  ]);
  for (const cells of rows) {
    cells.forEach((c, i) => (widths[i] = Math.max(widths[i], c.length)));
  }
  console.log(headers.map((h, i) => pad(h, widths[i])).join(" | "));
  console.log(widths.map((w) => "-".repeat(w)).join(" | "));
  for (const cells of rows) {
    console.log(cells.map((c, i) => pad(c, widths[i])).join(" | "));
  }
  console.log("");
  console.log(
    `(no baseline — run \`npm run report:ttfi:save-baseline\` to capture this run as the reference)`,
  );
}

const warnings = report.rows.flatMap((r) =>
  (r.warnings ?? []).map((w) => `${r.scenario}: ${w}`),
);
if (warnings.length > 0) {
  console.log("");
  console.log("## Advisory budget overruns");
  for (const w of warnings) console.log(`- ${w}`);
}
console.log("");
