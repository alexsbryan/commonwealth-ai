// SPDX-License-Identifier: AGPL-3.0-or-later
// Soak runner — the overnight bug-bash machine (plan Phase 5).
//
// Drives randomized persona actions against a REAL desktop's command
// bridge (no DOM — ~10× the action rate of UI automation; the DOM
// contract is the real suite's job). Every turn-shaped action is
// checked against the invariant pack; violations and errors append to
// test-artifacts/soak-findings.jsonl with full repro context (seed,
// tick, persona, action detail). scripts/soak-report.mjs renders the
// morning summary.
//
// Usage:
//   node tests/e2e/scripts/soak.mjs [--minutes 120] [--seed N] [--plant-finding]
//
// Requirements: a desktop running with SOVEREIGN_COMMAND_BRIDGE=1 on
// :9745 (the real-mode profile — run `npm run test:e2e:real` once to
// bake it, then launch the desktop on it, or let the soak spawn one
// with --spawn). The dev daemon may be up (attach mode) or down
// (local mode); personas only mutate scratch-local state.
//
// Determinism: one seeded PRNG drives every choice. Re-running with a
// finding's logged seed replays the same action sequence (model
// outputs still vary — the sequence is what repro needs).
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const REPO_ROOT = path.resolve(CRATE_ROOT, "../../..");
const ARTIFACTS = path.join(CRATE_ROOT, "test-artifacts");
const FINDINGS = path.join(ARTIFACTS, "soak-findings.jsonl");
const LATENCY = path.join(ARTIFACTS, "soak-latency.jsonl");
const BRIDGE = process.env.SOVEREIGN_BRIDGE_URL ?? "http://127.0.0.1:9745";

// ── args ──────────────────────────────────────────────────────────
const argv = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : fallback;
};
const MINUTES = Number(flag("minutes", "120"));
const SEED = Number(flag("seed", String(Date.now() % 2 ** 31)));
const PLANT = argv.includes("--plant-finding");
const SPAWN = argv.includes("--spawn");

// mulberry32 — tiny seeded PRNG.
function mulberry32(a) {
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const rand = mulberry32(SEED);
const pick = (arr) => arr[Math.floor(rand() * arr.length)];

// ── bridge plumbing ───────────────────────────────────────────────
async function invoke(cmd, args = {}, spec = "soak") {
  const res = await fetch(`${BRIDGE}/invoke`, {
    method: "POST",
    headers: { "content-type": "application/json", "x-sovereign-spec": spec },
    body: JSON.stringify({ cmd, args }),
  });
  const body = await res.json();
  if (!body.ok) throw new Error(`invoke ${cmd}: ${JSON.stringify(body.error)}`);
  return body.result;
}
async function listen(event) {
  await fetch(`${BRIDGE}/listen`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ event }),
  });
}
async function recent(sinceSeq = 0) {
  const res = await fetch(`${BRIDGE}/events/recent?since_seq=${sinceSeq}`);
  return (await res.json()).rows;
}
async function lastSeq() {
  const rows = await recent(0);
  return rows.length ? rows[rows.length - 1].seq : 0;
}

// ── findings ──────────────────────────────────────────────────────
let tick = 0;
function record(file, row) {
  fs.appendFileSync(file, `${JSON.stringify(row)}\n`);
}
function finding(persona, action, kind, detail) {
  const row = { ts: Date.now(), seed: SEED, tick, persona, action, kind, detail };
  record(FINDINGS, row);
  console.log(`  ⚠ FINDING [${kind}] ${persona}/${action}: ${JSON.stringify(detail).slice(0, 160)}`);
}

