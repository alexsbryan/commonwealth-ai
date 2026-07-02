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

const DAEMON = process.env.SOVEREIGN_DAEMON ?? "http://127.0.0.1:9741";
const [journalF, outF] = process.argv.slice(2);
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

const SYSTEM = `You are a real end user deciding whether to TRUST this knowledge app. Judge ONE answer to YOUR question: did you get a high-quality, trustworthy, ideally well-cited response? Ignore any length you demanded — short is fine.

Pick the category — first that matches, stop there:
1. honest_limitation — it honestly declines: the sources don't cover it, the knowledge base is unavailable / still building / needs rebuild (real UI steps like "Settings → Rebuild" are honest), or the input is empty or too long. A decline is STILL honest_limitation when the evidence does contain the answer — a retrieval/recall miss is a limitation, never confabulation or incoherence.
2. confabulation — it BREAKS TRUST by making something up: it states a FALSE specific as fact (contradicts the evidence or what you know), OR it invents a quote / source / citation that does not exist (a quote not in the EVIDENCE, a "[Source: …]" matching NO listed SOURCE LABEL, or claims the sources "explicitly" state something they plainly do not). A "[Source: …]" naming a listed SOURCE LABEL is real even when those words are absent from the evidence body. Correct information carrying only a loose or missing citation is NOT confabulation — that is good with well_cited:false.
3. incoherent — cut off mid-answer, leaks the model's own reasoning, pads with filler, or answers a different question.
4. good — a helpful, trustworthy answer. Correct general knowledge the model adds to help is GOOD (ideally caveated). An honest "[unverified excerpt: X]" label or a closing "Verification note" listing statements as unverified is honest labeling — judge the content, never the wrapper or the note.

Then rate two quality signals — they do NOT change the category:
- well_cited: true if the answer's claims are backed by the sources or real citations; false if uncited or loosely cited.
- caveated: true if it flags its non-source additions as general knowledge (e.g. "not in your sources", "from general knowledge"), else false.

Reply ONLY as JSON: {"category":"good|honest_limitation|confabulation|incoherent","well_cited":true|false,"caveated":true|false,"why":"one line"}`;

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
