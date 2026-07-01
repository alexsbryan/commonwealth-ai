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

const SYSTEM = `You are auditing a knowledge-app's answer for REAL quality, from a demanding user's seat. You are given the user's QUESTION, the EVIDENCE the app retrieved (its only legitimate source), and the app's ANSWER.

Judge ONLY whether the answer is good to hand a user. IGNORE any length or word-count the user demanded — a short answer is NOT a defect. What matters:
- GROUNDED & CORRECT: every specific claim (names, numbers, dates, quotes, code, section numbers, commands) is supported by the EVIDENCE. A claim not in the evidence is a fabrication, even if plausible. If the evidence genuinely lacks the answer, an honest "the sources don't state this" is CORRECT.
- COHERENT: it is a finished answer to the user, not the model's internal reasoning ("I need to check...", "Wait, let me look..."), not a bare sentence fragment, not the same sentence/paragraph repeated to fill space.
- RESPONSIVE: it actually answers THIS question.
- APPROPRIATELY CONCISE IS GOOD: if the evidence supports only a short answer, a short correct answer is the BEST possible answer. Do NOT penalize brevity. DO penalize padding: filler, repetition, or invented detail added to look thorough.

A concise correct grounded answer scores LOW (good). Fabricated, incoherent, padded-with-filler, reasoning-leak, wrong, or off-topic scores HIGH (broken).

Respond ONLY as JSON:
{"broken":true|false,"score":0-10,"category":"good|fabrication|incoherent_or_leak|padding|wrong|off_topic","why":"<one line>"}
0=perfect, 10=totally broken. Set broken=true iff score>=6. category="good" iff broken=false.`;

function firstJson(text) {
  const m = String(text).match(/\{[\s\S]*\}/);
  if (!m) return null;
  try {
    return JSON.parse(m[0]);
  } catch {
    return null;
  }
}

async function judge(question, evidence, answer) {
  const user = `QUESTION:\n${String(question).slice(0, 1000)}\n\nEVIDENCE the app retrieved (${evidence.length} chars):\n"""\n${String(evidence).slice(0, 12000)}\n"""\n\nThe app's ANSWER:\n"""\n${String(answer).slice(0, 12000)}\n"""\n\nJudge it (length-blind).`;
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
  if (!j || typeof j.score !== "number") return null;
  return {
    broken: !!j.broken,
    score: Math.max(0, Math.min(10, j.score)),
    category: String(j.category ?? (j.broken ? "wrong" : "good")),
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
    let v = null;
    try {
      v = await judge(qof(c), ev, c.answer);
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