// ── invariants (page-free port of tests/e2e/real/invariants.ts) ───
const localCorpusIds = new Set();
async function refreshLocalCorpora() {
  try {
    for (const c of await invoke("lc_list")) localCorpusIds.add(c.corpus_id ?? c.id);
  } catch {
    /* none */
  }
}
async function checkTurn(persona, action, sinceSeq, messageId, opts = {}) {
  const rows = await recent(sinceSeq);
  const chunks = rows
    .filter((r) => r.event === "message-chunk" && r.payload?.message_id === messageId)
    .map((r) => r.payload.chunk);
  const completes = rows.filter(
    (r) => r.event === "message-complete" && r.payload?.message_id === messageId,
  );
  if (completes.length !== 1) {
    finding(persona, action, "stream", { messageId, completes: completes.length });
    return null;
  }
  const complete = completes[0].payload;
  let full = complete.full_text ?? "";
  if (PLANT && !checkTurn.planted) {
    checkTurn.planted = true; // --plant-finding self-test: corrupt the FIRST checked turn
    full += "<<planted-corruption>>";
  }
  if (chunks.join("") !== full) {
    // Cancelled turns get their own kind: first soak observed an
    // equal-length byte mismatch on a cancel (note d4d81b6c — likely
    // emit-order scramble on the cancel flush) — a different question
    // from mid-stream corruption on normal turns.
    const kind = opts.cancelled ? "chunk_integrity_cancelled" : "chunk_integrity";
    const concat = chunks.join("");
    let div = -1;
    for (let i = 0; i < Math.min(concat.length, full.length); i++) {
      if (concat[i] !== full[i]) {
        div = i;
        break;
      }
    }
    finding(persona, action, kind, {
      messageId,
      concat_len: concat.length,
      full_len: full.length,
      first_divergence: div,
      concat_at_div: div >= 0 ? concat.slice(Math.max(0, div - 20), div + 40) : null,
      full_at_div: div >= 0 ? full.slice(Math.max(0, div - 20), div + 40) : null,
    });
  }
  const meta = complete.metadata ?? {};
  const intent = meta?.provenance?.intent ?? meta?.intent;
  if (!intent && !opts.cancelled) {
    finding(persona, action, "no_intent", { messageId, meta_keys: Object.keys(meta) });
  }
  const cites = meta?.retrieved_chunks ?? [];
  for (const c of cites) {
    if (c.provenance_tier === "web") continue;
    if (!localCorpusIds.has(c.corpus_id)) continue;
    try {
      const chunk = await invoke("read_get_chunk", {
        corpusId: c.corpus_id,
        chunkId: c.chunk_id,
      });
      if (!chunk) {
        finding(persona, action, "dangling_citation", { corpus: c.corpus_id, chunk: c.chunk_id });
      }
    } catch (e) {
      finding(persona, action, "citation_resolve_error", { error: String(e) });
    }
  }
  const sa = meta?.provenance?.self_assessment;
  if (sa && sa.includes("not traceable")) {
    finding(persona, action, "numeric_audit", { self_assessment: sa });
  }
  return { complete, intent, chunkCount: chunks.length };
}

/** Send one message and await its terminal event. Returns null on
 *  timeout (recorded as a finding). */
async function turn(persona, action, conversationId, message, timeoutMs = 240_000) {
  const since = await lastSeq();
  const t0 = Date.now();
  const started = await invoke(
    "send_message_stream",
    { message, conversationId },
    `soak:${persona}`,
  );
  const messageId = started.message_id;
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const rows = await recent(since);
    if (
      rows.some(
        (r) =>
          (r.event === "message-complete" || r.event === "message-error") &&
          r.payload?.message_id === messageId,
      )
    ) {
      break;
    }
    if (Date.now() > deadline) {
      finding(persona, action, "turn_timeout", { messageId, message: message.slice(0, 80) });
      return null;
    }
    await new Promise((r) => setTimeout(r, 1000));
  }
  const ms = Date.now() - t0;
  record(LATENCY, { ts: Date.now(), seed: SEED, tick, persona, action, ms });
  const rows = await recent(since);
  const errored = rows.find(
    (r) => r.event === "message-error" && r.payload?.message_id === messageId,
  );
  if (errored) {
    finding(persona, action, "turn_error", { message: errored.payload?.message });
    return null;
  }
  return checkTurn(persona, action, since, messageId);
}

