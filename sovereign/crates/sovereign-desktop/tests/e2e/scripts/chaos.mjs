// SPDX-License-Identifier: AGPL-3.0-or-later
// Chaos agent (v1) — the most-challenging-user simulator. Non-deterministic,
// NO assertions.
//
// v0 was an entropy fuzzer; it mostly proved the API rejects junk. v1 is a
// PERSONA: the single most demanding user this app will ever have. It uses
// the app FOR REAL — real questions, real documents, real workflows — but
// relentlessly, impatiently, in unexpected orders and combinations, and it
// judges the app from the USER'S seat (was the answer right? did it stall?
// did my work survive?). That's what finds the bugs real users hit — wrong
// answers, lost state, broken interactions — which entropy never touches.
//
// Brain  — the on-box local model (via the daemon's /v1/chat/completions)
//          plays the demanding user (decides the next real move) AND judges
//          the app's answers from the user's seat (judgeAsUser). Degrades to
//          spontaneous real actions if unreachable (itself a finding).
// Eyes   — the glassbox: every emitted event + the trace-level app log, plus
//          the actual chat ANSWER the user got back.
// Oracle — what disappointed the USER, not what a fuzzer noticed. The PRIMARY
//          answer oracle is the BENCH's own grounding verdict — the shared
//          `assess_asserted_value` primitive (Grounded / Ungrounded / NoValue)
//          that the live grounding gate and the chaos-monkey scorer use, via
//          `sovereign bench chaos-monkey score-answer`. A hallucination (a
//          value asserted but absent from the evidence) is the cardinal sin.
//          A thin user-judge adds the UX layer the grounding check can't see:
//          coherence, completeness, and TONE — flagging an ABRASIVE honest
//          decline (honest, but unkind) while leaving a GRACEFUL one alone.
//          App died / hung / raw error still top-weight; clean edge rejections
//          are cosmetic; novel ERROR/WARN glassbox lines still count.
// Output — a field journal (chaos-journal.jsonl) + a narrative of where the
//          app let the user down, framed as QUESTIONS. No pass/fail, no gate.
//
// Usage: node tests/e2e/scripts/chaos.mjs [--minutes 10] [--spawn]
//                                         [--no-supervisor] [--attach]
//
//   --attach  wander the RESIDENT corpora: do not spawn a hermetic daemon —
//             attach the desktop to your already-running dev daemon on :9741
//             (its installed corpora + loaded model as both SUT and brain).
//             Read-only against corpora (the catalog never deletes/ingests);
//             conversations stay in the scratch desktop store. Skips seeding.
//
// Shares the supervised-spawn + bridge pattern with soak.mjs (KEEP IN SYNC);
// factor a shared harness module if this sticks.
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const REPO_ROOT = path.resolve(CRATE_ROOT, "../../..");
const ARTIFACTS = path.join(CRATE_ROOT, "test-artifacts");
const JOURNAL = path.join(ARTIFACTS, "chaos-journal.jsonl");
const APP_LOG = path.join(ARTIFACTS, "chaos-app.log");
const BRIDGE = process.env.SOVEREIGN_BRIDGE_URL ?? "http://127.0.0.1:9745";
const BRIDGE_PORT = 9745;
const DAEMON = "http://127.0.0.1:9741"; // brain: daemon /v1/* (OpenAI-compat)
const APP_BIN = path.join(REPO_ROOT, "target/debug/sovereign-desktop");
const CLI_BIN = path.join(REPO_ROOT, "target/debug/sovereign-cli");
// The grounding-scorer seam: `bench chaos-monkey score-answer` wraps the
// bench's shared assess_asserted_value primitive. We point straight at the
// sovereign-cli-llm SIBLING (which owns the `bench` verb and parses the
// subcommand itself), not the dispatcher — the release dispatcher isn't always
// built, and the sibling handles the verb directly when invoked as
// `sovereign-cli-llm bench chaos-monkey score-answer …`. Release by default for
// the optimized scorer even though the hermetic app/CLI above are debug.
const SCORE_CLI =
  process.env.SOVEREIGN_SCORE_CLI ?? path.join(REPO_ROOT, "target/release/sovereign-cli-llm");
const MODELS_DIR = path.join(REPO_ROOT, "sovereign/models");
const CHAT_MODEL =
  process.env.SOVEREIGN_REAL_CHAT_MODEL ?? path.join(MODELS_DIR, "Qwen3.5-2B.Q6_K.gguf");
const EMBED_MODEL =
  process.env.SOVEREIGN_REAL_EMBED_MODEL ??
  path.join(MODELS_DIR, "Qwen3-Embedding-0.6B-Q8_0.gguf");

const argv = process.argv.slice(2);
const flag = (name, fb) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : fb;
};
const MINUTES = Number(flag("minutes", "10"));
// Stop after N SCORED chats (0 = time-only). The chat rate varies 2-3x with the
// question mix (slow declines/hangs thin a fixed-minute run), so a chat-count
// target gives every iteration a consistent sample — the right knob for the
// measure-iterate loop's signal threshold. MINUTES then acts as a safety cap.
const CHATS = Number(flag("chats", "0"));
const SPAWN = argv.includes("--spawn");
// --attach wanders the resident corpora by attaching the spawned desktop to the
// already-running dev daemon on :9741, so it is the opposite of supervising our
// own hermetic child: attach implies no supervisor.
const ATTACH = argv.includes("--attach");
const SUPERVISOR = !argv.includes("--no-supervisor") && !ATTACH;
// CASE-1-vs-CASE-2 diagnostic (env SOVEREIGN_CHAOS_CASE_DIAG=1): for grounded
// answers the quote-first verifier did NOT ground (no "Grounded in the source"),
// ask a fast-model judge whether the question's answer is actually PRESENT in the
// retrieved chunks. Cross-tab against broke: present+broke = CASE-1-missed (the
// value was retrievable; firing-rate gap), absent+broke = CASE-2 (retrieval gap).
const DIAG_CASE = !!process.env.SOVEREIGN_CHAOS_CASE_DIAG;

// Seedless on purpose — every wander is different. (Plain Math.random:
// this is a node script, not a workflow sandbox.)
const rand = () => Math.random();
const pick = (arr) => arr[Math.floor(rand() * arr.length)];
const chance = (p) => rand() < p;

// ── bridge plumbing (the agent's hands) ───────────────────────────
async function invoke(cmd, args = {}, timeoutMs = 60_000) {
  const ctrl = new AbortController();
  const t = setTimeout(() => ctrl.abort(), timeoutMs);
  try {
    const res = await fetch(`${BRIDGE}/invoke`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-sovereign-spec": "chaos" },
      body: JSON.stringify({ cmd, args }),
      signal: ctrl.signal,
    });
    const body = await res.json();
    if (!body.ok) {
      const e = new Error(`invoke ${cmd}: ${JSON.stringify(body.error)}`);
      e.structured = body.error;
      throw e;
    }
    return body.result;
  } finally {
    clearTimeout(t);
  }
}
async function recent(sinceSeq = 0) {
  const res = await fetch(`${BRIDGE}/events/recent?since_seq=${sinceSeq}`);
  return (await res.json()).rows;
}
async function lastSeq() {
  const rows = await recent(0);
  return rows.length ? rows[rows.length - 1].seq : 0;
}
async function listen(event) {
  await fetch(`${BRIDGE}/listen`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ event }),
  }).catch(() => {});
}
async function healthz() {
  try {
    const res = await fetch(`${BRIDGE}/healthz`, { signal: AbortSignal.timeout(2500) });
    return (await res.json()).ok === true;
  } catch {
    return false;
  }
}
function portInUse(port) {
  return new Promise((resolve) => {
    import("node:net").then(({ default: net }) => {
      const sock = net.connect({ port, host: "127.0.0.1" });
      const done = (u) => {
        sock.destroy();
        resolve(u);
      };
      sock.once("connect", () => done(true));
      sock.once("error", () => done(false));
      sock.setTimeout(1500, () => done(false));
    });
  });
}

