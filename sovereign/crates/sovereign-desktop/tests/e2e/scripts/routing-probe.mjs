// SPDX-License-Identifier: AGPL-3.0-or-later
// Routing probe — isolate how OVERVIEW vs SPECIFIC questions classify when asked
// in a FRESH conversation (no thread to inherit intent from). Reuses chaos.mjs's
// proven attach-spawn + bridge protocol. For each probe it captures the router's
// own decision (router:policy_applied / stream routed) from the app log, plus
// latency + answer head. Read-only against corpora (attach).
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const REPO_ROOT = path.resolve(CRATE_ROOT, "../../..");
const ARTIFACTS = path.join(CRATE_ROOT, "test-artifacts");
const APP_LOG = path.join(ARTIFACTS, "routing-probe-app.log");
const BRIDGE = "http://127.0.0.1:9745";
const BRIDGE_PORT = 9745;
const DAEMON = "http://127.0.0.1:9741";
const APP_BIN = path.join(REPO_ROOT, "target/debug/sovereign-desktop");
const MODELS_DIR = path.join(REPO_ROOT, "sovereign/models");
const CHAT_MODEL =
  process.env.SOVEREIGN_REAL_CHAT_MODEL ?? path.join(MODELS_DIR, "Qwen3.6-35B-A3B-MTP-UD-Q6_K_XL.gguf");
const EMBED_MODEL =
  process.env.SOVEREIGN_REAL_EMBED_MODEL ?? path.join(MODELS_DIR, "qwen-embedding-0.6b.gguf");

// matched overview/specific pairs over a big corpus (wikipedia) and a small
// authored one (maple-house), plus a couple extra phrasings to test the router.
const PROBES = [
  // Fix B win: maple-house has 67 atlas Claim atoms — overview should now
  // INJECT them (atom_enum_overview glassbox) and GROUND, where it abstained
  // pre-fix ("none of the retrieved passages support an answer").
  { corpus: "maple-house", kind: "overview", q: "What is the most important thing in the maple-house material, and why?" },
  // Control: a specific maple-house question — unchanged, still quote-grounds.
  { corpus: "maple-house", kind: "specific", q: "What does the Maple House charter say about smoking?" },
  // Fix B negative control: sep has no atlas Claims → injection finds nothing,
  // falls through to normal retrieval (no regression, no spurious injection).
  { corpus: "sep", kind: "overview", q: "What is the most important thing in this material, and why?" },
  // Fix A: wikipedia-newsworthy is indexes_built=false (sync paused, never
  // built) — NOT dim-mismatch. Should now be SKIPPED + get the readiness
  // disclosure ("rebuild"), where pre-fix it fabricated over 4 stale chunks.
  { corpus: "wikipedia-newsworthy", kind: "overview", q: "What is the most important thing in this material?" },
];

// ── bridge helpers (verbatim shape from chaos.mjs) ──
async function invoke(cmd, args = {}, timeoutMs = 60_000) {
  const ctrl = new AbortController();
  const t = setTimeout(() => ctrl.abort(), timeoutMs);
  try {
    const res = await fetch(`${BRIDGE}/invoke`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-sovereign-spec": "probe" },
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
      const done = (u) => { sock.destroy(); resolve(u); };
      sock.once("connect", () => done(true));
      sock.once("error", () => done(false));
      sock.setTimeout(1500, () => done(false));
    });
  });
}

// ── attach spawn (verbatim from chaos.mjs bakeProfile ATTACH + maybeSpawn) ──
function bakeProfile(home) {
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
    `model_path = ${JSON.stringify(CHAT_MODEL)}`,
    `primary_model_path = ${JSON.stringify(CHAT_MODEL)}`,
    `embed_model_path = ${JSON.stringify(EMBED_MODEL)}`,
    `setup_complete = true`,
    `auto_collaborate = false`,
    ``,
  ].join("\n");
  for (const d of configDirs) fs.writeFileSync(path.join(d, "desktop.toml"), desktopToml);
  const realConfig = path.join(os.homedir(), ".sovereign", "config.toml");
  if (!fs.existsSync(realConfig)) throw new Error(`--attach needs ${realConfig}`);
  fs.copyFileSync(realConfig, path.join(sovDir, "config.toml"));
  const realSov = path.join(os.homedir(), ".sovereign");
  for (const sub of ["indexes", "recipes"]) {
    const target = path.join(realSov, sub);
    const link = path.join(sovDir, sub);
    if (fs.existsSync(target) && !fs.existsSync(link)) fs.symlinkSync(target, link);
  }
  return home;
}

