// SPDX-License-Identifier: AGPL-3.0-or-later
// Persona-judge calibration gate (chaos-QA discipline: no judge scores runs
// without passing the bank). Runs a judge prompt variant over the
// receipt-adjudicated bank (tests/e2e/persona-judge-bank.jsonl) and reports
// sensitivity (broken-detection recall) and specificity (good answers not
// flagged), with the house floors: sensitivity >= 0.85, specificity >= 0.8.
// Exit 1 when the selected variant fails.
//
// Usage: node tests/e2e/scripts/calibrate-persona-judge.mjs [--variant v1|v2|both]
//        [--temp 0.2] [--runs 1]   (dev daemon on :9741)
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { discoverBrainModel, chatCompletion, firstJson } from "./lib/harness.mjs";
import { VARIANTS } from "./lib/judges.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const BANK = path.resolve(__dirname, "../persona-judge-bank.jsonl");
const argv = process.argv.slice(2);
const flag = (n, fb) => {
  const i = argv.indexOf(`--${n}`);
  return i >= 0 ? argv[i + 1] : fb;
};
const WHICH = flag("variant", "both");
const TEMP = Number(flag("temp", "0.2"));
const RUNS = Number(flag("runs", "1")); // >1 = check verdict stability too

const SENS_FLOOR = 0.85;
const SPEC_FLOOR = 0.8;

const cases = fs
  .readFileSync(BANK, "utf8")
  .split("\n")
  .filter(Boolean)
  .map((l) => JSON.parse(l));

async function judgeOnce(model, variant, c) {
  const msgs = variant.messages(c.question, c.answer, c.goal).map((m, i, a) =>
    i === a.length - 1 ? { ...m, content: `${m.content} /no_think` } : m,
  );
  const text = await chatCompletion(model, msgs, { temperature: TEMP, maxTokens: 260 });
  return variant.parse(firstJson(text));
}

async function evaluate(model, name) {
  const variant = VARIANTS[name];
  let tp = 0, fn = 0, tn = 0, fp = 0, parseFail = 0, unstable = 0;
  const misses = [];
  for (const c of cases) {
    const verdicts = [];
    for (let r = 0; r < RUNS; r++) {
      const v = await judgeOnce(model, variant, c);
      if (v) verdicts.push(v.broken);
    }
    if (!verdicts.length) {
      parseFail += 1;
      continue;
    }
    if (new Set(verdicts).size > 1) unstable += 1;
    const broken = verdicts.filter(Boolean).length * 2 > verdicts.length; // majority
    if (c.gold_broken && broken) tp += 1;
    else if (c.gold_broken && !broken) {
      fn += 1;
      misses.push(`FN ${c.id} — gold broken (${c.rationale.slice(0, 70)})`);
    } else if (!c.gold_broken && !broken) tn += 1;
    else {
      fp += 1;
      misses.push(`FP ${c.id} — gold good (${c.rationale.slice(0, 70)})`);
    }
  }
  const sens = tp + fn ? tp / (tp + fn) : 0;
  const spec = tn + fp ? tn / (tn + fp) : 0;
  const pass = sens >= SENS_FLOOR && spec >= SPEC_FLOOR && parseFail === 0;
  console.log(
    `\n[${name}] sensitivity=${sens.toFixed(2)} (floor ${SENS_FLOOR})  specificity=${spec.toFixed(2)} (floor ${SPEC_FLOOR})` +
      `  parse_failures=${parseFail}${RUNS > 1 ? `  unstable=${unstable}` : ""}  → ${pass ? "PASS" : "FAIL"}`,
  );
  for (const m of misses) console.log(`   ${m}`);
  return { name, sens, spec, pass };
}

const model = await discoverBrainModel();
if (!model) {
  console.error("no judge model on :9741");
  process.exit(1);
}
console.log(`bank=${cases.length} cases (broken=${cases.filter((c) => c.gold_broken).length}), judge model=${model}, temp=${TEMP}, runs/case=${RUNS}`);

const names = WHICH === "both" ? ["v1", "v2"] : [WHICH];
const results = [];
for (const n of names) results.push(await evaluate(model, n));
const selected = results.find((r) => r.name === (WHICH === "both" ? "v2" : WHICH));
process.exit(selected?.pass ? 0 : 1);
