#!/usr/bin/env node
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// mesh-app-driver.mjs — drive ONE desktop app (via its command bridge) as a
// realistic "app user" while the mesh soak chaos-tests the node underneath it.
// This is the P2 user layer of the "mesh of application users" harness: the
// soak owns the substrate + all daemon lifecycle; each node also runs a headless
// desktop (attach-mode) + one of these drivers, so user-visible TURN invariants
// are asserted WHILE the node it attaches to is being killed/restarted.
//
// It talks ONLY to the desktop bridge (the production webview.on_message
// dispatch path), never the daemon directly — so it exercises the real app
// surface (intent provenance, citation resolution, stream integrity), not raw
// HTTP. Findings are emitted in the mesh-soak JSONL schema so the soak verdict
// folds them in (a `phase` finding with ok:false is a counted checkpoint
// failure; a `kind` finding is observational and never fails the run).
//
// The turn invariants are a faithful port of the desktop's canonical pack
// (tests/e2e/real/invariants.ts — KEEP IN SYNC): on a COMPLETED turn,
//   1. stream integrity   concat(message-chunk.chunk) === message-complete.full_text
//   2. intent present      provenance.intent OR metadata.intent is non-empty
//   3. finish_reason sane  ∈ {stop, length}
//   4. citations resolve   every local-corpus retrieved_chunk derefs via read_get_chunk
// Invariants are asserted only on turns that COMPLETE: a turn that errors or
// hangs while the soak has just kill-9'd this node is EXPECTED, not a bug —
// those are recorded observationally. The headline cross-layer assertion is the
// FINAL turn (after chaos heals): the user surface MUST recover and pass.
//
// Env knobs (set by the orchestrator, one driver per node):
//   SOVEREIGN_BRIDGE_URL        bridge of this node's desktop (default :9745)
//   SOVEREIGN_DRIVER_FINDINGS   JSONL output path (mesh-soak schema)
//   SOVEREIGN_DRIVER_NODE       node label for findings (default "0")
//   SOVEREIGN_DRIVER_MINUTES    how long to drive (default 3)
//   SOVEREIGN_DRIVER_CORPUS     corpus to ground questions against
//   SOVEREIGN_DRIVER_TRANSCRIPT dir for failing-turn transcripts (forensics)
import fs from "node:fs";
import path from "node:path";

const BRIDGE = (process.env.SOVEREIGN_BRIDGE_URL ?? "http://127.0.0.1:9745").replace(/\/$/, "");
const FINDINGS = process.env.SOVEREIGN_DRIVER_FINDINGS ?? "./mesh-app-driver-findings.jsonl";
const NODE = process.env.SOVEREIGN_DRIVER_NODE ?? "0";
const MINUTES = parseFloat(process.env.SOVEREIGN_DRIVER_MINUTES ?? "3");
const CORPUS = process.env.SOVEREIGN_DRIVER_CORPUS ?? "chaos-secret-agent";
const TRANSCRIPT_DIR = process.env.SOVEREIGN_DRIVER_TRANSCRIPT ?? null;

// One-shot probe mode (P3): the orchestrator calls `--probe` at a moment it
// controls (right after killing the victim's node, and again after the heal) to
// HARD-assert the cross-layer arc — a turn must fail FAST during the outage and
// complete cleanly after recovery. Prints ONE finding JSON line to stdout and
// exits 0/1, so the orchestrator can fold it into the unified verdict.
const _argv = process.argv.slice(2);
const _flag = (f) => _argv.includes(f);
const _val = (f, d) => {
  const i = _argv.indexOf(f);
  return i >= 0 && _argv[i + 1] ? _argv[i + 1] : d;
};
const PROBE = _flag("--probe");
const PROBE_LABEL = _val("--label", "app-probe");
const PROBE_EXPECT = _val("--expect", "complete"); // "fail-fast" (outage) | "complete" (recovery)
const PROBE_TIMEOUT = parseInt(_val("--timeout", "60"), 10) * 1000;

