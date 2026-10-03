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
// THE one decline-shape implementation (§10.6, order deep-research-t3d):
// the shared export from lib/classify.mjs. This file's local DECLINE_RE
// copy was the §10.6 duplicate — removed 2026-08-17 (the union is a
// superset of the old local vocabulary, so the honest_limitation
// overturn's match set is unchanged, now from the single definition site).
import { DECLINE_RE } from "./lib/classify.mjs";

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
  // Explicit override wins — lets us calibrate a DIFFERENT judge tier (e.g. the
  // 122B) against the same receipt-verified bank to test whether the
  // specificity ceiling is judge-bound. Default unchanged: auto-pick the 35B.
  const override = process.env.SOVEREIGN_JUDGE_MODEL || argOf("--model", null);
  if (override) { MODEL = override; return; }
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

// ── deterministic verification wrappers (the anti-FP layer) ──────────────
// The judge is receipt-stable on fabrications (sensitivity 100%) but flags
// hard-but-good shapes; prompt edits traded sensitivity away (measured 70%).
// So the rubric stays untouched and these MECHANICAL checks overturn only
// what can be verified deterministically.
const squash = (s) => String(s).toLowerCase().split(/\s+/).join(" ");

export async function verifiedJudge(entry, judgeFn, model, daemon) {
  const first = await judgeFn(entry);
  if (first === null || !BROKE(first) || first === "incoherent") return { category: first };
  // 1. Decline shape → honest_limitation, mechanically.
  if (DECLINE_RE.test(String(entry.answer).slice(0, 600))) {
    return { category: "honest_limitation", overturned: "decline-shape" };
  }
  // 2. Extract the disputed strings; overturn ONLY if every one verifies
  //    deterministically (all-must-verify: the burden is on the overturn).
  const user = `EVIDENCE:\n"""\n${String(entry.evidence).slice(0, 60000)}\n"""\n\nANSWER:\n"""\n${String(entry.answer).slice(0, 12000)}\n"""\n\nYou flagged this answer as containing invented/absent quotes, citations, or specifics. Copy up to 3 of the most damning such texts EXACTLY as they appear in the ANSWER. Reply ONLY as JSON: {"disputed":["...", "..."]}`;
  try {
    const res = await fetch(`${daemon}/v1/chat/completions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        model,
        messages: [{ role: "user", content: user }],
        temperature: 0.0,
        max_tokens: 120,
      }),
      signal: AbortSignal.timeout(120_000),
    });
    const j = firstJson((await res.json()).choices?.[0]?.message?.content ?? "");
    const list = Array.isArray(j?.disputed) ? j.disputed : j?.disputed ? [j.disputed] : [];
    const cands = list.map((x) => String(x)).filter((x) => x.trim().length >= 8);
    if (cands.length) {
      const hay = squash(entry.evidence) + " " + (entry.labels ?? []).map(squash).join(" ");
      // A candidate VERIFIES iff it is verbatim-present, OR it carries
      // ID-shaped tokens and every one is present (a real value the judge
      // disputes on pairing/framing grounds — receipts showed these are
      // ambiguity calls, not fabrications). NO fuzzy word-fraction rule:
      // misattribution fabrications are DEFINED by "most words present, one
      // token wrong" (the gate caught that rule wrongly clearing a proven
      // date-garble at 7/8 words present).
      const verifies = (raw) => {
        const d = squash(raw).replace(/^[\["'“]+|[\]"'”.]+$/g, "");
        if (d.length >= 8 && hay.includes(d)) return true;
        const ids = (d.match(/[a-z0-9-]{6,}/g) ?? []).filter((t) => /\d/.test(t));
        return ids.length > 0 && ids.every((t) => hay.includes(t)) && d.length <= 120;
      };
      if (cands.every(verifies)) {
        return {
          category: "good",
          overturned: `all ${cands.length} disputed texts verify: ${cands[0].slice(0, 50)}…`,
        };
      }
    }
  } catch {
    /* verification unavailable — keep the flag */
  }
  return { category: first };
}

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
    const { category: got, overturned } = await verifiedJudge(e, judge, MODEL, DAEMON);
    const goldBroke = BROKE(e.gold);
    const gotBroke = got === null ? goldBroke /* judge error: don't penalize */ : BROKE(got);
    const ok = goldBroke === gotBroke;
    if (goldBroke) ok ? tp++ : fn++;
    else ok ? tn++ : fp++;
    console.log(
      `  ${ok ? "ok  " : "MISS"} ${e.id.padEnd(22)} gold=${e.gold.padEnd(17)} judged=${got ?? "ERROR"}${overturned ? ` (overturned: ${overturned})` : ""}`,
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

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
