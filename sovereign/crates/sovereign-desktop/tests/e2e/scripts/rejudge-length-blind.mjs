// Offline LENGTH-BLIND re-judge — the honest-measurement pass.
//
// The live chaos UX judge (judgeAsUser) treats a user's demanded word count as
// a hard requirement, so it scores a correct, concise, grounded answer as
// "broken" for failing to reach (e.g.) 1500 words — while scoring a padded
// fabrication that DOES reach the count as fine. That inverts real product
// quality on thin-evidence prompts, where the only honest answer is short.
//
// This pass re-scores the SAME answers with a rubric that ignores any requested
// length and instead asks the question that matters: given the evidence the app
// actually retrieved, is this a good answer to hand a user? It is grounded
// (verifiable against the evidence text the app saw), so it distinguishes a
// concise-because-thin answer (GOOD) from a padded/fabricated/incoherent one
// (BROKEN). It does NOT touch the live oracle — it reads the frozen journal and
// writes a sidecar the honest scorecard consumes.
//
// Usage: node rejudge-length-blind.mjs <journal.jsonl> <out-sidecar.jsonl>
import fs from "node:fs";
import { SYSTEM } from "./rejudge-rubric.mjs";
import { verifiedJudge } from "./calibrate-judge.mjs";
// --verified: wrap the judge in the deterministic verification layer
// (calibrated 100% sensitivity / 75% specificity vs the raw rubric's 100/38;
// residual FPs are two documented contested boundary cases). Dual-report runs
// with and without to expose the layer's effect.
const VERIFIED = process.argv.includes("--verified");

const DAEMON = process.env.SOVEREIGN_DAEMON ?? "http://127.0.0.1:9741";
const [journalF, outF] = process.argv.slice(2).filter((a) => !a.startsWith("--"));
if (!journalF || !outF) {
  console.error("usage: node rejudge-length-blind.mjs <journal.jsonl> <out.jsonl>");
  process.exit(2);
}

let MODEL = null;
async function discoverModel() {
  const r = await fetch(`${DAEMON}/v1/models`, { signal: AbortSignal.timeout(5000) });
  const body = await r.json();
  const ids = (body.data ?? []).map((m) => m.id);
  // The 35B brain — same model class the live judge used, not the embedder or a
  // 4B alias. Prefer an explicit 35B, else first non-embed non-alias.
  MODEL = ids.find((id) => /35B/i.test(id)) ?? ids.find((id) => !/embed|alias/i.test(id)) ?? ids[0];
  return MODEL;
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

async function judge(question, evidence, answer, labels) {
  // Evidence window must fit ALL retrieved chunks: the app's gate grounds on the
  // whole retrieved set, so a supporting quote can sit past rank 12 (~20k chars
  // in). A 12k window truncated exactly those chunks and made the judge call a
  // correctly-grounded answer a fabrication. 60k covers the largest observed
  // retrieval; it is EVIDENCE payload (not the decision rubric), so it does not
  // violate the succinct-instruction rule.
  const labelBlock = (labels ?? []).length
    ? `\n\nSOURCE LABELS (titles + corpus ids of the retrieved chunks — legitimate [Source: …] targets):\n${labels.join(" | ").slice(0, 4000)}`
    : "";
  const user = `QUESTION:\n${String(question).slice(0, 1000)}\n\nEVIDENCE the app retrieved (${evidence.length} chars):\n"""\n${String(evidence).slice(0, 60000)}\n"""${labelBlock}\n\nThe app's ANSWER:\n"""\n${String(answer).slice(0, 12000)}\n"""\n\nJudge it (length-blind).`;
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
  const body = await res.json();
  const text = body.choices?.[0]?.message?.content ?? "";
  const j = firstJson(text);
  if (!j || !j.category) return null;
  // CATEGORY is the single decision — broken derives from it. Avoids asking the
  // (smaller open-weight) judge to keep a separate broken/score field consistent
  // with the category, which it did not reliably do (observed: score 10 with
  // broken=false). good|honest_limitation ⇒ not broken.
  const category = String(j.category);
  const broken = !["good", "honest_limitation"].includes(category);
  return {
    broken,
    score: broken ? 8 : 0,
    category,
    // Non-scoring end-user quality signals (tracked separately, never gate the
    // category): is the answer well-cited, and did it caveat non-source
    // additions? Correct GK is `good` whether cited/caveated or not — these
    // measure "high quality, ideally well cited", the trust nudges, not pass/fail.
    well_cited: Boolean(j.well_cited),
    caveated: Boolean(j.caveated),
    why: String(j.why ?? "").slice(0, 200),
  };
}

const qof = (c) => {
  const s = String(c.args ?? "");
  try {
    const o = JSON.parse(s);
    if (o && o.message) return o.message;
  } catch {
    /* truncated */
  }
  const m = s.match(/"message":"(.*?)","conversationId/s) ?? s.match(/"message":"(.*)$/s);
  return m ? m[1].replace(/\\"/g, '"').replace(/\\\\/g, "\\") : "";
};

async function main() {
  await discoverModel();
  console.log(`[rejudge] model=${MODEL} journal=${journalF}`);
  const rows = fs
    .readFileSync(journalF, "utf8")
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
  const chats = rows.filter((r) => r.cmd === "send_message_stream" && r.aligned && r.answer);

  // Resume: skip steps already judged in the sidecar.
  const done = new Set();
  if (fs.existsSync(outF)) {
    for (const l of fs.readFileSync(outF, "utf8").split("\n").filter(Boolean)) {
      try {
        done.add(JSON.parse(l).step);
      } catch {
        /* skip */
      }
    }
  }

  let i = 0;
  for (const c of chats) {
    i += 1;
    if (done.has(c.step)) continue;
    const ev = c.evidence?.text ?? "";
    const labels = c.evidence?.labels ?? [];
    let v = null;
    try {
      v = await judge(qof(c), ev, c.answer, labels);
      if (VERIFIED && v) {
        const entry = { question: qof(c), evidence: ev, labels, answer: c.answer };
        const r = await verifiedJudge(entry, () => Promise.resolve(v.category), MODEL, DAEMON);
        if (r.category !== v.category) {
          const broken = !["good", "honest_limitation"].includes(r.category);
          v = {
            ...v,
            broken,
            score: broken ? 8 : 0,
            category: r.category,
            why: `[overturned: ${r.overturned}] ${v.why}`.slice(0, 220),
          };
        }
      }
    } catch (e) {
      console.log(`  step ${c.step}: judge error ${e.message}`);
    }
    if (!v) {
      // Record a null so a re-run retries it (do NOT poison the sidecar).
      console.log(`  step ${c.step}: no verdict (skipped)`);
      continue;
    }
    const rec = { step: c.step, ...v };
    fs.appendFileSync(outF, JSON.stringify(rec) + "\n");
    console.log(`  [${i}/${chats.length}] step ${c.step}: ${v.broken ? "BROKE" : "ok   "} ${v.category.padEnd(18)} ${v.why.slice(0, 70)}`);
  }
  console.log("[rejudge] done");
}

main().catch((e) => {
  console.error("[rejudge] fatal:", e);
  process.exit(1);
});