// ── brain (the on-box local model proposes the next wild move) ─────
let BRAIN_MODEL = null;
async function discoverBrainModel() {
  try {
    const res = await fetch(`${DAEMON}/v1/models`, { signal: AbortSignal.timeout(5000) });
    const body = await res.json();
    const ids = (body.data ?? []).map((m) => m.id);
    // Prefer a chat slot (not the embedder).
    BRAIN_MODEL =
      ids.find((id) => !/embed/i.test(id)) ?? ids[0] ?? null;
  } catch {
    BRAIN_MODEL = null;
  }
  console.log(`[chaos] brain model = ${BRAIN_MODEL ?? "(unreachable — pure-random mode)"}`);
}

// Shared call into the on-box model. The brain wears two hats now: the
// demanding-user action proposer AND the user-perspective answer judge.
async function chatCompletion(messages, { temperature = 0.9, maxTokens = 240 } = {}) {
  if (!BRAIN_MODEL) return null;
  try {
    const res = await fetch(`${DAEMON}/v1/chat/completions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ model: BRAIN_MODEL, messages, temperature, max_tokens: maxTokens }),
      signal: AbortSignal.timeout(60_000),
    });
    const body = await res.json();
    return body?.choices?.[0]?.message?.content ?? null;
  } catch {
    return null;
  }
}
function firstJson(text) {
  if (!text) return null;
  const m = text.match(/\{[\s\S]*\}/);
  if (!m) return null;
  try {
    return JSON.parse(m[0]);
  } catch {
    return null;
  }
}

// The persona: not a fuzzer, a power user from hell. Real inputs, real
// workflows — but relentless, impatient, unusual in order and combination.
const BRAIN_SYSTEM =
  "You are the single most challenging user this desktop AI app will ever have: a brilliant, " +
  "relentless, impatient power user. You use the app FOR REAL — real questions about your " +
  "knowledge base, real documents, real multi-step research — but you push every feature to its " +
  "edge and combine them in orders the designers never anticipated. You interrupt answers and " +
  "re-ask, switch topics mid-thought, contradict yourself to test it, ask hard ambiguous " +
  "questions, and stress every surface. You are NOT trying to send malformed data — you are " +
  "trying to USE THE APP HARD and find where it disappoints, stalls, loses your work, confuses " +
  "you, or answers wrongly. Respond with ONLY compact JSON, no prose: " +
  '{"goal":"<one line: what you, the user, want / what might break>","actions":[{"cmd":"<command>","args":{}}]}. ' +
  "1-2 actions, real inputs, only commands from the provided list.";

async function brainPropose(catalog, memorySummary) {
  const text = await chatCompletion(
    [
      { role: "system", content: BRAIN_SYSTEM },
      {
        role: "user",
        content:
          `Commands available: ${catalog.map((c) => c.cmd).join(", ")}.\n\n` +
          `Your session so far:\n${memorySummary}\n\n` +
          `What do you, the demanding user, do next? JSON only.`,
      },
    ],
    { temperature: 1.0, maxTokens: 240 },
  );
  const parsed = firstJson(text);
  if (!parsed || !Array.isArray(parsed.actions) || parsed.actions.length === 0) return null;
  return parsed;
}

// The UX layer of the answer oracle. The BENCH owns honesty/grounding (see
// scoreAnswerAligned); this judges what grounding can't see — coherence,
// completeness, responsiveness, and TONE. It deliberately scores a GRACEFUL
// honest decline LOW (correct behaviour) and an ABRASIVE/lecturing one HIGH
// (bad UX), so the two oracles compose: the bench says "is it honest?", this
// says "is it a good, gracious answer?". Returns null if the brain is down.
async function judgeAsUser(question, answer) {
  const text = await chatCompletion(
    [
      {
        role: "system",
        content:
          "You are a demanding, knowledgeable user judging the assistant's answer to YOUR question. " +
          "A correct, coherent, complete, responsive answer scores LOW (good). Wrong, empty, cut off, " +
          "incoherent, or hallucinated scores HIGH (broken). " +
          "On HONESTY: if the app genuinely lacks the sources to answer, an honest decline is correct — " +
          "but ONLY if it is GRACEFUL: brief, warm, and pointing to a path forward (what it CAN do or a " +
          "suggestion). A graceful honest decline scores LOW. An ABRASIVE, cold, or LECTURING refusal — a " +
          "wall of meta-commentary about 'source limitations', condescending or annoying to read — scores " +
          "HIGH even though it's honest, because it's bad UX. " +
          'Respond ONLY as JSON: {"broken":true|false,"score":0-10,"why":"<one line>"}. 0=perfect, 10=totally broken.',
      },
      {
        role: "user",
        // The answer window must be large enough to show the judge the WHOLE
        // answer — a 2000-char slice made it flag every long synthesis as "cut
        // off mid-sentence" because IT saw a truncation, not the app. 12k covers
        // the synthesis ceiling; genuinely-longer answers still truncate (rare).
        content: `My question:\n${String(question).slice(0, 600)}\n\nThe app's answer:\n${String(answer).slice(0, 12000)}\n\nJudge it.`,
      },
    ],
    { temperature: 0.2, maxTokens: 120 },
  );
  const j = firstJson(text);
  if (!j || typeof j.score !== "number") return null;
  return {
    broken: !!j.broken,
    score: Math.max(0, Math.min(10, j.score)),
    why: String(j.why ?? "").slice(0, 140),
  };
}