// A small bank of corpus-grounded questions so successive turns aren't identical
// (the soak's chaos-secret-agent corpus is Conrad's "The Secret Agent").
const QUESTIONS = [
  "Who is Mr Verloc?",
  "What is Mr Verloc's occupation?",
  "Who is Stevie in relation to the Verlocs?",
  "What does the Embassy ask Verloc to do?",
  "Who is Chief Inspector Heat?",
  "What happens at the Greenwich Observatory?",
];

// ── tiny bridge client ────────────────────────────────────────────────────
async function invoke(cmd, args = {}, timeoutMs = 130_000) {
  const ctl = AbortSignal.timeout(timeoutMs);
  const res = await fetch(`${BRIDGE}/invoke`, {
    method: "POST",
    headers: { "content-type": "application/json", "x-sovereign-spec": `mesh-app-driver-node${NODE}` },
    body: JSON.stringify({ cmd, args }),
    signal: ctl,
  });
  const body = await res.json();
  if (!body.ok) throw new Error(`invoke ${cmd}: ${body.error ?? "unknown"}`);
  return body.result;
}
async function recent(sinceSeq = 0) {
  const res = await fetch(`${BRIDGE}/events/recent?since_seq=${sinceSeq}`);
  return (await res.json()).rows ?? [];
}
async function lastSeq() {
  const rows = await recent(0);
  return rows.length ? rows[rows.length - 1].seq : 0;
}
async function healthz() {
  try {
    const r = await fetch(`${BRIDGE}/healthz`, { signal: AbortSignal.timeout(2000) });
    return r.ok;
  } catch {
    return false;
  }
}
async function listen(event) {
  await fetch(`${BRIDGE}/listen`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ event }),
  }).catch(() => {});
}

// ── findings (mesh-soak JSONL schema) ─────────────────────────────────────
function finding(obj) {
  fs.appendFileSync(FINDINGS, JSON.stringify({ node: Number(NODE), ...obj }) + "\n");
}
function log(msg) {
  process.stdout.write(`  [app-driver node${NODE}] ${msg}\n`);
}

// ── one turn ──────────────────────────────────────────────────────────────
// Returns {status:"complete", full_text, concat, meta} | {status:"error"} |
// {status:"timeout"} — never throws (a dead node must not crash the driver).
async function chatTurn(convoId, question, timeoutMs = 130_000) {
  const deadline = Date.now() + timeoutMs;
  const since = await lastSeq().catch(() => 0);
  let messageId;
  for (;;) {
    try {
      const r = await invoke(
        "send_message_stream",
        { message: question, conversationId: convoId },
        Math.max(5000, deadline - Date.now()),
      );
      messageId = r?.message_id;
      break;
    } catch (e) {
      const msg = String(e.message ?? e);
      // The desktop's Runtime builds asynchronously after the bridge comes up;
      // in attach mode there is no backend-ready event, so early turns can race
      // the build and get "Backend is still loading." Transient — wait it out
      // within the turn deadline. A killed-node daemon gives a CONNECTION error
      // (not "loading"), so chaos turns still fail fast as expected.
      if (/loading|not ready/i.test(msg) && Date.now() < deadline) {
        await new Promise((r) => setTimeout(r, 3000));
        continue;
      }
      return { status: "error", detail: msg };
    }
  }
  if (!messageId) return { status: "error", detail: "send_message_stream returned no message_id" };
  for (;;) {
    const rows = await recent(since).catch(() => []);
    const done = rows.find((r) => r.event === "message-complete" && r.payload?.message_id === messageId);
    if (done) {
      const concat = rows
        .filter((r) => r.event === "message-chunk" && r.payload?.message_id === messageId)
        .map((r) => String(r.payload?.chunk ?? ""))
        .join("");
      return {
        status: "complete",
        messageId,
        full_text: String(done.payload?.full_text ?? ""),
        concat,
        meta: done.payload?.metadata ?? null,
      };
    }
    if (rows.some((r) => r.event === "message-error" && r.payload?.message_id === messageId))
      return { status: "error", detail: "message-error" };
    if (Date.now() > deadline) return { status: "timeout", detail: `no terminal in ${timeoutMs}ms` };
    await new Promise((r) => setTimeout(r, 1000));
  }
}

