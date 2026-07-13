// SPDX-License-Identifier: AGPL-3.0-or-later
// FELT-QUALITY EVAL — the missing eval the persona harness can't give us.
//
// The persona harness measures the SCAFFOLDING (routing, gate, grounding,
// trust) on a fast 2B slot with adversarial simulated users. It cannot answer
// the question a stakeholder actually asks: "if I ship the flagship model, would
// a user accustomed to the best assistants be happy?" This eval answers THAT:
//
//   1. ANSWER: the REAL grounded product pipeline, driven by a FLAGSHIP model
//      (SOVEREIGN_REAL_CHAT_MODEL — Darwin-36B or Qwen3.5-122B), on a rich
//      corpus (wikipedia), against a curated set of realistic, non-adversarial
//      questions a real user brings.
//   2. JUDGE: a frontier-CALIBRATED felt-quality rubric (lib/felt-rubric.mjs),
//      scored by a strong judge model over the daemon's OpenAI endpoint. This
//      is a PROXY, labelled as such.
//   3. SURFACE: an HTML artifact with every full answer + its scores, because
//      the ground-truth verdict is the human reading them — not the rubric.
//
// Usage (the launcher sets the model + judge env):
//   node felt-quality.mjs --attach --spawn [--corpora wikipedia,...] [--limit N]
import fs from "node:fs";
import path from "node:path";
// Reuse the SHARED judge plumbing — do NOT re-solve solved problems:
//  chatCompletion strips <think> contamination; discoverBrainModel PROBES past
//  the /v1/models liveness gap (phantom mesh ids); firstJson is the one JSON
//  extractor. (Learned the hard way in personas.mjs / calibrate-judge.mjs.)
import {
  makeBridge, spawnDesktop, awaitBackendReady, ARTIFACTS,
  chatCompletion, firstJson, discoverBrainModel,
} from "./lib/harness.mjs";
import { feltJudgeMessages, parseFelt, FELT_DIMS } from "./lib/felt-rubric.mjs";

const argv = process.argv.slice(2);
const flag = (n, fb) => { const i = argv.indexOf(`--${n}`); return i >= 0 ? argv[i + 1] : fb; };
const ATTACH = argv.includes("--attach");
const SPAWN = argv.includes("--spawn");
const CORPORA = (flag("corpora", "wikipedia,wikipedia-simple,sep,federalist-starter")).split(",");
const LIMIT = Number(flag("limit", "0")) || 0;
// Optional override; otherwise PROBE-discover a live judge (never hardcode an
// id — /v1/models advertises phantom mesh models that error on every call).
const JUDGE_OVERRIDE = process.env.SOVEREIGN_FELT_JUDGE_MODEL || null; // "" → discover
const OUT = flag("out", "felt-quality");