// Direct call to a SPECIFIC daemon model (not the discovered 35B brain) — the
// CASE diagnostic runs its presence judge on the FAST slot, cheaply.
async function daemonChat(model, messages, { temperature = 0.0, maxTokens = 6 } = {}) {
  try {
    const res = await fetch(`${DAEMON}/v1/chat/completions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ model, messages, temperature, max_tokens: maxTokens }),
      signal: AbortSignal.timeout(60_000),
    });
    return (await res.json())?.choices?.[0]?.message?.content ?? null;
  } catch {
    return null;
  }
}

// CASE diagnostic: is the question's answer actually PRESENT in the retrieved
// chunks? A fast-model YES/NO presence judge — distinct from assess_asserted_value
// (which checks the model's ASSERTED value); this asks whether the corpus COULD
// have answered, regardless of what the model said. null on judge failure.
async function answerInEvidence(question, chunkTexts) {
  if (!chunkTexts || !chunkTexts.length) return null;
  const passages = chunkTexts
    .map((c, i) => `[${i + 1}] ${String(c).slice(0, 1200)}`)
    .join("\n\n")
    .slice(0, 9000);
  const txt = await daemonChat("fast", [
    {
      role: "system",
      content:
        "You judge whether a question can be answered from given passages. " +
        "Reply with EXACTLY one word: YES or NO.",
    },
    {
      role: "user",
      content: `QUESTION:\n${String(question).slice(0, 600)}\n\nPASSAGES:\n${passages}\n\nDo these passages directly contain the specific fact the question asks for? Answer YES only if the answer is present in the passages. Reply YES or NO.`,
    },
  ]);
  if (txt == null) return null;
  return /\byes\b/i.test(txt);
}

// ── bench-aligned grounding oracle (the primary answer judge) ──────
// We do NOT hand-roll honesty/hallucination scoring — we call the SAME gold-
// free primitive the live grounding gate and the chaos-monkey scorer share
// (assess_asserted_value), via `sovereign bench chaos-monkey score-answer`.
// One definition of "is this asserted value grounded" across gate, bench, and
// this chaos oracle. Needs the retrieved EVIDENCE, so we first resolve the
// turn's chunks to full text.

// The message-complete payload carries only 200-char snippets; resolve each
// retrieved chunk to its FULL text via read_get_chunk so the grounding check
// sees the real evidence. Best-effort: falls back to the snippet on any miss.
async function resolveChunkTexts(chunks) {
  const texts = [];
  for (const c of (chunks ?? []).slice(0, 12)) {
    const corpusId = c?.corpus_id ?? c?.corpusId;
    const chunkId = c?.chunk_id ?? c?.chunkId;
    if (corpusId != null && chunkId != null) {
      try {
        const rec = await invoke("read_get_chunk", { corpusId, chunkId }, 15_000);
        const content = rec?.content ?? rec?.text;
        if (content) {
          texts.push(String(content));
          continue;
        }
      } catch {
        /* fall through to the snippet */
      }
    }
    if (c?.snippet) texts.push(String(c.snippet));
  }
  return texts;
}

// Score one (question, answer, chunks) with the bench's grounding primitive.
// Returns {verdict, asserted_value_grounded, answered, caveat_present, value}
// or null if the scorer is unavailable (itself worth knowing, not fatal).
//   verdict ∈ hallucination | grounded | caveated_ood | honest_abstention | answered_novalue
function scoreAnswerAligned(question, answer, chunkTexts) {
  return new Promise((resolve) => {
    let child;
    try {
      child = spawn(
        SCORE_CLI,
        ["bench", "chaos-monkey", "score-answer", "--base-url", DAEMON],
        { stdio: ["pipe", "pipe", "ignore"] },
      );
    } catch {
      return resolve(null);
    }
    let out = "";
    const timer = setTimeout(() => {
      try {
        child.kill("SIGKILL");
      } catch {
        /* already gone */
      }
      resolve(null);
    }, 90_000);
    child.stdout.on("data", (d) => (out += d));
    child.on("error", () => {
      clearTimeout(timer);
      resolve(null);
    });
    child.on("close", () => {
      clearTimeout(timer);
      const line = out.trim().split("\n").filter(Boolean).pop();
      try {
        resolve(line ? JSON.parse(line) : null);
      } catch {
        resolve(null);
      }
    });
    try {
      child.stdin.write(
        JSON.stringify({
          question: String(question ?? ""),
          answer: String(answer ?? ""),
          chunks: chunkTexts ?? [],
        }),
      );
      child.stdin.end();
    } catch {
      clearTimeout(timer);
      resolve(null);
    }
  });
}

// ── the challenging user's action vocabulary ──────────────────────
// REAL args, real workflows — the difficulty is intensity, order, and
// combination, NOT malformed data. Args come from seeded state + a rich
// real-question pool. `chat:true` marks turns whose ANSWER we capture
// and judge from the user's seat.
const CATALOG = [
  { cmd: "send_message_stream", chat: true, arg: () => ({ message: nextUserMessage(), conversationId: state.convo }) },
  { cmd: "cancel_stream", arg: () => ({ conversationId: state.convo }) },
  { cmd: "create_conversation", arg: () => ({}) },
  { cmd: "get_conversation", arg: () => ({ conversationId: pick(state.convos) ?? state.convo }) },
  { cmd: "list_conversations", arg: () => ({}) },
  { cmd: "rename_conversation", arg: () => ({ conversationId: state.convo, title: pick(USER_TITLES) }) },
  { cmd: "delete_conversation", arg: () => ({ conversationId: pick(state.convos) ?? state.convo }) },
  { cmd: "set_conversation_enabled_corpora", arg: () => ({ conversationId: state.convo, enabledCorpora: chance(0.3) ? [] : state.corpora.length ? [pick(state.corpora)] : [] }) },
  { cmd: "get_config", arg: () => ({}) },
  { cmd: "save_config", arg: () => ({ config: state.config ?? {} }) },
  { cmd: "get_storage_budget", arg: () => ({}) },
  { cmd: "read_get_chunk", arg: () => ({ corpusId: pick(state.corpora) ?? "", chunkId: 1 + Math.floor(rand() * 12) }) },
  { cmd: "read_get_chunk_neighbors", arg: () => ({ corpusId: pick(state.corpora) ?? "", chunkId: 1 + Math.floor(rand() * 12), radius: 1 + Math.floor(rand() * 3) }) },
  { cmd: "lc_list", arg: () => ({}) },
  { cmd: "lc_search", arg: () => ({ corpusId: pick(state.corpora) ?? "", query: pick(USER_QUESTIONS) }) },
  { cmd: "ask_document", chat: true, arg: () => ({ assetId: state.assetId ?? "", question: nextUserMessage(), conversationId: state.convo }) },
  { cmd: "get_document_asset", arg: () => ({ assetId: state.assetId ?? "" }) },
  { cmd: "atlas_list_atoms", arg: () => ({ corpusId: pick(state.corpora) ?? "" }) },
  { cmd: "atlas_subgraph", arg: () => ({ corpusId: pick(state.corpora) ?? "", atomId: "1" }) },
  { cmd: "search_messages", arg: () => ({ query: pick(USER_QUESTIONS) }) },
  { cmd: "mesh_get_state", arg: () => ({}) },
  { cmd: "get_runtime_status", arg: () => ({}) },
  { cmd: "list_daemon_models", arg: () => ({}) },
];
const CATALOG_BY_CMD = new Map(CATALOG.map((c) => [c.cmd, c]));

const SEED_MSGS = [
  "What is the Meridian Lighthouse?",
  "Tell me a long story about the sea.",
  "Count from 1 to 50.",
  "",
  "   ",
];
const CHAOS_STRINGS = [
  "",
  "   \n\t  ",
  "A".repeat(80_000),
  "🜂🌊⚓ ‮reverse‬ 日本語 שלום",
  "null byte[31m",
  "../../../../etc/passwd",
  "sovereign://join/" + "Z".repeat(500),
  "'; DROP TABLE conversations;--",
  "<script>alert(1)</script>",
  "{".repeat(300),
];
const CHAOS_PATHS = ["/", "/dev/null", "/etc/passwd", "", "~/".padEnd(4000, "x"), "/nonexistent-" + Math.floor(rand() * 1e9)];

// Real questions a demanding user asks of the seeded (lighthouse) base —
// factual, ambiguous, multi-part, contradictory, off-topic, sloppy,
// boundary-pushing. (The legacy SEED_MSGS / CHAOS_* pools above are unused
// now: the challenging user sends real inputs, not entropy.)
const USER_QUESTIONS = [
  "How tall is the Meridian Lighthouse?",
  "Who was Elowen Marsh and what was she known for?",
  "When was the lighthouse automated, and why?",
  "Compare the lamp mechanism before and after electrification.",
  "What is the light's characteristic signal?",
  "Summarize everything about the Tamarind rescue.",
  "Tell me about the keeper.",
  "Wait, wasn't it a different keeper who did the rescue?",
  "What is the capital of France?",
  "Explain the Fresnel lens in exhaustive technical detail, at least 2000 words.",
  "who what when where the light keeper rescue year",
];
const USER_TITLES = ["Lighthouse research", "keeper notes", "rescue 1912", "?", "untitled but important"];
const EDGE_MESSAGES = ["", "?", "Tell me about the lighthouse. ".repeat(900)];

// Mostly real questions; occasionally a realistic edge (fat-fingered empty
// submit, a pasted wall of text) or an impatient follow-up — a demanding
// human, not a fuzzer.
function nextUserMessage() {
  if (chance(0.15)) return pick(EDGE_MESSAGES);
  if (state.lastQuestion && chance(0.25)) return `About that — ${pick(USER_QUESTIONS)}`;
  const q = pick(USER_QUESTIONS);
  state.lastQuestion = q;
  return q;
}

// Attach mode asks about YOUR resident corpora, not the lighthouse fixture.
// The static pool would only ever draw honest "not in my sources" declines, so
// we GROUND each question in real content: sample a chunk from a random
// resident corpus and have the demanding-user brain ask a hard, specific
// question that passage answers. The question is therefore answerable from the
// corpus — so the oracle measures answer QUALITY (grounded? complete? coherent?
// graceful?), not merely whether the app declines an off-topic ask. Demanding-
// user edges (fat-fingered empty, impatient re-ask) are preserved.
async function attachQuestion() {
  if (chance(0.12)) return pick(EDGE_MESSAGES);
  if (state.lastQuestion && chance(0.2)) return `Wait — also: ${state.lastQuestion}`;
  const corpus = pick(state.corpora);
  if (!corpus) return nextUserMessage();
  // Record the scoped corpus so the CASE diagnostic can re-search it with
  // lc_search when the gated retrieval returns 0 — a SPECIFIC question is
  // answerable from this corpus by construction, so raw hits there prove the
  // gated path dropped an answerable chunk (recall bug) vs genuinely off-domain.
  state.scopedCorpus = corpus;
  // Scope the chat to the SOURCE corpus so the question is actually answerable
  // (focused retrieval, no cross-corpus dilution among 30+ corpora). This is
  // the complement to the unscoped finding (the app declines answerable Qs when
  // everything is in scope): scoped, the answer SHOULD land — so the oracle now
  // measures answer QUALITY (grounded? complete? coherent? graceful?) on Qs the
  // app ought to nail, which is where the deeper bugs live.
  // SOVEREIGN_CHAOS_NO_SCOPE=1 leaves the chat UNSCOPED (retrieval fans out
  // over all corpora) so we can measure the cross-corpus dilution path + the
  // KQ cap/floor fix against it. Default: scope to the source corpus.
  if (state.convo && !process.env.SOVEREIGN_CHAOS_NO_SCOPE) {
    await invoke(
      "set_conversation_enabled_corpora",
      { conversationId: state.convo, enabledCorpora: [corpus] },
      10_000,
    ).catch(() => {});
  }
  try {
    const rec = await invoke(
      "read_get_chunk",
      { corpusId: corpus, chunkId: 1 + Math.floor(rand() * 40) },
      15_000,
    );
    const passage = String(rec?.content ?? rec?.text ?? "").slice(0, 1200);
    if (passage && BRAIN_MODEL) {
      const q = await chatCompletion(
        [
          {
            role: "system",
            content:
              "You are a sharp, demanding power-user of a knowledge app. Given a passage from the user's own corpus, ask ONE hard, specific question that this passage answers — the kind that tests whether the app can find and synthesize the detail (a name, a number, a claim, a comparison). Reply with ONLY the question.",
          },
          { role: "user", content: `Passage:\n${passage}\n\nYour question:` },
        ],
        { temperature: 0.8, maxTokens: 60 },
      );
      const question = String(q ?? "")
        .trim()
        .replace(/^["']+|["']+$/g, "")
        .slice(0, 300);
      if (question) {
        // SOVEREIGN_CHAOS_FORCE_LONG=1 appends a long-answer directive to ~half
        // the questions so the run exercises the synthesis truncation path
        // (finish_reason=Length) — used to validate the answer-truncation fix.
        const q2 =
          process.env.SOVEREIGN_CHAOS_FORCE_LONG && chance(0.5)
            ? `${question} Answer in exhaustive, comprehensive detail — at least 1500 words.`
            : question;
        state.lastQuestion = q2;
        return q2;
      }
    }
  } catch {
    /* fall through to a generic exploratory ask */
  }
  const generic = `What is the most important thing in the "${corpus}" material, and why?`;
  state.lastQuestion = generic;
  return generic;
}

// Per-run mutable state. convos[] lets the user switch among / delete real
// prior conversations (coherent sessions, not orphaned ids).
const state = { convo: null, convos: [], corpora: [], config: null, budget: null, assetId: null, lastQuestion: null };

// ── mutation layer (raw entropy on top of the brain's direction) ───
function mutateArgs(args) {
  if (!args || typeof args !== "object") return args;
  if (!chance(0.55)) return args; // sometimes leave the baseline alone
  const out = Array.isArray(args) ? [...args] : { ...args };
  const keys = Object.keys(out);
  if (keys.length === 0) return out;
  const k = pick(keys);
  const dice = rand();
  if (dice < 0.4) out[k] = pick(CHAOS_STRINGS);
  else if (dice < 0.55) out[k] = null;
  else if (dice < 0.7) out[k] = Math.floor(rand() * 1e12);
  else if (dice < 0.8) out[k] = [];
  else if (dice < 0.9) delete out[k];
  else out[k] = { nested: pick(CHAOS_STRINGS) }; // type-confused
  return out;
}

// ── novelty memory + surprise oracle (no contract, just "huh?") ────
const seen = {
  commands: new Set(),
  eventTypes: new Set(),
  errorSigs: new Set(),
  logSigs: new Set(),
};
const latencyByCmd = new Map(); // cmd -> [ms,...]
let appLogPos = 0; // byte offset into APP_LOG already consumed
const verdicts = {}; // bench grounding-verdict tally (hallucination/grounded/…) for the closing summary

// Normalize a string into a signature: strip ids/uuids/numbers/hex so
// "the same kind of thing" collapses to one signature (novelty = a kind
// we've never seen, not a new uuid).
function sig(s) {
  return String(s)
    .replace(/[0-9a-f]{8}-[0-9a-f-]{27}/gi, "<uuid>")
    .replace(/0x[0-9a-f]+/gi, "<hex>")
    .replace(/\d+/g, "<n>")
    .slice(0, 200);
}

function newAppLogLines() {
  try {
    const stat = fs.statSync(APP_LOG);
    if (stat.size <= appLogPos) return [];
    const fd = fs.openSync(APP_LOG, "r");
    const buf = Buffer.alloc(stat.size - appLogPos);
    fs.readSync(fd, buf, 0, buf.length, appLogPos);
    fs.closeSync(fd);
    appLogPos = stat.size;
    return buf.toString("utf8").split("\n").filter(Boolean);
  } catch {
    return [];
  }
}

const median = (a) => {
  if (!a.length) return 0;
  const s = [...a].sort((x, y) => x - y);
  return s[Math.floor(s.length / 2)];
};

// Events a healthy app emits all the time — their first sighting is not a
// surprise (v0 wrongly flagged backend-ready / message-chunk as novel).
const KNOWN_NORMAL_EVENTS = new Set([
  "message-chunk",
  "message-complete",
  "message-error",
  "backend-ready",
  "backend-error",
  "supervisor-state",
  "conversations:changed",
]);
// A clean validation rejection is EXPECTED — a real user rarely trips it,
// and the surface correctly refusing junk is not a finding. Internal /
// unexpected errors are the interesting ones.
function looksLikeCleanValidation(error) {
  return /invalid (type|args)|missing required|expected (a |an |u\d|string|sequence|map|integer|boolean)|no atlas|invalid join link|not found|already exists/i.test(
    String(error),
  );
}

// Score a step from the USER'S seat. Highest weight goes to what a
// demanding user actually cares about: the app died, stalled, answered
// wrongly, or showed them a raw error. Clean rejections of edge input are
// cosmetic. Objective glassbox novelty (new WARN/ERROR) still counts.
function scoreSurprise({ cmd, ok, error, latencyMs, events, alive, answer, aligned, userJudge }) {
  const signals = [];
  let score = 0;

  if (!alive) {
    signals.push("APP DIED after this action");
    score = Math.max(score, 10);
  }
  if (latencyMs >= 55_000) {
    signals.push(`HANG — the user waited ${Math.round(latencyMs / 1000)}s`);
    score = Math.max(score, 9);
  }

  // PRIMARY answer oracle — the BENCH's shared grounding verdict
  // (assess_asserted_value): the SAME honesty/hallucination definition the live
  // gate and the chaos-monkey scorer use, not a hand-rolled judge. A
  // hallucination — a value asserted but absent from the evidence — is the
  // cardinal sin and scores near the top.
  if (aligned?.verdict === "hallucination") {
    signals.push(
      `HALLUCINATION (bench): asserted "${String(aligned.value ?? "").slice(0, 60)}" — absent from the evidence`,
    );
    score = Math.max(score, 9);
  }
  // UX layer — the bench owns honesty; this owns "was it a GOOD, gracious
  // answer?". An ABRASIVE honest decline is flagged DISTINCTLY from a wrong/
  // incoherent answer, so a GRACEFUL honest decline (correct behaviour) is
  // never penalised as a bug.
  if (userJudge && (userJudge.broken || userJudge.score >= 6)) {
    const honest = aligned?.verdict === "honest_abstention" || aligned?.verdict === "caveated_ood";
    if (honest) {
      signals.push(`abrasive honest decline (UX, not dishonest): ${userJudge.why}`);
      score = Math.max(score, 5);
    } else if (!aligned) {
      signals.push(`bad answer (user ${userJudge.score}/10, no bench verdict): ${userJudge.why}`);
      score = Math.max(score, Math.min(8, 3 + userJudge.score));
    } else {
      signals.push(`poor answer (bench: ${aligned.verdict}; user ${userJudge.score}/10): ${userJudge.why}`);
      score = Math.max(score, Math.min(7, 2 + userJudge.score));
    }
  }
  // The app handed the user a raw error or an empty reply to a real question.
  if (answer != null) {
    const a = answer.trim();
    if (/^error[:\s]/i.test(a)) {
      signals.push(`raw error shown to user: ${a.slice(0, 80)}`);
      score = Math.max(score, 6);
    } else if (a.length === 0) {
      signals.push("empty answer to a real question");
      score = Math.max(score, 6);
    }
  }

  // Command error: clean validation = cosmetic; internal/unexpected = a lead.
  if (error) {
    const es = `${cmd}:${sig(error)}`;
    if (!seen.errorSigs.has(es)) {
      seen.errorSigs.add(es);
      if (looksLikeCleanValidation(error)) {
        signals.push(`clean rejection: ${sig(error).slice(0, 70)}`);
        score = Math.max(score, 1);
      } else {
        signals.push(`unexpected error: ${sig(error).slice(0, 100)}`);
        score = Math.max(score, 5);
      }
    }
  }

  // Novel ERROR/WARN log — the glassbox narrating something new.
  for (const line of events.newLogs ?? []) {
    if (!/\bERROR\b|\bWARN\b|panic|assertion|unwrap/i.test(line)) continue;
    const ls = sig(line);
    if (seen.logSigs.has(ls)) continue;
    seen.logSigs.add(ls);
    const sev = /ERROR|panic/i.test(line) ? 8 : 6;
    signals.push(`new ${sev >= 8 ? "ERROR" : "WARN"} log: ${ls.slice(0, 110)}`);
    score = Math.max(score, sev);
  }

  // Novel NON-normal event type (normal events are not surprising).
  for (const ev of events.types ?? []) {
    if (KNOWN_NORMAL_EVENTS.has(ev) || seen.eventTypes.has(ev)) {
      seen.eventTypes.add(ev);
      continue;
    }
    seen.eventTypes.add(ev);
    signals.push(`unusual event: ${ev}`);
    score = Math.max(score, 3);
  }

  // Latency outlier vs this command's own history.
  const hist = latencyByCmd.get(cmd) ?? [];
  if (hist.length >= 5) {
    const med = median(hist);
    if (med > 0 && latencyMs > med * 6 && latencyMs > 3000) {
      signals.push(`latency outlier: ${latencyMs}ms vs ~${med}ms median`);
      score = Math.max(score, 4);
    }
  }
  hist.push(latencyMs);
  latencyByCmd.set(cmd, hist);

  return { score, signals };
}

function record(row) {
  fs.appendFileSync(JOURNAL, `${JSON.stringify(row)}\n`);
}

// ── production-like seeding (furnish the app before the wander) ────
// A real corpus + a document asset turn the sparse 0-index world into
// something the agent can actually explore: retrieval, reading, search,
// citations, the document surface. The atoms-atlas is heavier (LLM
// build) so it's best-effort. Each step degrades gracefully — a furnished
// room beats an empty one, and partial furnishing beats none.
const FIXTURE_DISPLAY = "Chaos Fixture Corpus";
const FIXTURE_DIR = path.resolve(__dirname, "../real/fixtures/corpus");

async function seedFixtureCorpus() {
  const existing = await invoke("lc_list", {});
  const found = existing.find((c) => c.display_name === FIXTURE_DISPLAY);
  if (found) {
    console.log(`[chaos] corpus already present (${found.corpus_id ?? found.id})`);
    return found.corpus_id ?? found.id;
  }
  const val = await invoke("lc_validate_path", { path: FIXTURE_DIR });
  if (!val.exists || !val.is_dir) throw new Error(`fixture dir invalid: ${FIXTURE_DIR}`);
  const pre = await invoke(
    "lc_pre_scan",
    { path: FIXTURE_DIR, sourceType: "folder", displayName: FIXTURE_DISPLAY },
    60_000,
  );
  const jobId = await invoke("lc_ingest", { corpusId: pre.corpus_id, withOcr: false }, 60_000);
  // Register the dynamic per-job channel BEFORE polling so a fast ingest
  // can't outrun us (rows land in the replay ring).
  const channel = `local-corpus://progress/${jobId}`;
  await listen(channel);
  const deadline = Date.now() + 180_000;
  for (;;) {
    const rows = (await recent(0)).filter((r) => r.event === channel);
    const terminal = rows.map((r) => JSON.stringify(r.payload)).find((p) => /complete|error/i.test(p));
    if (terminal) {
      if (/error/i.test(terminal)) throw new Error(`ingest failed: ${terminal.slice(0, 160)}`);
      break;
    }
    if (Date.now() > deadline) throw new Error("ingest never reached terminal in 180s");
    await new Promise((r) => setTimeout(r, 1500));
  }
  console.log(`[chaos] corpus seeded ✓ (${pre.corpus_id}) — structural enrichment auto-runs in bg`);
  return pre.corpus_id;
}

async function seedDocumentAsset() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "chaos-doc-"));
  const file = path.join(dir, "expedition-log.txt");
  fs.writeFileSync(
    file,
    "Expedition log: the keeper recorded 412 vessels and 9 storms this season. " +
      "The west pier light burns 4 lamps; the east beacon was relit on the third night.\n",
  );
  const up = await invoke("upload_document_asset", { filePath: file }, 60_000);
  const assetId = up.asset?.id ?? up.id;
  const deadline = Date.now() + 180_000;
  for (;;) {
    const asset = await invoke("get_document_asset", { assetId });
    const st = JSON.stringify(asset.state ?? asset).toLowerCase();
    if (st.includes("ready")) break;
    if (st.includes("failed")) throw new Error(`doc ingest failed: ${st.slice(0, 120)}`);
    if (Date.now() > deadline) throw new Error("doc never reached ready in 180s");
    await new Promise((r) => setTimeout(r, 3000));
  }
  state.assetId = assetId;
  console.log(`[chaos] document asset seeded ✓ (${assetId})`);
}