// ── personas ──────────────────────────────────────────────────────
const QUESTIONS = [
  "What is the chemical symbol for gold?",
  "When was the Meridian Lighthouse automated?",
  "Who rescued the crew of the schooner Tamarind?",
  "How tall is the Meridian Lighthouse tower?",
  "What is the capital of Japan?",
  "How does a Fresnel lens focus light?",
  "What powered the lighthouse rotation before electrification?",
  "Summarize what you know about Elowen Marsh.",
];

const personas = {
  // Multi-turn conversations, topic switches, occasional corpus seal.
  chatter: {
    weight: 4,
    state: { convo: null, turnsLeft: 0 },
    async act() {
      const s = this.state;
      if (!s.convo || s.turnsLeft <= 0) {
        s.convo = (await invoke("create_conversation", {}, "soak:chatter")).id;
        s.turnsLeft = 1 + Math.floor(rand() * 4);
        if (rand() < 0.3) {
          const fixture = [...localCorpusIds][0];
          if (fixture) {
            await invoke(
              "set_conversation_enabled_corpora",
              { conversationId: s.convo, enabledCorpora: [fixture] },
              "soak:chatter",
            );
          }
        }
      }
      s.turnsLeft -= 1;
      await turn("chatter", "ask", s.convo, pick(QUESTIONS));
    },
  },

  // Send a long generation, cancel after a random beat, then verify
  // the stream terminates. Fresh conversation each time — note
  // 2cd9227e: accumulated cancelled partials brick a conversation.
  canceler: {
    weight: 2,
    async act() {
      const convo = (await invoke("create_conversation", {}, "soak:canceler")).id;
      const since = await lastSeq();
      const started = await invoke(
        "send_message_stream",
        { message: "Write a very long story about the sea and its moods.", conversationId: convo },
        "soak:canceler",
      );
      await new Promise((r) => setTimeout(r, 500 + rand() * 3000));
      await invoke("cancel_stream", { conversationId: convo }, "soak:canceler");
      const deadline = Date.now() + 60_000;
      for (;;) {
        const rows = await recent(since);
        if (
          rows.some(
            (r) =>
              (r.event === "message-complete" || r.event === "message-error") &&
              r.payload?.message_id === started.message_id,
          )
        ) {
          break;
        }
        if (Date.now() > deadline) {
          finding("canceler", "cancel", "cancel_no_terminal", {
            messageId: started.message_id,
          });
          return;
        }
        await new Promise((r) => setTimeout(r, 1000));
      }
      await checkTurn("canceler", "cancel", since, started.message_id, { cancelled: true });
    },
  },

  // Reading surface: resolve chunks + neighbors on local corpora.
  reader: {
    weight: 2,
    async act() {
      const corpora = await invoke("lc_list", {}, "soak:reader");
      if (!corpora.length) return;
      const picked = pick(corpora);
      const corpus = picked.corpus_id ?? picked.id;
      const chunkId = Math.floor(rand() * 8);
      try {
        await invoke("read_get_chunk", { corpusId: corpus, chunkId }, "soak:reader");
        await invoke(
          "read_get_chunk_neighbors",
          { corpusId: corpus, chunkId, radius: 1 },
          "soak:reader",
        );
      } catch (e) {
        finding("reader", "read_chunk", "command_error", { corpus, chunkId, error: String(e) });
      }
    },
  },

  // Config churn: flip a flag, read it back, restore. Budget toggles.
  fiddler: {
    weight: 1,
    async act() {
      const cfg = await invoke("get_config", {}, "soak:fiddler");
      const flipped = { ...cfg, enable_recipe_authoring: !cfg.enable_recipe_authoring };
      await invoke("save_config", { config: flipped }, "soak:fiddler");
      const back = await invoke("get_config", {}, "soak:fiddler");
      if (back.enable_recipe_authoring !== flipped.enable_recipe_authoring) {
        finding("fiddler", "save_config", "config_roundtrip", { wrote: flipped.enable_recipe_authoring, read: back.enable_recipe_authoring });
      }
      await invoke("save_config", { config: cfg }, "soak:fiddler");
      const budget = await invoke("get_storage_budget", {}, "soak:fiddler");
      await invoke("set_storage_budget", { budget }, "soak:fiddler").catch((e) =>
        finding("fiddler", "set_storage_budget", "command_error", { error: String(e) }),
      );
    },
  },

  // Burn-down: safe read-only sweeps over cold command surface.
  surveyor: {
    weight: 2,
    async act() {
      const safe = [
        ["list_skills", {}],
        ["list_insights", {}],
        ["get_sink_status", {}],
        ["search_insights", { query: "lighthouse" }],
        ["enrich_list_corpora", {}],
        ["lc_incomplete_jobs", {}],
        ["lc_search", { corpusId: [...localCorpusIds][0] ?? "", query: "lighthouse" }],
        ["meshapp_installed_apps", {}],
        ["meshapp_list_installs", {}],
        ["get_activity_summary", {}],
        ["get_activity_recent", {}],
        ["get_chat_activity", {}],
        ["get_contribution_status", {}],
        ["get_recent_contributions", {}],
        ["recommended_profile", {}],
        ["primary_catalog", {}],
        ["list_daemon_models", {}],
        ["get_runtime_status", {}],
        ["scan_for_models", {}],
        ["atlas_list_corpora", {}],
        ["atlas_list_conv_corpora", {}],
        ["mesh_is_running", {}],
        ["mesh_relay_candidates", {}],
        ["list_legacy_documents", {}],
        ["get_mobile_pairing", {}],
        ["get_setup_context_size", {}],
        ["lc_watch_list", {}],
        ["lc_watch_incomplete_jobs", {}],
        ["recipe_author_list_projects", {}],
        ["search_messages", { query: "lighthouse" }],
      ];
      const [cmd, args] = pick(safe);
      try {
        await invoke(cmd, args, "soak:surveyor");
      } catch (e) {
        // Read-only commands erroring under normal conditions is a
        // legitimate finding (or a contract to learn — triage decides).
        finding("surveyor", cmd, "command_error", { error: String(e).slice(0, 200) });
      }
    },
  },

  // Heavy: upload a small unique doc, await ready, one ask, delete.
  importer: {
    weight: 1,
    async act() {
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), "soak-doc-"));
      const file = path.join(dir, `note-${tick}.txt`);
      fs.writeFileSync(
        file,
        `Soak note ${tick} (seed ${SEED}). The harbor master logged ${100 + Math.floor(rand() * 900)} vessels this season. The west pier light burns ${1 + Math.floor(rand() * 9)} lamps.\n`,
      );
      const up = await invoke("upload_document_asset", { filePath: file }, "soak:importer");
      const assetId = up.asset.id;
      const deadline = Date.now() + 180_000;
      for (;;) {
        const asset = await invoke("get_document_asset", { assetId }, "soak:importer");
        const state = JSON.stringify(asset.state ?? asset).toLowerCase();
        if (state.includes("ready")) break;
        if (state.includes("failed")) {
          finding("importer", "ingest", "ingest_failed", { assetId, state });
          return;
        }
        if (Date.now() > deadline) {
          finding("importer", "ingest", "ingest_timeout", { assetId });
          return;
        }
        await new Promise((r) => setTimeout(r, 3000));
      }
      const convo = (await invoke("create_conversation", {}, "soak:importer")).id;
      const ask = await invoke(
        "ask_document",
        { assetId, question: "How many vessels did the harbor master log?", conversationId: convo },
        "soak:importer",
      );
      if (!ask.response || ask.response.length === 0) {
        finding("importer", "ask_document", "empty_response", { assetId });
      }
      await invoke("delete_document_asset", { assetId }, "soak:importer").catch((e) =>
        finding("importer", "delete_document_asset", "command_error", { error: String(e) }),
      );
      fs.rmSync(dir, { recursive: true, force: true });
    },
  },
};

