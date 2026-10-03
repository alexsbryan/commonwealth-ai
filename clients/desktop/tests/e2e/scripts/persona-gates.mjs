// SPDX-License-Identifier: AGPL-3.0-or-later
// Persona-QA gate runner — evaluates journals against persona-gates.toml
// (aspirational floors/targets, each with its theoretical rationale).
// Multiple journals aggregate into one evaluation (bigger N). Exits
// non-zero when any FLOOR fails; insufficient-n is reported, never a
// silent pass.
//
// Usage: node tests/e2e/scripts/persona-gates.mjs <journal.jsonl>...
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseToml } from "./lib/toml.mjs";
import { computeMetrics } from "./lib/metrics.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const GATES = path.resolve(__dirname, "../persona-gates.toml");

const files = process.argv.slice(2);
if (!files.length) {
  console.error("usage: persona-gates.mjs <journal.jsonl> [...]");
  process.exit(1);
}

// persona-gates.toml uses [table] headers (one per metric) — the shared
// mini-parser handles [[array]] only, so parse tables here: split on
// headers, reuse value parsing via a tiny shim.
function parseGates(src) {
  const gates = {};
  let cur = null;
  for (const rawLine of src.split("\n")) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;
    const h = line.match(/^\[([a-z_0-9]+)\]$/);
    if (h) {
      cur = {};
      gates[h[1]] = cur;
      continue;
    }
    if (!cur) continue;
    const kv = line.match(/^([a-z_0-9]+)\s*=\s*(.*)$/);
    if (!kv) continue;
    const [, k, raw] = kv;
    if (raw.startsWith('"""')) {
      cur[k] = "(rationale)"; // multi-line rationale — not needed at runtime
      continue;
    }
    if (raw.startsWith('"')) cur[k] = raw.replace(/^"|"$/g, "");
    else if (/^-?\d+(\.\d+)?$/.test(raw)) cur[k] = Number(raw);
  }
  return gates;
}

const gates = parseGates(fs.readFileSync(GATES, "utf8"));
const rows = files.flatMap((f) =>
  fs
    .readFileSync(f, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => {
      try {
        return JSON.parse(l);
      } catch {
        return null;
      }
    })
    .filter(Boolean),
);
const m = computeMetrics(rows);

console.log(
  `persona gates — ${files.length} journal(s), ${m.nSessions} sessions / ${m.nTurns} turns (judge v2)\n`,
);
let floorFailures = 0;
let evaluated = 0;
const fmt = (v) => (v == null ? "—" : Number.isInteger(v) ? String(v) : v.toFixed(2));
for (const [name, g] of Object.entries(gates)) {
  const value = m[name];
  const enoughN =
    (g.min_n_sessions == null || m.nSessions >= g.min_n_sessions) &&
    (g.min_n_turns == null || m.nTurns >= g.min_n_turns);
  if (value == null || !enoughN) {
    console.log(`  ?  ${name.padEnd(22)} ${fmt(value).padStart(7)}  insufficient-n`);
    continue;
  }
  evaluated += 1;
  const passFloor = g.direction === "min" ? value >= g.floor : value <= g.floor;
  const passTarget = g.direction === "min" ? value >= g.target : value <= g.target;
  const mark = passTarget ? "✓✓" : passFloor ? "✓ " : "✗ ";
  if (!passFloor) floorFailures += 1;
  console.log(
    `  ${mark} ${name.padEnd(22)} ${fmt(value).padStart(7)}  floor ${g.direction === "min" ? ">=" : "<="} ${g.floor}, target ${g.target}${
      passTarget ? "  TARGET MET" : passFloor ? "" : "  FLOOR FAILED"
    }`,
  );
}
console.log(
  `\n${floorFailures === 0 ? "GATES PASS" : `GATES FAIL — ${floorFailures} floor failure(s)`} (${evaluated} evaluated)`,
);
process.exit(floorFailures === 0 ? 0 : 1);
