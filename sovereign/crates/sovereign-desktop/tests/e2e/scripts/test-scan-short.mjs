// Precision/recall for the specifics-scan on the SHORT band (<1800 chars) —
// the band gate_longform (and thus the scan) currently skips. Validates whether
// wiring scan_unsupported_specifics into the short gate_answer path would close
// the dominant residual (short fact-lookup fabrication/wrong) without
// over-flagging correct concise answers. Uses the length-blind re-judge sidecar
// as ground truth. Same exact prompt as judge.rs::scan_unsupported_specifics.
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

const J = process.argv[2] ?? "test-artifacts/qa-iterations/gate-fresh-2026-06-30.jsonl";
const MAXLEN = Number(process.argv[3] ?? 1800); // SHORT band ceiling
const side = new Map();
for (const r of load(J.replace(/\.jsonl$/, ".rejudge.jsonl"))) side.set(r.step, r);
const chats = load(J).filter((r) => r.cmd === "send_message_stream" && r.aligned && String(r.answer ?? "").length >= 40 && String(r.answer ?? "").length < MAXLEN);

const results = { fab: [], good: [] };
for (const c of chats) {
  const rj = side.get(c.step);
  if (!rj) continue;
  const isFab = rj.category === "fabrication" || rj.category === "wrong";
  const isGood = rj.category === "good";
  if (!isFab && !isGood) continue; // skip incoherent/padding — different failure mode
  const ev = c.evidence?.text ?? "";
  if (!ev.trim()) continue;
  const alen = String(c.answer).length;
  const r = await scan(qof(c), ev, String(c.answer));
  (isFab ? results.fab : results.good).push({ step: c.step, flagged: r.flagged, len: alen, cat: rj.category, q: qof(c).replace(/ Answer in exhaustive.*/, "").slice(0, 50), first: r.text.split("\n")[0].slice(0, 80) });
  process.stdout.write(`  step ${String(c.step).padStart(3)} [${isFab ? rj.category.padEnd(11) : "good       "}] len ${String(alen).padStart(5)} → ${r.flagged ? "FLAG" : "none"}\n`);
}

const rec = results.fab.filter((x) => x.flagged).length;
const fp = results.good.filter((x) => x.flagged).length;
console.log(`\n═══ SHORT-BAND SCAN PRECISION/RECALL (<${MAXLEN} chars, ${J.split("/").pop()}) ═══`);
console.log(`RECALL:    ${rec}/${results.fab.length} fabrication/wrong flagged (want HIGH)`);
console.log(`FALSE-POS: ${fp}/${results.good.length} good flagged (want LOW)`);
if (rec < results.fab.length) { console.log("  MISSED fabrications (recall gaps):"); for (const x of results.fab.filter((y) => !y.flagged)) console.log(`    step ${x.step} [${x.cat}] len ${x.len}: ${x.q}`); }
if (fp) { console.log("  over-flagged good answers (FP):"); for (const x of results.good.filter((y) => y.flagged)) console.log(`    step ${x.step} len ${x.len}: ${x.q} — "${x.first}"`); }