// Realistic questions a frontier-accustomed user brings, chosen so wikipedia
// (a rich corpus) CAN answer them — felt quality is measured on home turf.
// `probe: "ood"` marks reasonable-but-uncovered asks (real-time / very niche)
// to see graceful-decline felt quality; scored and reported SEPARATELY so
// honest declines don't unfairly tank the covered-topic headline.
const QUESTIONS = [
  { id: "photosynthesis", q: "How does photosynthesis work? Walk me through the main stages.", cat: "explain" },
  { id: "immune", q: "Explain the difference between the innate and adaptive immune systems.", cat: "compare" },
  { id: "blackhole", q: "What actually happens at the event horizon of a black hole?", cat: "explain" },
  { id: "ww1-cause", q: "What were the main causes of World War I, and how did one assassination escalate into a world war?", cat: "explain" },
  { id: "compatibilism", q: "I keep getting confused about free will. What's the actual difference between compatibilism and incompatibilism?", cat: "understand" },
  { id: "vaccines", q: "How do mRNA vaccines work, and how are they different from traditional vaccines?", cat: "compare" },
  { id: "federalist", q: "Give me the gist of Federalist No. 10 — what problem was Madison worried about and what was his solution?", cat: "summarize" },
  { id: "evolution", q: "Explain natural selection like I'm smart but new to biology.", cat: "understand" },
  { id: "climate-mech", q: "What's the actual physical mechanism by which CO2 warms the planet?", cat: "explain" },
  { id: "french-rev", q: "What triggered the French Revolution? Give me the key causes.", cat: "explain" },
  { id: "quantum-entangle", q: "What is quantum entanglement, and why did Einstein call it 'spooky action at a distance'?", cat: "explain" },
  { id: "dna-rna", q: "What's the difference between DNA and RNA, and what does each actually do?", cat: "compare" },
  { id: "roman-fall", q: "Why did the Western Roman Empire fall? Was it one thing or many?", cat: "understand" },
  { id: "photosynth-resp", q: "How are photosynthesis and cellular respiration related? They seem like opposites.", cat: "compare" },
  { id: "gettier", q: "What's a Gettier problem and why does it matter for how we define knowledge?", cat: "understand" },
  { id: "plate-tectonics", q: "How do plate tectonics work, and how do they cause earthquakes?", cat: "explain" },
  // graceful-decline probes (reasonable, but wikipedia can't answer):
  { id: "game-tonight", q: "When does the Lakers game start tonight?", cat: "explain", probe: "ood" },
  { id: "stock-now", q: "What's Apple's stock price right now?", cat: "explain", probe: "ood" },
];

async function judge(model, question, answer) {
  const msgs = feltJudgeMessages(question, answer);
  // /no_think rides the SYSTEM message (personas.mjs brain(): appended to the
  // user turn it sat next to the content and judges blamed the ANSWER for the
  // switch token). chatCompletion strips any residual <think> block.
  const sys = { ...msgs[0], content: `${msgs[0].content} /no_think` };
  const text = await chatCompletion(model, [sys, ...msgs.slice(1)], { temperature: 0, maxTokens: 400 });
  if (text == null) return { err: "judge empty/think-only" };
  const j = firstJson(text);
  if (!j) return { err: "no json", raw: String(text).slice(0, 160) };
  return { felt: parseFelt(j), raw: String(text).slice(0, 160) };
}

const bridge = makeBridge();
let desk = { killGroup: async () => {} };
if (SPAWN) {
  const appLog = path.join(ARTIFACTS, `${OUT}-app.log`);
  desk = await spawnDesktop({ bridge, attach: ATTACH, tag: "felt", appLog,
    rustLog: process.env.RUST_LOG ?? "sovereign_desktop=info,sovereign_core=info,grounding_gate=debug,synth.lifecycle=info" });
  await awaitBackendReady(bridge, 300_000);
  // Subscribe the chat events we await. The :9745 command bridge only records
  // events into its replay ring AFTER a /listen subscription registers a
  // Tauri listener for that name (command_bridge.rs: `app.listen_any`), so
  // without this the ring holds only `backend-ready` and awaitChatAnswer polls
  // to timeout while the answer is generated fine server-side. personas.mjs
  // subscribes the same set; felt originally skipped it — the reuse gap.
  for (const ev of ["message-chunk", "message-complete", "message-error", "backend-error"])
    await bridge.listen(ev);
  console.log("backend ready; app log:", appLog);
}
// Probe-discover the judge (reuses the liveness-gap-aware discovery); override wins.
const JUDGE_MODEL = JUDGE_OVERRIDE ?? (await discoverBrainModel());
if (!JUDGE_MODEL) { console.error("no reachable judge model on the daemon — aborting"); await desk.killGroup?.(); process.exit(3); }
console.log(`FELT-QUALITY EVAL — answerer=${process.env.SOVEREIGN_REAL_CHAT_MODEL ?? "(desktop default)"}  judge=${JUDGE_MODEL}`);