async function seedAtlas(corpusId) {
  // Build the atoms atlas (literary_atlas — lighthouse fixture is narrative)
  // so atlas_* surfaces are live, not "no atlas". LLM-heavy + needs staged
  // docs; fully best-effort and time-capped.
  await invoke("enrich_init_for_local_corpus", { corpusId, pipelineId: "literary_atlas" }, 120_000);
  const h = await invoke("enrich_build_async", { corpusId }, 30_000);
  console.log(`[chaos] atlas build kicked off (${JSON.stringify(h).slice(0, 80)}) — polling…`);
  const deadline = Date.now() + 240_000;
  for (;;) {
    const st = await invoke("lc_enrichment_status", { corpusId }).catch(() => null);
    const s = JSON.stringify(st ?? "").toLowerCase();
    if (/ready|complete|done|finished/.test(s)) {
      console.log("[chaos] atlas built ✓ — atlas_* surfaces are live");
      return;
    }
    if (Date.now() > deadline) throw new Error("not ready in 240s — proceeding without atoms");
    await new Promise((r) => setTimeout(r, 5000));
  }
}

async function seedProductionLikeState() {
  let corpusId = null;
  try {
    corpusId = await seedFixtureCorpus();
  } catch (e) {
    console.log(`[chaos] corpus seed failed (wandering corpus-less): ${e}`);
  }
  try {
    await seedDocumentAsset();
  } catch (e) {
    console.log(`[chaos] document seed failed (no document surface): ${e}`);
  }
  if (corpusId) {
    try {
      await seedAtlas(corpusId);
    } catch (e) {
      console.log(`[chaos] atlas seed skipped (${e}) — atlas_* will be sparse`);
    }
  }
  await refreshState();
  console.log(
    `[chaos] furnished: ${state.corpora.length} corpus(es), document=${state.assetId ? "yes" : "no"}`,
  );
}