let spawnedPid = null;
async function spawnDesktop() {
  if (await healthz()) { console.log("[probe] bridge already up — reusing"); return; }
  if (!(await portInUse(9741))) throw new Error("attach needs the dev daemon on :9741");
  const profileDir = path.join(ARTIFACTS, "routing-probe-profile");
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
    SOVEREIGN_COMMAND_BRIDGE_LEDGER: path.join(ARTIFACTS, "ledger-probe.jsonl"),
    RUST_LOG: process.env.RUST_LOG ?? "sovereign_desktop=debug,sovereign_core=debug,sovereign_inference=info",
  };
  const child = spawn(APP_BIN, [], { env, cwd: os.homedir(), stdio: ["ignore", log, log], detached: true });
  child.unref();
  spawnedPid = child.pid;
  const deadline = Date.now() + 240_000;
  while (!(await healthz())) {
    if (Date.now() > deadline) throw new Error("spawned desktop never came up");
    await new Promise((r) => setTimeout(r, 2000));
  }
  // gate on backend-ready (sticky)
  const bd = Date.now() + 240_000;
  for (;;) {
    const r = await fetch(`${BRIDGE}/listen`, {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ event: "backend-ready" }),
    }).then((x) => x.json()).catch(() => ({}));
    if (r.replayed) break;
    if (Date.now() > bd) throw new Error("backend-ready never fired");
    await new Promise((r) => setTimeout(r, 2000));
  }
  for (const ev of ["message-chunk", "message-complete", "message-error"]) await listen(ev);
}
function killGroup() {
  if (!spawnedPid) return;
  try { process.kill(-spawnedPid, "SIGTERM"); } catch {}
  setTimeout(() => { try { process.kill(-spawnedPid, "SIGKILL"); } catch {} }, 4000);
}
for (const s of ["SIGINT", "SIGTERM"]) process.on(s, () => { killGroup(); process.exit(130); });