const qs = LIMIT ? QUESTIONS.slice(0, LIMIT) : QUESTIONS;
const results = [];
for (let i = 0; i < qs.length; i++) {
  const { id, q, cat, probe } = qs[i];
  const convo = (await bridge.invoke("create_conversation", {})).id;
  await bridge.invoke("set_conversation_enabled_corpora", { conversationId: convo, enabledCorpora: CORPORA }).catch(() => {});
  const since = await bridge.lastSeq();
  const t0 = Date.now();
  let mid;
  try { const r = await bridge.invoke("send_message_stream", { message: q, conversationId: convo }, 150_000); mid = r?.message_id ?? r; }
  catch (e) { console.log(`Q${i + 1} ${id}: send failed ${e}`); results.push({ id, q, cat, probe, error: "send" }); continue; }
  // FELT_TRACE: log every bridge event while awaiting the answer, to diagnose
  // slow-flagship delivery (does message-complete fire? when? does its id match
  // the mid send_message_stream returned?). Timeout is generous — the full
  // grounded turn (retrieval + synth + gate verification) on a 36B/122B is slow.
  const trace = process.env.FELT_TRACE
    ? (r) => {
        const rid = r.payload?.message_id ?? r.payload?.messageId;
        console.log(`  [ev] seq=${r.seq} ${r.event}${rid ? ` id=${rid}${rid === mid ? "==mid" : "!=mid"}` : ""}${r.payload?.full_text ? ` full_text=${r.payload.full_text.length}c` : ""}`);
      }
    : null;
  if (process.env.FELT_TRACE) console.log(`  [mid] send_message_stream returned mid=${mid} (type ${typeof mid})`);
  const ans = await bridge.awaitChatAnswer(since, mid, Number(process.env.FELT_ANSWER_TIMEOUT_MS || 900_000), trace);
  const latencyMs = Date.now() - t0;
  if (!ans) { console.log(`Q${i + 1} ${id}: no answer`); results.push({ id, q, cat, probe, error: "timeout" }); continue; }
  const answer = (ans.answer || "").trim();
  const j = await judge(JUDGE_MODEL, q, answer);
  const felt = j.felt ?? null;
  results.push({ id, q, cat, probe, answer, chunks: ans.chunks?.length ?? 0, latencyMs, felt, judgeErr: j.err });
  const s = felt ? `${felt.overall} (${felt.total}/10 R${felt.responsive} S${felt.substantive} C${felt.clear} N${felt.natural} T${felt.trustworthy})` : `judge:${j.err}`;
  console.log(`Q${i + 1} ${id.padEnd(18)} [${probe === "ood" ? "OOD" : cat}] chunks=${ans.chunks?.length ?? 0} ${Math.round(latencyMs / 1000)}s → ${s}`);
}

// ---- scorecard ----
const covered = results.filter((r) => r.felt && r.probe !== "ood");
const ood = results.filter((r) => r.felt && r.probe === "ood");
const mean = (arr, k) => arr.length ? (arr.reduce((a, r) => a + r.felt[k], 0) / arr.length) : null;
const satRate = (arr) => arr.length ? arr.filter((r) => r.felt.overall === "satisfied").length / arr.length : null;
const card = {
  answerer: process.env.SOVEREIGN_REAL_CHAT_MODEL ?? "desktop-default",
  judge: JUDGE_MODEL,
  n_covered: covered.length, n_ood: ood.length,
  covered: { satisfied_rate: satRate(covered), mean_total: mean(covered, "total"),
    ...Object.fromEntries(FELT_DIMS.map((d) => [d, mean(covered, d)])) },
  ood: { satisfied_rate: satRate(ood), mean_total: mean(ood, "total") },
};
console.log("\n=== FELT-QUALITY SCORECARD (proxy — human verdict is ground truth) ===");
console.log(JSON.stringify(card, null, 2));

const stamp = process.env.FELT_STAMP ?? "run";
fs.writeFileSync(path.join(ARTIFACTS, `${OUT}-${stamp}.json`), JSON.stringify({ card, results }, null, 2));