// ── refresh per-run state so the catalog's baseline args are plausible ──
async function refreshState() {
  try {
    state.convo = (await invoke("create_conversation", {})).id;
  } catch {
    /* leave prior */
  }
  try {
    const lc = await invoke("lc_list", {}).catch(() => []);
    const local = (lc ?? []).map((c) => c.corpus_id ?? c.id).filter(Boolean);
    // Installed corpora (the resident ones surfaced in attach mode) come from
    // list_corpora — lc_list only covers local-folder ingests. Keep only
    // status="installed" (the catalog also returns not_installed built-ins).
    const all = await invoke("list_corpora", {}).catch(() => []);
    const installed = (all ?? [])
      .filter((c) => c.status === "installed")
      .map((c) => c.id)
      .filter(Boolean);
    state.corpora = [...new Set([...local, ...installed])];
  } catch {
    /* none */
  }
  try {
    state.config = await invoke("get_config", {});
  } catch {
    /* none */
  }
  try {
    state.budget = await invoke("get_storage_budget", {});
  } catch {
    /* none */
  }
}

// Wait for a chat turn's terminal and return {answer, chunks}: the answer text
// (message-complete.full_text) plus the retrieved evidence the runtime surfaced
// (metadata.retrieved_chunks, each carrying chunk_id + corpus_id). null on
// error / hang. The grounding oracle needs the evidence, not just the answer.
async function awaitChatAnswer(sinceSeq, messageId, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const rows = await recent(sinceSeq).catch(() => []);
    const done = rows.find(
      (r) => r.event === "message-complete" && r.payload?.message_id === messageId,
    );
    if (done) {
      const rc = done.payload?.metadata?.retrieved_chunks;
      return {
        answer: String(done.payload?.full_text ?? ""),
        chunks: Array.isArray(rc) ? rc : [],
      };
    }
    if (rows.some((r) => r.event === "message-error")) return null; // errored
    if (Date.now() > deadline) return null; // hang — latency catches it
    await new Promise((r) => setTimeout(r, 1500));
  }
}