// extract the LAST routing decision from the app log after `fromBytes`
function readRouting(fromBytes) {
  let txt = "";
  try {
    const fd = fs.openSync(APP_LOG, "r");
    const sz = fs.fstatSync(fd).size;
    const len = Math.max(0, sz - fromBytes);
    const buf = Buffer.alloc(len);
    fs.readSync(fd, buf, 0, len, fromBytes);
    fs.closeSync(fd);
    txt = buf.toString("utf8").replace(/\x1b\[[0-9;]*m/g, "");
  } catch { return null; }
  const policy = [...txt.matchAll(/router:policy_applied tier=(\w+) move_kind=(\w+) primary_intent=(\w+) confidence=([\d.]+)/g)].pop();
  const routed = [...txt.matchAll(/stream routed intent=(\w+) coarse=(Some\("[^"]+"\)|None)/g)].pop();
  if (!policy && !routed) return null;
  return {
    intent: policy?.[3] ?? routed?.[1] ?? "?",
    tier: policy?.[1] ?? "?",
    move_kind: policy?.[2] ?? "?",
    confidence: policy?.[4] ?? "?",
    coarse: routed?.[2] ?? "?",
  };
}
function fileSize() { try { return fs.statSync(APP_LOG).size; } catch { return 0; } }

async function awaitComplete(convoId, sinceSeq, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const rows = await recent(sinceSeq).catch(() => []);
    const done = rows.find((r) => r.event === "message-complete" && r.payload?.conversation_id === convoId);
    if (done) return { answer: String(done.payload?.full_text ?? ""), metadata: done.payload?.metadata ?? null };
    await new Promise((r) => setTimeout(r, 1000));
  }
  return null;
}

async function main() {
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  await spawnDesktop();
  console.log(`[probe] up. running ${PROBES.length} fresh-conversation probes\n`);
  const results = [];
  for (const p of PROBES) {
    let convo;
    try {
      const cc = await invoke("create_conversation", { title: `probe ${p.kind}` }, 15_000);
      convo = cc?.id ?? cc?.conversation_id ?? cc?.conversationId ?? (typeof cc === "string" ? cc : null);
      if (!convo) { console.log(`[probe] create_conversation odd result: ${JSON.stringify(cc)}`); continue; }
      // The desktop command param is `enabled_corpora` (Tauri camelCase
      // `enabledCorpora`) — NOT `corpora`. Sending the wrong key leaves the
      // conversation UNSCOPED (every corpus searched), which silently broke
      // every prior probe's scoping. Matches chaos.mjs.
      await invoke("set_conversation_enabled_corpora", { conversationId: convo, enabledCorpora: [p.corpus] }, 15_000);
    } catch (e) {
      console.log(`[probe] setup failed for ${p.corpus}/${p.kind}: ${e.message}`);
      results.push({ ...p, intent: "SETUP_FAIL", err: e.message });
      continue;
    }
    const beforeBytes = fileSize();
    const sinceSeq = await lastSeq();
    const t0 = Date.now();
    // fire the send (don't block on the full synthesis; we capture routing fast)
    const sendP = invoke("send_message_stream", { message: p.q, conversationId: convo }, 200_000).catch((e) => ({ _err: e.message }));
    // poll the app log for the routing line (appears within ~1-2s)
    let routing = null;
    for (let i = 0; i < 30 && !routing; i++) {
      await new Promise((r) => setTimeout(r, 500));
      routing = readRouting(beforeBytes);
    }
    // bounded wait for the answer for latency/verdict; cancel if it runs long
    const done = await awaitComplete(convo, sinceSeq, 150_000);
    const latency = Date.now() - t0;
    if (!done) { try { await invoke("cancel_stream", { conversationId: convo }, 8_000); } catch {} }
    await sendP.catch(() => {});
    const intent = done?.metadata?.intent;
    const retrieved = Array.isArray(done?.metadata?.retrieved_chunks) ? done.metadata.retrieved_chunks.length : "-";
    const row = {
      corpus: p.corpus, kind: p.kind,
      routerIntent: routing?.intent ?? "?", tier: routing?.tier ?? "?",
      conf: routing?.confidence ?? "?", coarse: routing?.coarse ?? "?",
      mdIntent: intent ?? (done ? "?" : "TIMEOUT"),
      retrieved, latencyS: Math.round(latency / 1000),
      answerHead: String(done?.answer ?? "").replace(/\n+/g, " ").slice(0, 90),
    };
    results.push(row);
    console.log(
      `[probe] ${p.corpus.padEnd(20)} ${p.kind.padEnd(9)} routerIntent=${row.routerIntent} tier=${row.tier} conf=${row.conf} coarse=${row.coarse} | mdIntent=${row.mdIntent} retrieved=${row.retrieved} ${row.latencyS}s`,
    );
  }
  console.log("\n======== ROUTING PROBE SUMMARY (fresh conversations — no thread inheritance) ========");
  for (const r of results) {
    console.log(`${r.corpus.padEnd(20)} ${String(r.kind).padEnd(9)} routerIntent=${String(r.routerIntent).padEnd(14)} tier=${String(r.tier).padEnd(8)} coarse=${r.coarse}`);
  }
  fs.writeFileSync(path.join(ARTIFACTS, "routing-probe-results.json"), JSON.stringify(results, null, 2));
  console.log(`\nresults → ${path.join(ARTIFACTS, "routing-probe-results.json")}`);
  killGroup();
}
main().catch((e) => { console.error(`[probe] fatal: ${e}`); killGroup(); process.exit(1); });