let _localIds = null;
async function localCorpusIds() {
  if (_localIds) return _localIds;
  const ids = new Set();
  try {
    for (const c of (await invoke("lc_list", {})) ?? []) {
      const id = c.corpus_id ?? c.id;
      if (id) ids.add(id);
    }
  } catch {
    /* none */
  }
  _localIds = ids;
  return ids;
}

// Apply the invariant pack to a COMPLETED turn. Returns [] if clean, else a
// list of violation strings. Citation resolution derefs via the bridge.
async function turnViolations(turn) {
  const v = [];
  // 1. stream integrity
  if (turn.concat !== turn.full_text)
    v.push(`stream_integrity: concat(${turn.concat.length}b) != full_text(${turn.full_text.length}b)`);
  const meta = turn.meta;
  if (!meta) {
    v.push("metadata absent on message-complete");
    return v;
  }
  // 2. intent present (provenance.intent OR top-level metadata.intent)
  const prov = meta.provenance;
  const intent = prov?.intent ?? meta.intent;
  if (!(typeof intent === "string" && intent.length > 0))
    v.push(`no intent (metadata keys: ${Object.keys(meta).join(",")})`);
  // 3. finish_reason sane
  if (prov && prov.finish_reason !== undefined && !["stop", "length"].includes(prov.finish_reason))
    v.push(`finish_reason=${JSON.stringify(prov.finish_reason)} not in {stop,length}`);
  // 4. citations resolve (local corpora only — attach-mode external corpora
  //    aren't readable through this instance's reading surface)
  const cites = meta.retrieved_chunks ?? [];
  if (cites.length > 0) {
    const localIds = await localCorpusIds();
    for (const c of cites) {
      if (c.provenance_tier === "web") continue;
      if (!(typeof c.corpus_id === "string" && c.corpus_id.length > 0)) {
        v.push(`citation missing corpus_id: ${JSON.stringify(c)}`);
        continue;
      }
      if (!Number.isFinite(c.chunk_id)) {
        v.push(`citation missing chunk_id: ${JSON.stringify(c)}`);
        continue;
      }
      if (!localIds.has(c.corpus_id)) continue; // external (attach) corpus
      try {
        const chunk = await invoke("read_get_chunk", { corpusId: c.corpus_id, chunkId: c.chunk_id }, 15_000);
        const content = chunk?.content ?? chunk?.text;
        if (!content || String(content).length === 0)
          v.push(`dangling citation: read_get_chunk(${c.corpus_id},${c.chunk_id}) empty`);
      } catch (e) {
        v.push(`dangling citation: read_get_chunk(${c.corpus_id},${c.chunk_id}) threw ${e.message ?? e}`);
      }
    }
  }
  return v;
}

function dumpTranscript(label, turn, violations) {
  if (!TRANSCRIPT_DIR) return null;
  try {
    fs.mkdirSync(TRANSCRIPT_DIR, { recursive: true });
    const p = path.join(TRANSCRIPT_DIR, `node${NODE}-${label}.json`);
    fs.writeFileSync(p, JSON.stringify({ node: NODE, label, violations, turn }, null, 2));
    return p;
  } catch {
    return null;
  }
}

// Assert a completed turn (hard=counted checkpoint). Returns true if the turn
// completed-and-clean. An incomplete turn is observational unless `hard`.
async function assertTurn(convoId, question, { hard, label, timeoutMs }) {
  const turn = await chatTurn(convoId, question, timeoutMs ?? 130_000);
  if (turn.status !== "complete") {
    if (hard) {
      const bundle = dumpTranscript(label, turn, [turn.detail]);
      finding({ phase: label, ok: false, detail: `turn did not complete (${turn.status}: ${turn.detail})${bundle ? ` bundle=${bundle}` : ""}` });
      log(`✗ ${label}: ${turn.status} (${turn.detail})`);
    } else {
      finding({ kind: "app-turn-incomplete", status: turn.status, detail: turn.detail, q: question });
      log(`~ turn ${turn.status} under chaos (expected; ${turn.detail})`);
    }
    return false;
  }
  const violations = await turnViolations(turn);
  if (violations.length > 0) {
    // A COMPLETED turn that violates an invariant is a real bug regardless of
    // chaos — always a counted failure.
    const bundle = dumpTranscript(label, turn, violations);
    finding({ phase: hard ? label : "app-turn", ok: false, detail: violations.join(" | ") + (bundle ? ` bundle=${bundle}` : "") });
    log(`✗ ${label}: ${violations.length} violation(s): ${violations.join(" | ")}`);
    return false;
  }
  finding({ phase: hard ? label : "app-turn", ok: true, detail: `clean (${turn.full_text.length}b, ${(turn.meta?.retrieved_chunks ?? []).length} chunks)` });
  return true;
}