// ── app lifecycle (optional --spawn) ─────────────────────────────
async function healthz() {
  try {
    const res = await fetch(`${BRIDGE}/healthz`, { signal: AbortSignal.timeout(2000) });
    return (await res.json()).ok === true;
  } catch {
    return false;
  }
}
async function maybeSpawn() {
  if (await healthz()) return null;
  if (!SPAWN) {
    throw new Error(
      `bridge not reachable at ${BRIDGE}. Launch the desktop on the real-mode ` +
        `profile (or pass --spawn to let the soak do it).`,
    );
  }
  const home = path.join(ARTIFACTS, "real-profile/home");
  if (!fs.existsSync(path.join(home, ".config/sovereign/desktop.toml"))) {
    throw new Error("real-profile missing — run `npm run test:e2e:real` once first.");
  }
  const log = fs.openSync(path.join(ARTIFACTS, "soak-app.log"), "w");
  const child = spawn(path.join(REPO_ROOT, "target/debug/sovereign-desktop"), [], {
    env: {
      ...process.env,
      HOME: home,
      XDG_CONFIG_HOME: path.join(home, ".config"),
      XDG_DATA_HOME: path.join(home, ".local/share"),
      XDG_CACHE_HOME: path.join(home, ".cache"),
      SOVEREIGN_COMMAND_BRIDGE: "1",
      SOVEREIGN_COMMAND_BRIDGE_LEDGER: path.join(ARTIFACTS, "ledger-soak.jsonl"),
      RUST_LOG: "sovereign_desktop=info,sovereign_inference=info",
    },
    cwd: os.homedir(),
    stdio: ["ignore", log, log],
    detached: true,
  });
  child.unref();
  const deadline = Date.now() + 240_000;
  while (!(await healthz())) {
    if (Date.now() > deadline) throw new Error("spawned desktop never came up");
    await new Promise((r) => setTimeout(r, 2000));
  }
  return child.pid;
}