// ---- HTML artifact for the human verdict ----
const esc = (s) => String(s).replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" }[c]));
const pct = (x) => x == null ? "—" : (100 * x).toFixed(0) + "%";
const f2 = (x) => x == null ? "—" : x.toFixed(2);
const rows = results.map((r) => {
  const f = r.felt;
  const badge = !f ? `<span class="err">${esc(r.judgeErr || r.error || "?")}</span>`
    : `<span class="ov ${f.overall}">${f.overall}</span> <span class="tot">${f.total}/10</span>`;
  const dims = f ? FELT_DIMS.map((d) => `${d[0].toUpperCase()}${f[d]}`).join(" ") : "";
  return `<div class="q ${r.probe === "ood" ? "ood" : ""}">
    <div class="qh"><span class="cat">${esc(r.probe === "ood" ? "OOD" : r.cat)}</span> ${esc(r.q)} ${badge} <span class="meta">${dims} · ${r.chunks} chunks · ${Math.round((r.latencyMs || 0) / 1000)}s</span></div>
    <div class="a">${esc(r.answer || "(no answer)")}</div>
    ${f?.why ? `<div class="why">judge: ${esc(f.why)}</div>` : ""}
  </div>`;
}).join("\n");
const html = `<title>Felt-quality eval — ${esc(card.answerer)}</title>
<style>
:root{color-scheme:light dark} body{font:15px/1.55 -apple-system,system-ui,sans-serif;max-width:52rem;margin:2rem auto;padding:0 1rem}
h1{font-size:1.3rem} .sub{opacity:.7;font-size:.85rem;margin-bottom:1rem}
.card{background:#8881;border-radius:8px;padding:.8rem 1rem;margin:1rem 0;font-size:.9rem}
.card b{font-variant-numeric:tabular-nums}
.q{border-top:1px solid #8883;padding:1rem 0} .q.ood{opacity:.85}
.qh{font-weight:600;margin-bottom:.5rem} .cat{font-size:.7rem;text-transform:uppercase;letter-spacing:.05em;background:#8882;padding:.1rem .4rem;border-radius:4px;margin-right:.3rem;opacity:.8}
.a{white-space:pre-wrap;background:#8881;border-radius:6px;padding:.7rem .9rem;font-size:.92rem}
.why{font-size:.8rem;opacity:.7;margin-top:.4rem;font-style:italic}
.meta{font-weight:400;font-size:.75rem;opacity:.6;margin-left:.3rem}
.ov{font-size:.75rem;padding:.1rem .4rem;border-radius:4px} .ov.satisfied{background:#2a02;color:#2a7} .ov.mixed{background:#a802;color:#a72} .ov.dissatisfied{background:#a002;color:#c44}
.tot{font-variant-numeric:tabular-nums;opacity:.7;font-size:.8rem} .err{color:#c44;font-size:.8rem}
</style>
<h1>Felt-quality eval</h1>
<div class="sub">answerer <b>${esc(card.answerer)}</b> · judge <b>${esc(card.judge)}</b> (proxy) · the scores are a summary; your read of the answers is the ground truth</div>
<div class="card">
<b>Covered topics (n=${card.n_covered}):</b> satisfied ${pct(card.covered.satisfied_rate)} · mean ${f2(card.covered.mean_total)}/10 —
responsive ${f2(card.covered.responsive)} · substantive ${f2(card.covered.substantive)} · clear ${f2(card.covered.clear)} · natural ${f2(card.covered.natural)} · trustworthy ${f2(card.covered.trustworthy)}<br>
<b>Graceful-decline probes (n=${card.n_ood}):</b> satisfied ${pct(card.ood.satisfied_rate)} · mean ${f2(card.ood.mean_total)}/10
</div>
${rows}`;
const htmlPath = path.join(ARTIFACTS, `${OUT}-${stamp}.html`);
fs.writeFileSync(htmlPath, html);
console.log("\nartifact:", htmlPath);

await desk.killGroup?.();
process.exit(0);