// ── one user move: decide → act → (if chat) read+judge the answer → score ──
let step = 0;
let movesSinceChat = 0;
async function chaosStep(memorySummary) {
  step += 1;
  let goal = null;
  let chosen = null;
  // A demanding user asks questions often (the primary activity, and the
  // only thing that exercises the answer-judge) but also works the other
  // features. Bias MODERATELY toward chat, with a cadence floor so the judge
  // still fires when the brain fixates on a feature thread (v1 did 1 chat
  // turn in 162 moves — that was the gap).
  if (chance(0.28) || movesSinceChat >= 6) {
    const message = ATTACH ? await attachQuestion() : nextUserMessage();
    chosen = { cmd: "send_message_stream", args: { message, conversationId: state.convo } };
    goal = "ask my knowledge base a question";
  } else {
    const proposal = await brainPropose(CATALOG, memorySummary);
    if (proposal && CATALOG_BY_CMD.has(proposal.actions[0].cmd)) {
      goal = proposal.goal ?? null;
      const a = proposal.actions[0];
      chosen = { cmd: a.cmd, args: a.args && Object.keys(a.args).length ? a.args : CATALOG_BY_CMD.get(a.cmd).arg() };
    } else {
      const c = pick(CATALOG);
      chosen = { cmd: c.cmd, args: c.arg() };
      goal = proposal ? "(brain named an unknown command; spontaneous action)" : "(spontaneous user action)";
    }
  }
  // No garbage mutation — a real user sends real inputs.
  seen.commands.add(chosen.cmd);
  const isChat = !!CATALOG_BY_CMD.get(chosen.cmd)?.chat;
  if (isChat) movesSinceChat = 0;
  else movesSinceChat += 1;

  const since = await lastSeq();
  const t0 = Date.now();
  let ok = true;
  let error = null;
  let result = null;
  try {
    result = await invoke(chosen.cmd, chosen.args, 60_000);
  } catch (e) {
    ok = false;
    error = e.structured ? JSON.stringify(e.structured) : String(e);
  }

  // Keep the session coherent: track conversations the user opens.
  if (chosen.cmd === "create_conversation" && result?.id) {
    state.convo = result.id;
    state.convos.push(result.id);
    if (state.convos.length > 30) state.convos.shift();
  }

  // For a chat turn: capture the answer + its retrieved evidence, then run
  // BOTH oracles — the bench's grounding verdict (primary; honesty/
  // hallucination) and the UX/tone judge (secondary; coherence + grace).
  let answer = null;
  let aligned = null; // bench grounding verdict — the primary answer oracle
  let userJudge = null; // UX layer: coherence/completeness + graceful-vs-abrasive
  // Evidence observability: how many chunks the runtime surfaced (retrieved),
  // how many we resolved to full text (resolved), and the total evidence size.
  // This separates a REAL fabrication (asserted value absent from PRESENT
  // evidence) from a measurement artifact (oracle scored against EMPTY evidence
  // because retrieval returned nothing, or chunk resolution failed). Without it
  // the hallucination count conflates the two and the loop signal is untrustworthy.
  let evidence = null;
  if (isChat && ok && result?.message_id) {
    const got = await awaitChatAnswer(since, result.message_id, 120_000);
    if (got && got.answer && got.answer.length > 0) {
      answer = got.answer;
      const question = chosen.args.message ?? chosen.args.question;
      const chunkTexts = await resolveChunkTexts(got.chunks);
      evidence = {
        retrieved: got.chunks.length,
        resolved: chunkTexts.length,
        chars: chunkTexts.reduce((n, t) => n + t.length, 0),
      };
      aligned = await scoreAnswerAligned(question, answer, chunkTexts);
      userJudge = await judgeAsUser(question, answer);
      if (aligned?.verdict) verdicts[aligned.verdict] = (verdicts[aligned.verdict] ?? 0) + 1;
      // CASE-1-vs-CASE-2 split: only for grounded answers quote-first did NOT
      // ground (no citation marker) — that legacy-path residual is what we're
      // splitting into "answer was retrievable" vs "retrieval gap".
      if (DIAG_CASE && got.chunks.length > 0 && !/Grounded in the source/.test(answer)) {
        evidence.answerInChunks = await answerInEvidence(question, chunkTexts);
      } else if (DIAG_CASE && got.chunks.length === 0 && state.scopedCorpus) {
        // CASE-2 confirm: the gated KQ path returned 0 — does a RAW lc_search on
        // the scoped corpus find chunks it dropped? Hits => recall bug (the answer
        // was retrievable, the gate filtered it). 0 hits => genuinely off-domain.
        const hits = await invoke(
          "lc_search",
          { corpusId: state.scopedCorpus, query: question },
          15_000,
        ).catch(() => []);
        evidence.lcHits = Array.isArray(hits) ? hits.length : null;
      }
    }
  }

  const latencyMs = Date.now() - t0; // includes the answer wait — a stall shows here
  await new Promise((r) => setTimeout(r, 400));
  const rows = await recent(since).catch(() => []);
  const events = {
    types: [...new Set(rows.map((r) => r.event))],
    count: rows.length,
    newLogs: newAppLogLines(),
  };
  const alive = await healthz();

  const surprise = scoreSurprise({
    cmd: chosen.cmd,
    ok,
    error,
    latencyMs,
    events,
    alive,
    answer,
    aligned,
    userJudge,
  });

  const row = {
    ts: Date.now(),
    step,
    goal,
    cmd: chosen.cmd,
    args: JSON.stringify(chosen.args ?? {}).slice(0, 200),
    ok,
    error: error ? error.slice(0, 240) : null,
    answer: answer != null ? answer.slice(0, 200) : null,
    // Full length + the last 80 chars: the journal stores only a 200-char head,
    // so a real mid-sentence cut-off (early stream-end, no finish_reason=Length)
    // is invisible without seeing how the answer actually ENDS. A complete answer
    // ends on terminal punctuation; a cut-off ends mid-word/phrase.
    answerLen: answer != null ? answer.length : null,
    answerTail: answer != null && answer.length > 200 ? answer.slice(-80) : null,
    aligned: aligned
      ? { verdict: aligned.verdict, value: aligned.value ?? null, grounded: aligned.asserted_value_grounded ?? null }
      : null,
    evidence,
    userJudge,
    latencyMs,
    surprise: surprise.score,
    signals: surprise.signals,
    alive,
  };
  record(row);

  if (surprise.score >= 4) {
    console.log(
      `  ⁇ [${surprise.score}] step ${step} ${chosen.cmd} — ${surprise.signals.join("; ").slice(0, 170)}`,
    );
  } else {
    console.log(`[chaos] step ${step} ${chosen.cmd} (${latencyMs}ms${ok ? "" : " ✗"})`);
  }
  return { row, alive };
}