// ── main loop ─────────────────────────────────────────────────────
const weighted = Object.entries(personas).flatMap(([name, p]) =>
  Array(p.weight).fill(name),
);

async function main() {
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  const spawnedPid = await maybeSpawn();
  // healthz only proves the bridge thread is up — the backend (store,
  // models, local-corpus manager) loads for many more seconds. Gate
  // the loop on the backend-ready sticky or every early persona
  // action lands "Backend is still loading" noise findings.
  {
    const deadline = Date.now() + 240_000;
    for (;;) {
      const res = await fetch(`${BRIDGE}/listen`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ event: "backend-ready" }),
      });
      if ((await res.json()).replayed) break;
      if (Date.now() > deadline) throw new Error("backend-ready never fired");
      await new Promise((r) => setTimeout(r, 2000));
    }
  }
  await listen("message-chunk");
  await listen("message-complete");
  await listen("message-error");
  await refreshLocalCorpora();

  console.log(
    `[soak] seed=${SEED} minutes=${MINUTES} bridge=${BRIDGE} ` +
      `personas=${Object.keys(personas).join(",")}${PLANT ? " PLANT-FINDING" : ""}`,
  );
  record(FINDINGS, { ts: Date.now(), seed: SEED, kind: "soak_start", minutes: MINUTES });

  const endAt = Date.now() + MINUTES * 60_000;
  let actions = 0;
  while (Date.now() < endAt) {
    tick += 1;
    const name = pick(weighted);
    const persona = personas[name];
    const t0 = Date.now();
    try {
      await persona.act();
      actions += 1;
    } catch (e) {
      finding(name, "act", "persona_crash", { error: String(e).slice(0, 300) });
    }
    console.log(`[soak] tick=${tick} ${name} (${((Date.now() - t0) / 1000).toFixed(1)}s)`);
    await new Promise((r) => setTimeout(r, 500 + rand() * 1500));
  }

  record(FINDINGS, { ts: Date.now(), seed: SEED, kind: "soak_end", ticks: tick, actions });
  console.log(`[soak] done — ${actions} actions over ${tick} ticks. Report: npm run report:soak`);
  if (spawnedPid) {
    try {
      process.kill(spawnedPid, "SIGTERM");
    } catch {
      /* gone */
    }
  }
}

main().catch((e) => {
  console.error(`[soak] fatal: ${e}`);
  process.exit(1);
});