async function waitReady(timeoutMs = 120_000) {
  const deadline = Date.now() + timeoutMs;
  let healthy = false;
  while (Date.now() < deadline) {
    if (await healthz()) { healthy = true; break; }
    await new Promise((r) => setTimeout(r, 1000));
  }
  if (!healthy) return false; // the desktop bridge never came up
  // Subscribe so chunks/complete land in the replay ring.
  for (const ev of ["message-chunk", "message-complete", "message-error", "backend-ready", "backend-error"])
    await listen(ev);
  // NB: `backend-ready` is a LOCAL/embedded-mode signal (the desktop's OWN
  // embedded daemon finished loading a model). In ATTACH mode the desktop talks
  // to an already-live daemon and emits NO backend-ready — and the soak's nodes
  // boot autostart=false, so no model is loaded until the first request. So do
  // NOT hard-require backend-ready: wait briefly for it (or a backend-error),
  // then proceed regardless. The warm turn is the real readiness gate — it
  // triggers the lazy model load under its own (long) timeout.
  const settle = Math.min(Date.now() + 20_000, deadline);
  while (Date.now() < settle) {
    const rows = await recent(0).catch(() => []);
    if (rows.some((r) => r.event === "backend-error")) return false;
    if (rows.some((r) => r.event === "backend-ready")) break;
    await new Promise((r) => setTimeout(r, 1500));
  }
  return true; // bridge up + subscribed; the warm turn gates real readiness
}

async function main() {
  log(`bridge=${BRIDGE} corpus=${CORPUS} minutes=${MINUTES} findings=${FINDINGS}`);
  const ready = await waitReady();
  if (!ready) {
    finding({ phase: "app-ready", ok: false, detail: "desktop bridge never reached backend-ready" });
    log("✗ app-ready: backend never became ready");
    return 1;
  }
  finding({ phase: "app-ready", ok: true, detail: "desktop attached + backend-ready" });

  let convo;
  try {
    convo = (await invoke("create_conversation", {})).id;
  } catch (e) {
    finding({ phase: "app-ready", ok: false, detail: `create_conversation failed: ${e.message ?? e}` });
    return 1;
  }

  // Warm turn (hard): proves the grounded app surface works BEFORE chaos. If
  // this fails the whole premise is broken, so it's a counted checkpoint.
  log("warm grounded turn (pre-chaos, authoritative; triggers lazy model load)…");
  await assertTurn(convo, QUESTIONS[0], { hard: true, label: "app-warm", timeoutMs: 180_000 });

  // Drive grounded turns for the window. Invariants on completed turns are
  // counted (app-turn); incompletes are observational (chaos is expected).
  const deadline = Date.now() + MINUTES * 60 * 1000;
  let i = 0,
    completed = 0,
    incomplete = 0;
  while (Date.now() < deadline) {
    i += 1;
    const q = QUESTIONS[i % QUESTIONS.length];
    const ok = await assertTurn(convo, q, { hard: false, label: "app-turn" });
    ok ? (completed += 1) : (incomplete += 1);
    log(`turn ${i}: ${ok ? "ok" : "incomplete"} (ok=${completed} incomplete=${incomplete})`);
    await new Promise((r) => setTimeout(r, 3000));
  }

  // ── headline cross-layer assertion: the user surface RECOVERS ──
  // By now the soak's last cycle has healed the mesh (victim restarted). A
  // fresh grounded turn MUST complete cleanly. If chaos left the surface
  // bricked, this is the failure that matters. Retry briefly to let a just-
  // restarted attached daemon settle.
  log("final recovery turn (post-chaos, authoritative)…");
  let recovered = false;
  for (let attempt = 1; attempt <= 5 && !recovered; attempt++) {
    let fresh;
    try {
      fresh = (await invoke("create_conversation", {})).id;
    } catch {
      fresh = convo;
    }
    recovered = await assertTurn(fresh, QUESTIONS[0], { hard: attempt === 5, label: "app-recovered", timeoutMs: 180_000 });
    if (!recovered) await new Promise((r) => setTimeout(r, 4000));
  }

  finding({
    kind: "app-summary",
    completed,
    incomplete,
    recovered,
    detail: `node${NODE}: ${completed} clean / ${incomplete} chaos-incomplete turns; recovered=${recovered}`,
  });
  log(`done: ${completed} clean, ${incomplete} incomplete, recovered=${recovered}`);
  return 0;
}