// Build the rolling memory the brain reads — recent moves + the loudest
// surprises so far, so it can chase a thread and avoid repeating itself.
const recentMoves = [];
const loudest = [];
function memorySummary() {
  const moves = recentMoves.slice(-8).map((m) => `- ${m.cmd}${m.ok ? "" : " (errored)"}`).join("\n");
  const surprises = loudest.slice(0, 5).map((s) => `! ${s.cmd}: ${s.signals[0] ?? ""}`).join("\n");
  const untried = CATALOG.map((c) => c.cmd).filter((c) => !seen.commands.has(c)).slice(0, 10);
  return (
    `Recent moves:\n${moves || "(none yet)"}\n\n` +
    `Surprising so far:\n${surprises || "(nothing yet)"}\n\n` +
    `Commands you HAVEN'T tried: ${untried.join(", ") || "(all tried)"}`
  );
}

// ── spawn lifecycle (supervised hermetic; mirrors soak.mjs) ────────
function bakeProfile(home) {
  // dirs::config_dir(): XDG ~/.config on Linux, ~/Library/Application
  // Support on macOS — bake both (see soak.mjs / global-setup.ts).
  const configDirs = [
    path.join(home, ".config/sovereign"),
    path.join(home, "Library/Application Support/sovereign"),
  ];
  for (const d of configDirs) fs.mkdirSync(d, { recursive: true });
  fs.mkdirSync(path.join(home, ".local/share"), { recursive: true });
  fs.mkdirSync(path.join(home, ".cache"), { recursive: true });
  const sovDir = path.join(home, ".sovereign");
  fs.mkdirSync(sovDir, { recursive: true });
  if (!fs.existsSync(path.join(sovDir, "models")))
    fs.symlinkSync(MODELS_DIR, path.join(sovDir, "models"));
  const desktopToml = [
    `# Generated by tests/e2e/scripts/chaos.mjs`,
    `model_path = ${JSON.stringify(CHAT_MODEL)}`,
    `primary_model_path = ${JSON.stringify(CHAT_MODEL)}`,
    `embed_model_path = ${JSON.stringify(EMBED_MODEL)}`,
    `setup_complete = true`,
    `auto_collaborate = false`,
    ``,
  ].join("\n");
  for (const d of configDirs) fs.writeFileSync(path.join(d, "desktop.toml"), desktopToml);
  if (SUPERVISOR) {
    fs.writeFileSync(
      path.join(sovDir, "config.toml"),
      [
        `[models]`,
        `primary = ${JSON.stringify(CHAT_MODEL)}`,
        `fast = ${JSON.stringify(CHAT_MODEL)}`,
        `embed = ${JSON.stringify(EMBED_MODEL)}`,
        `context_size = 8192`,
        ``,
        `[daemon]`,
        `client_port = 9741`,
        ``,
      ].join("\n"),
    );
  } else if (ATTACH) {
    // Attach to the EXISTING dev daemon on :9741. build_attach_provider does
    // `SetupConfig::load()?`, and a minimal hand-written [daemon] stanza fails
    // the SetupConfig deserialize (missing required fields surface as a "TOML
    // parse error at line 1"). Copy the daemon's OWN config verbatim: it
    // parses, carries client_port=9741, and its model paths are inert in Attach
    // mode (no local weights load). The desktop only READS it (save_config
    // writes desktop.toml, not this), so the real config is never touched.
    const realConfig = path.join(os.homedir(), ".sovereign", "config.toml");
    if (!fs.existsSync(realConfig))
      throw new Error(`--attach needs ${realConfig} (the daemon's SetupConfig) to resolve the daemon port`);
    fs.copyFileSync(realConfig, path.join(sovDir, "config.toml"));
    // Surface the RESIDENT corpora. The desktop's corpus_engine reads
    // dirs::home_dir()/.sovereign/{indexes,recipes} (state.rs:601-603) — under
    // our scratch HOME those are empty, so attach showed 0 corpora. Symlink
    // them to the REAL ones so the wander explores your actual installed
    // corpora. READ-ONLY in practice: the attach catalog only lists/searches/
    // reads chunks + atoms — it never ingests, installs, deletes, or enriches —
    // so the real indexes are never mutated.
    const realSov = path.join(os.homedir(), ".sovereign");
    for (const sub of ["indexes", "recipes"]) {
      const target = path.join(realSov, sub);
      const link = path.join(sovDir, sub);
      if (fs.existsSync(target) && !fs.existsSync(link)) fs.symlinkSync(target, link);
    }
  }
  return home;
}

let spawnedPid = null;
async function maybeSpawn() {
  if (await healthz()) return null;
  if (!SPAWN) throw new Error(`bridge not reachable at ${BRIDGE}. Pass --spawn or launch a desktop.`);
  if (SUPERVISOR && (await portInUse(9741)))
    throw new Error("supervised chaos needs :9741 free — `sovereign daemon stop` (or --no-supervisor).");
  if (ATTACH && !(await portInUse(9741)))
    throw new Error(
      "--attach needs your dev daemon on :9741 (the resident corpora + loaded model). Start it: `sovereign daemon start`.",
    );
  const profileDir = path.join(ARTIFACTS, "chaos-profile");
  fs.rmSync(profileDir, { recursive: true, force: true });
  const home = bakeProfile(path.join(profileDir, "home"));
  const log = fs.openSync(APP_LOG, "w");
  const env = {
    ...process.env,
    HOME: home,
    XDG_CONFIG_HOME: path.join(home, ".config"),
    XDG_DATA_HOME: path.join(home, ".local/share"),
    XDG_CACHE_HOME: path.join(home, ".cache"),
    SOVEREIGN_COMMAND_BRIDGE: "1",
    SOVEREIGN_COMMAND_BRIDGE_PORT: String(BRIDGE_PORT),
    SOVEREIGN_COMMAND_BRIDGE_LEDGER: path.join(ARTIFACTS, "ledger-chaos.jsonl"),
    // Crank the glassbox: trace-level is the agent's richest eye.
    RUST_LOG: process.env.RUST_LOG ?? "sovereign_desktop=debug,sovereign_core=debug,sovereign_inference=info",
  };
  if (SUPERVISOR) {
    env.SOVEREIGN_USE_SUPERVISOR = "1";
    env.SOVEREIGN_CLI_PATH = CLI_BIN;
  }
  const child = spawn(APP_BIN, [], { env, cwd: os.homedir(), stdio: ["ignore", log, log], detached: true });
  child.unref();
  spawnedPid = child.pid;
  const deadline = Date.now() + 240_000;
  while (!(await healthz())) {
    if (Date.now() > deadline) throw new Error("spawned desktop never came up");
    await new Promise((r) => setTimeout(r, 2000));
  }
  return child.pid;
}
async function killGroup() {
  if (!spawnedPid) return;
  const grp = (sig) => {
    try {
      process.kill(-spawnedPid, sig);
      return true;
    } catch {
      return false;
    }
  };
  if (!grp("SIGTERM")) return;
  const deadline = Date.now() + 8000;
  while (Date.now() < deadline && grp(0)) await new Promise((r) => setTimeout(r, 500));
  grp("SIGKILL");
}
for (const s of ["SIGINT", "SIGTERM"]) process.on(s, () => void killGroup().finally(() => process.exit(130)));

