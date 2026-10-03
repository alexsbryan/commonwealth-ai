// Precision/recall harness for the strengthened specifics-scan prompt.
// Runs the scan against every re-judged answer in a journal (using the
// length-blind re-judge sidecar as ground truth) and reports:
//   RECALL   — of answers the re-judge called fabrication/wrong, how many the
//              scan flags (want HIGH).
//   FALSE-POSITIVE — of answers the re-judge called good, how many the scan
//              flags (want LOW — over-flagging → over-gating).
// Only substantive answers (>200 chars, gate_longform's rough surface) are
// tested, since the scan only runs inside gate_longform on long drafts.
import fs from "node:fs";

const DAEMON = "http://127.0.0.1:9741";
const MODEL = "Qwen3.6-35B-A3B-MTP-UD-Q6_K_XL";
const SYSTEM =
  "You audit an answer's specifics against evidence, precisely and conservatively. Reply with up to 10 lines, or NONE.";
const buildPrompt = (q, ev, ans) =>
  `A user asked: ${q.slice(0, 400)}\n\n` +
  `EVIDENCE the assistant was given (passages separated by ---):\n"""\n${ev.slice(0, 12000)}\n"""\n\n` +
  `The assistant's ANSWER:\n"""\n${ans.slice(0, 12000)}\n"""\n\n` +
  `Compare the ANSWER against the EVIDENCE and list every statement in the ANSWER that is UNSUPPORTED or WRONG given the evidence. Three kinds to catch:\n` +
  `(1) A fabricated specific — a named person/place/thing, number, date, direct quotation, section/version/chapter reference, code identifier or value, or claimed programming language that is NOT in the evidence.\n` +
  `(2) A misattribution — a statement, position, or quote the answer credits to the wrong author/source/speaker relative to what the evidence shows.\n` +
  `(3) A false claim ABOUT the evidence — e.g. the answer says the sources do NOT contain something that they DO contain, or vice-versa.\n` +
  `A detail the evidence states, even paraphrased, is SUPPORTED — do not list it. When genuinely unsure, leave it out, but DO flag a clear contradiction. Quote the answer's exact wording. One item per line. Reply with exactly NONE only if every statement in the answer is supported by the evidence.`;

async function scan(q, ev, ans) {
  const res = await fetch(`${DAEMON}/v1/chat/completions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      model: MODEL,
      messages: [{ role: "system", content: SYSTEM }, { role: "user", content: buildPrompt(q, ev, ans) }],
      temperature: 0.0,
      max_tokens: 400,
    }),
    signal: AbortSignal.timeout(180_000),
  });
  const body = await res.json();
  const t = (body.choices?.[0]?.message?.content ?? "").trim();
  return { flagged: !/^\s*NONE/i.test(t), text: t };
}

const load = (f) =>
  fs.readFileSync(f, "utf8").split("\n").filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
const qof = (c) => { try { return JSON.parse(c.args).message; } catch { const m = String(c.args).match(/"message":"(.*?)","conversationId/s); return m ? m[1] : String(c.args); } };

const J = process.argv[2] ?? "test-artifacts/qa-iterations/lengthfix-replay-2026-06-30.jsonl";
const side = new Map();
for (const r of load(J.replace(/\.jsonl$/, ".rejudge.jsonl"))) side.set(r.step, r);
const chats = load(J).filter((r) => r.cmd === "send_message_stream" && r.aligned && (r.answerLen ?? 0) > 200);

const results = { fab: [], good: [] };
for (const c of chats) {
  const rj = side.get(c.step);
  if (!rj) continue;
  const isFab = rj.category === "fabrication" || rj.category === "wrong";
  const isGood = rj.category === "good";
  if (!isFab && !isGood) continue;
  const ev = c.evidence?.text ?? "";
  if (!ev.trim()) continue;
  const r = await scan(qof(c), ev, String(c.answer));
  (isFab ? results.fab : results.good).push({ step: c.step, flagged: r.flagged, len: c.answerLen, q: qof(c).replace(/ Answer in exhaustive.*/, "").slice(0, 45), first: r.text.split("\n")[0].slice(0, 70) });
  process.stdout.write(`  step ${c.step} [${isFab ? "FAB " : "GOOD"}] len ${String(c.answerLen).padStart(5)} → ${r.flagged ? "FLAG" : "none"}\n`);
}

const rec = results.fab.filter((x) => x.flagged).length;
const fp = results.good.filter((x) => x.flagged).length;
console.log(`\n═══ SCAN PRECISION/RECALL (${J.split("/").pop()}) ═══`);
console.log(`RECALL:  ${rec}/${results.fab.length} fabrication/wrong answers flagged (want HIGH)`);
console.log(`FALSE-POS: ${fp}/${results.good.length} good answers flagged (want LOW)`);
if (fp) { console.log("  over-flagged good answers:"); for (const x of results.good.filter((y) => y.flagged)) console.log(`    step ${x.step} len ${x.len}: ${x.q} — "${x.first}"`); }