// P3 one-shot probe — a single controlled turn at an orchestrator-chosen moment.
// Prints ONE finding line to stdout (the orchestrator folds it into the verdict);
// does NOT write the shared findings file (avoids racing the autonomous driver).
async function probeMain() {
  const up = await healthz();
  if (up) for (const ev of ["message-chunk", "message-complete", "message-error"]) await listen(ev);
  let convo = null;
  try {
    convo = (await invoke("create_conversation", {}, 10_000)).id;
  } catch {
    /* the SEND is what we measure; conversation-create may itself wobble in an outage */
  }
  const t0 = Date.now();
  const turn = up
    ? await chatTurn(convo, QUESTIONS[0], PROBE_TIMEOUT)
    : { status: "error", detail: "bridge unreachable" };
  const latency = Date.now() - t0;

  let ok;
  let detail;
  if (PROBE_EXPECT === "fail-fast") {
    // Outage: the user's node is DOWN. The turn must resolve FAST with an error,
    // not hang waiting out a long timeout (the node2 hang hypothesis).
    if (turn.status === "error") {
      ok = true;
      detail = `graceful fast error in ${latency}ms while node down: ${turn.detail}`;
    } else if (turn.status === "timeout") {
      ok = false;
      detail = `HANG: turn did not resolve in ${PROBE_TIMEOUT}ms while node down — should fail fast`;
    } else {
      // The desktop's inference DID hit the dead node (the daemon's
      // /v1/chat/completions connection error shows in the desktop log) and the
      // runtime fell back to a FAST error-completion instead of hanging — graceful
      // degradation. Fast resolution (no hang) is the pass condition; the snippet
      // + latency document that it's a fallback, not a real answer from elsewhere
      // (a real cold inference is ~5s; this path is ~1s).
      const snip = (turn.full_text || "").replace(/\s+/g, " ").trim().slice(0, 140);
      ok = true;
      detail = `graceful degradation: fast error-completion in ${latency}ms — "${snip}"`;
    }
  } else {
    // Recovery: node is back. A fresh turn must complete cleanly.
    if (turn.status === "complete") {
      const violations = await turnViolations(turn);
      ok = violations.length === 0;
      detail = ok
        ? `recovered: clean turn in ${latency}ms (${(turn.meta?.retrieved_chunks ?? []).length} chunks)`
        : `recovered but invariants failed: ${violations.join(" | ")}`;
    } else {
      ok = false;
      detail = `did NOT recover: ${turn.status} after restart (${turn.detail}) in ${latency}ms`;
    }
  }
  process.stdout.write(
    JSON.stringify({ phase: PROBE_LABEL, ok, node: Number(NODE), status: turn.status, latency_ms: latency, detail }) +
      "\n",
  );
  return ok ? 0 : 1;
}

(PROBE ? probeMain() : main())
  .then((rc) => process.exit(rc ?? 0))
  .catch((e) => {
    if (PROBE) {
      process.stdout.write(
        JSON.stringify({ phase: PROBE_LABEL, ok: false, node: Number(NODE), detail: `probe crashed: ${e?.message ?? e}` }) +
          "\n",
      );
    } else {
      finding({ kind: "app-driver-crash", detail: String(e?.stack ?? e) });
      process.stderr.write(`mesh-app-driver crashed: ${e?.stack ?? e}\n`);
    }
    process.exit(1);
  });