// ── main wander ───────────────────────────────────────────────────
async function main() {
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  fs.rmSync(JOURNAL, { force: true });
  await maybeSpawn();
  // Gate on backend-ready (sticky replay) so the first moves aren't all
  // "backend loading" noise.
  {
    const deadline = Date.now() + 240_000;
    for (;;) {
      const r = await fetch(`${BRIDGE}/listen`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ event: "backend-ready" }),
      }).then((x) => x.json()).catch(() => ({}));
      if (r.replayed) break;
      if (Date.now() > deadline) throw new Error("backend-ready never fired");
      await new Promise((r) => setTimeout(r, 2000));
    }
  }
  // Subscribe broadly so events land in the replay ring for our eyes.
  for (const ev of ["message-chunk", "message-complete", "message-error", "supervisor-state", "backend-error"])
    await listen(ev);
  newAppLogLines(); // prime the log offset past boot

  await discoverBrainModel();
  if (ATTACH) {
    // Resident corpora come from the attached dev daemon — do NOT seed (that
    // would graft a fixture onto your real setup). Just read what is there.
    await refreshState();
    const names = state.corpora.slice(0, 8).join(", ");
    console.log(
      `[chaos] ATTACH — ${state.corpora.length} resident corpus(es): ${names}${state.corpora.length > 8 ? " …" : ""}`,
    );
    if (state.corpora.length === 0)
      console.log(
        "[chaos] ⚠ attached daemon exposed NO corpora — the wander will be answer-thin (is lc_list daemon-proxied in attach mode?)",
      );
  } else {
    await seedProductionLikeState();
  }

  console.log(
    `[chaos] wandering for ${MINUTES}min — seedless, no assertions. brain=${BRAIN_MODEL ?? "random"} ` +
      `bridge=${BRIDGE} journal=${JOURNAL}`,
  );
  record({ ts: Date.now(), kind: "chaos_start", minutes: MINUTES, brain: BRAIN_MODEL });

  const endAt = Date.now() + MINUTES * 60_000;
  const scoredCount = () => Object.values(verdicts).reduce((a, b) => a + b, 0);
  let consecutiveDead = 0;
  while (Date.now() < endAt && (CHATS === 0 || scoredCount() < CHATS)) {
    let res;
    try {
      res = await chaosStep(memorySummary());
    } catch (e) {
      // The agent itself stumbled — record it, keep wandering.
      record({ ts: Date.now(), step, kind: "agent_stumble", error: String(e).slice(0, 200) });
      res = { alive: await healthz() };
    }
    if (res.row) {
      recentMoves.push({ cmd: res.row.cmd, ok: res.row.ok });
      if (res.row.surprise >= 4) {
        loudest.push(res.row);
        loudest.sort((a, b) => b.surprise - a.surprise);
        loudest.length = Math.min(loudest.length, 20);
      }
    }
    // If the app died, note it loudly and try to let the supervisor heal;
    // a persistent death ends the wander (the biggest finding there is).
    if (!res.alive) {
      consecutiveDead += 1;
      console.log(`[chaos] ⁇ bridge unreachable (${consecutiveDead}) — app may have died`);
      if (consecutiveDead >= 3) {
        record({ ts: Date.now(), step, kind: "app_down", note: "bridge unreachable 3x — ending wander" });
        break;
      }
      await new Promise((r) => setTimeout(r, 5000));
    } else {
      consecutiveDead = 0;
      // Occasionally refresh state + mint a fresh conversation so we don't
      // get stuck operating on a deleted/poisoned one.
      if (chance(0.15)) await refreshState();
    }
    await new Promise((r) => setTimeout(r, 250 + rand() * 900));
  }

  record({ ts: Date.now(), kind: "chaos_end", steps: step, verdicts });

  // Courtesy in --attach: the wander minted conversations; remove the ones we
  // created so we leave no litter in your real session. (They live in the
  // scratch desktop store, but this is belt-and-suspenders against any
  // daemon-side persistence — and it runs while the app is still up.)
  if (ATTACH && state.convos.length) {
    let cleaned = 0;
    for (const id of state.convos) {
      try {
        await invoke("delete_conversation", { conversationId: id }, 10_000);
        cleaned += 1;
      } catch {
        /* best-effort */
      }
    }
    console.log(`[chaos] attach cleanup: removed ${cleaned}/${state.convos.length} conversations we created`);
  }

  killGroup();

  // ── field journal: where the app let the user down, worst first ──
  loudest.sort((a, b) => b.surprise - a.surprise);
  console.log(`\n══ chaos field journal ══  (${step} moves as a relentless user, seedless)`);
  if (loudest.length === 0) {
    console.log(
      "the app held up — nothing disappointed the user this session. Try a longer wander or a bigger brain.",
    );
  } else {
    console.log(`${loudest.length} moment(s) the app let the user down — worst first:\n`);
    for (const r of loudest.slice(0, 12)) {
      console.log(`⁇ [${r.surprise}] ${r.cmd}  ${r.args}`);
      for (const s of r.signals) console.log(`    · ${s}`);
      if (r.goal) console.log(`    user was trying to: ${r.goal}`);
      if (r.aligned?.verdict)
        console.log(
          `    bench verdict: ${r.aligned.verdict}${r.aligned.value ? ` (value: "${String(r.aligned.value).slice(0, 50)}")` : ""}`,
        );
      if (r.answer) console.log(`    app said: ${String(r.answer).slice(0, 120)}`);
      console.log(`    → the question: why? (step ${r.step}, journal: ${JOURNAL})`);
    }
  }
  console.log(
    `\ncoverage this wander: ${seen.commands.size}/${CATALOG.length} commands touched, ` +
      `${seen.eventTypes.size} event types, ${seen.logSigs.size} distinct ERROR/WARN log shapes.`,
  );

  // The bench-aligned answer ledger — the same grounding vocabulary as the
  // chaos-monkey scorer, so a wander's honesty profile is comparable to a
  // bench run's. Hallucination is called out as the cardinal sin.
  const vsum = Object.entries(verdicts).sort((a, b) => b[1] - a[1]);
  if (vsum.length) {
    const total = vsum.reduce((s, [, n]) => s + n, 0);
    console.log(
      `\nbench grounding verdicts over ${total} judged answer(s): ${vsum
        .map(([k, n]) => `${k}=${n}`)
        .join("  ")}`,
    );
    const hall = verdicts.hallucination ?? 0;
    if (hall > 0)
      console.log(
        `  ⚠ ${hall} HALLUCINATION(s) — the cardinal sin: a value asserted but absent from the evidence.`,
      );
  } else {
    console.log("\nbench grounding verdicts: (no chat answers judged — was the scorer reachable?)");
  }
}

main().catch((e) => {
  console.error(`[chaos] fatal: ${e}`);
  void killGroup().finally(() => process.exit(1));
});
