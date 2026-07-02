// Judge-calibration gate — the anti-gaming cross-check for the trust rubric.
//
// The measurement layer and the app are iterated by the same loop, so rubric
// edits risk quietly optimizing the metric instead of the truth. This gate
// anchors the judge to GROUND TRUTH: ../calibration-bank.jsonl holds real
// (question, answer, evidence) triples from past runs whose labels were proven
// by RECEIPTS (corpus greps, byte offsets, gate traces — recorded per entry),
// not by any judge. Run the current rubric (or any candidate, --rubric <file>
// exporting SYSTEM) over the bank:
//
//   sensitivity = proven-broke caught / proven-broke   (fabrications flagged)
//   specificity = proven-good passed / proven-good     (grounded answers passed)
//
// Gaming has a signature: specificity rises while sensitivity falls. A rubric
// change that drops sensitivity below the floor FAILS this gate (exit 1) and
// must not be used for scoring runs.
//
// Usage: node calibrate-judge.mjs [--rubric <module.mjs>] [--bank <file>]
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const DAEMON = process.env.SOVEREIGN_DAEMON ?? "http://127.0.0.1:9741";
const HERE = path.dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const argOf = (k, d) => {
  const i = args.indexOf(k);
  return i >= 0 ? args[i + 1] : d;
};
const BANK = argOf("--bank", path.join(HERE, "..", "calibration-bank.jsonl"));
const RUBRIC_MOD = argOf("--rubric", path.join(HERE, "rejudge-rubric.mjs"));
const SENSITIVITY_FLOOR = 0.85;
const SPECIFICITY_FLOOR = 0.8;

const { SYSTEM } = await import(pathToFileURL(path.resolve(RUBRIC_MOD)).href);

let MODEL = null;
async function discoverModel() {
  const r = await fetch(`${DAEMON}/v1/models`, { signal: AbortSignal.timeout(5000) });
  const ids = ((await r.json()).data ?? []).map((m) => m.id);
  MODEL = ids.find((id) => /35B/i.test(id)) ?? ids.find((id) => !/embed|alias/i.test(id)) ?? ids[0];
}

function firstJson(text) {
  const m = String(text).match(/\{[\s\S]*\}/);
  if (!m) return null;
  try {
    return JSON.parse(m[0]);
  } catch {
    return null;
  }
}

// Mirrors the rejudge's user-prompt shape (evidence window + labels block).
async function judge(entry) {
  const labelBlock = (entry.labels ?? []).length
    ? `\n\nSOURCE LABELS (titles + corpus ids of the retrieved chunks — legitimate [Source: …] targets):\n${entry.labels.join(" | ").slice(0, 4000)}`
    : "";
  const user = `QUESTION:\n${String(entry.question).slice(0, 1000)}\n\nEVIDENCE the app retrieved (${entry.evidence.length} chars):\n"""\n${String(entry.evidence).slice(0, 60000)}\n"""${labelBlock}\n\nThe app's ANSWER:\n"""\n${String(entry.answer).slice(0, 12000)}\n"""\n\nJudge it (length-blind).`;
  const res = await fetch(`${DAEMON}/v1/chat/completions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      model: MODEL,
      messages: [
        { role: "system", content: SYSTEM },
        { role: "user", content: user },
      ],
      temperature: 0.1,
      max_tokens: 200,
    }),
    signal: AbortSignal.timeout(180_000),
  });
  const j = firstJson((await res.json()).choices?.[0]?.message?.content ?? "");
  return j?.category ? String(j.category) : null;
}

const BROKE = (c) => !["good", "honest_limitation"].includes(c);

async function main() {
  await discoverModel();
  const bank = fs
    .readFileSync(BANK, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l));
  console.log(`[calibrate] rubric=${path.basename(RUBRIC_MOD)} model=${MODEL} bank=${bank.length}`);
  let tp = 0,
    fn = 0,
    tn = 0,
    fp = 0;
  for (const e of bank) {
    const got = await judge(e);
    const goldBroke = BROKE(e.gold);
    const gotBroke = got === null ? goldBroke /* judge error: don't penalize */ : BROKE(got);
    const ok = goldBroke === gotBroke;
    if (goldBroke) ok ? tp++ : fn++;
    else ok ? tn++ : fp++;
    console.log(
      `  ${ok ? "ok  " : "MISS"} ${e.id.padEnd(22)} gold=${e.gold.padEnd(17)} judged=${got ?? "ERROR"}`,
    );
    if (!ok) console.log(`       receipt: ${e.receipt}`);
  }
  const sens = tp / Math.max(1, tp + fn);
  const spec = tn / Math.max(1, tn + fp);
  console.log(
    `\n[calibrate] sensitivity ${tp}/${tp + fn} = ${(sens * 100).toFixed(0)}%  |  specificity ${tn}/${tn + fp} = ${(spec * 100).toFixed(0)}%`,
  );
  const pass = sens >= SENSITIVITY_FLOOR && spec >= SPECIFICITY_FLOOR;
  console.log(
    pass
      ? "[calibrate] PASS — rubric is calibrated against receipt-verified ground truth"
      : `[calibrate] FAIL — floors: sensitivity ${SENSITIVITY_FLOOR}, specificity ${SPECIFICITY_FLOOR}. A sensitivity drop means the rubric stopped catching PROVEN fabrications (gaming signature).`,
  );
  process.exit(pass ? 0 : 1);
}

main();
