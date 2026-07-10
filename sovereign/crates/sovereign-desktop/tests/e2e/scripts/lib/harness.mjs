// SPDX-License-Identifier: AGPL-3.0-or-later
// Shared bridge + spawn plumbing for the DOM-free desktop harness scripts.
//
// chaos.mjs's header says: "Shares the supervised-spawn + bridge pattern with
// soak.mjs (KEEP IN SYNC); factor a shared harness module if this sticks."
// personas.mjs is the third consumer — this is that module. chaos.mjs and
// soak.mjs still carry their own copies (they are mid-measurement-loop and
// stability there matters more than DRY); fold them onto this module the next
// time either needs surgery.
//
// Everything here mirrors the proven chaos.mjs behaviour, with two deliberate
// parameterizations:
//   - `autoCollaborate` in the baked profile. chaos bakes `false` (its oracle
//     is the answer itself); persona mode bakes `true` because the gap-check →
//     information-request → web-search path is gated on it
//     (collaboration.rs: run_collaboration returns NotAttempted when off).
//   - artifact paths, so concurrent harnesses don't clobber each other's
//     journals/profiles.
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const CRATE_ROOT = path.resolve(__dirname, "../../../..");
export const REPO_ROOT = path.resolve(CRATE_ROOT, "../../..");
export const ARTIFACTS = path.join(CRATE_ROOT, "test-artifacts");
export const APP_BIN = path.join(REPO_ROOT, "target/debug/sovereign-desktop");
export const CLI_BIN = path.join(REPO_ROOT, "target/debug/sovereign-cli");
// This dev box runs DEBUG builds by convention — release siblings often don't
// exist. Resolve by existence so the grounding scorer doesn't silently null
// every verdict (observed: 0/13 aligned verdicts in a study run because only
// target/debug/sovereign-cli-llm was built).
const SCORE_CANDIDATES = [
  process.env.SOVEREIGN_SCORE_CLI,
  path.join(REPO_ROOT, "target/release/sovereign-cli-llm"),
  path.join(REPO_ROOT, "target/debug/sovereign-cli-llm"),
].filter(Boolean);
export const SCORE_CLI =
  SCORE_CANDIDATES.find((p) => fs.existsSync(p)) ?? SCORE_CANDIDATES[SCORE_CANDIDATES.length - 1];
export const MODELS_DIR = path.join(REPO_ROOT, "sovereign/models");
export const DAEMON = "http://127.0.0.1:9741";
const CHAT_MODEL =
  process.env.SOVEREIGN_REAL_CHAT_MODEL ?? path.join(MODELS_DIR, "Qwen3.5-2B.Q6_K.gguf");
const EMBED_MODEL =
  process.env.SOVEREIGN_REAL_EMBED_MODEL ??
  path.join(MODELS_DIR, "Qwen3-Embedding-0.6B-Q8_0.gguf");

// ── bridge client ──────────────────────────────────────────────────
export function makeBridge(url = process.env.SOVEREIGN_BRIDGE_URL ?? "http://127.0.0.1:9745") {
  async function invoke(cmd, args = {}, timeoutMs = 60_000) {
    const ctrl = new AbortController();
    const t = setTimeout(() => ctrl.abort(), timeoutMs);
    try {
      const res = await fetch(`${url}/invoke`, {
        method: "POST",
        headers: { "content-type": "application/json", "x-sovereign-spec": "persona" },
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
    const res = await fetch(`${url}/events/recent?since_seq=${sinceSeq}`);
    return (await res.json()).rows;
  }
  async function lastSeq() {
    const rows = await recent(0);
    return rows.length ? rows[rows.length - 1].seq : 0;
  }
  async function listen(event) {
    return fetch(`${url}/listen`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ event }),
    })
      .then((r) => r.json())
      .catch(() => ({}));
  }
  async function healthz() {
    try {
      const res = await fetch(`${url}/healthz`, { signal: AbortSignal.timeout(2500) });
      return (await res.json()).ok === true;
    } catch {
      return false;
    }
  }
  // Poll the replay ring for the first row matching `pred`, from `sinceSeq`.
  // Returns the row or null on timeout. `onRow` (optional) sees every new row
  // once — for logging narration chips etc. while waiting.
  async function awaitEvent(sinceSeq, pred, timeoutMs, onRow = null) {
    const deadline = Date.now() + timeoutMs;
    let cursor = sinceSeq;
    for (;;) {
      const rows = await recent(cursor).catch(() => []);
      if (rows.length) cursor = rows[rows.length - 1].seq;
      for (const r of rows) {
        if (onRow) onRow(r);
        if (pred(r)) return r;
      }
      if (Date.now() > deadline) return null;
      await new Promise((r) => setTimeout(r, 1200));
    }
  }
  // Wait for a chat turn's terminal. Returns {answer, chunks, completeSeq} or
  // null on error/timeout (chaos.mjs contract, plus the seq so callers can
  // await FOLLOW-ON events — information-request, message-refined — from the
  // right cursor).
  async function awaitChatAnswer(sinceSeq, messageId, timeoutMs) {
    const done = await awaitEvent(
      sinceSeq,
      (r) =>
        (r.event === "message-complete" && r.payload?.message_id === messageId) ||
        r.event === "message-error",
      timeoutMs,
    );
    if (!done || done.event === "message-error") return null;
    const rc = done.payload?.metadata?.retrieved_chunks;
    return {
      answer: String(done.payload?.full_text ?? ""),
      chunks: Array.isArray(rc) ? rc : [],
      completeSeq: done.seq,
    };
  }
  return { url, invoke, recent, lastSeq, listen, healthz, awaitEvent, awaitChatAnswer };
}

export function portInUse(port) {
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

// ── profile baking + spawn lifecycle (mirrors chaos.mjs) ───────────
function bakeProfile(home, { attach, supervisor, autoCollaborate, tag }) {
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
    `# Generated by tests/e2e/scripts/lib/harness.mjs (${tag})`,
    `model_path = ${JSON.stringify(CHAT_MODEL)}`,
    `primary_model_path = ${JSON.stringify(CHAT_MODEL)}`,
    `embed_model_path = ${JSON.stringify(EMBED_MODEL)}`,
    `setup_complete = true`,
    // The gap-check → information-request → web-search path is gated on this
    // (collaboration.rs:116). Persona mode needs it ON; chaos keeps it off.
    `auto_collaborate = ${autoCollaborate ? "true" : "false"}`,
    ``,
  ].join("\n");
  for (const d of configDirs) fs.writeFileSync(path.join(d, "desktop.toml"), desktopToml);
  if (supervisor) {
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
  } else if (attach) {
    // Attach to the EXISTING dev daemon on :9741 — copy its own config
    // verbatim (a minimal stanza fails SetupConfig deserialize), and symlink
    // the resident indexes/recipes so the catalog surfaces real corpora.
    // Read-only in practice; see chaos.mjs for the full rationale.
    const realConfig = path.join(os.homedir(), ".sovereign", "config.toml");
    if (!fs.existsSync(realConfig))
      throw new Error(`--attach needs ${realConfig} (the daemon's SetupConfig)`);
    fs.copyFileSync(realConfig, path.join(sovDir, "config.toml"));
    const realSov = path.join(os.homedir(), ".sovereign");
    for (const sub of ["indexes", "recipes"]) {
      const target = path.join(realSov, sub);
      const link = path.join(sovDir, sub);
      if (fs.existsSync(target) && !fs.existsSync(link)) fs.symlinkSync(target, link);
    }
  }
  return home;
}

// Spawn a bridged desktop with a scratch profile. Returns a handle with the
// pid and a killGroup(). `tag` names the profile/log so harnesses don't
// collide (chaos uses "chaos-profile"/"chaos-app.log"; persona uses its own).
export async function spawnDesktop({
  bridge,
  attach = false,
  supervisor = false,
  autoCollaborate = false,
  tag = "harness",
  appLog,
  ledger,
  rustLog,
}) {
  if (await bridge.healthz()) return { pid: null, killGroup: async () => {} };
  if (supervisor && (await portInUse(9741)))
    throw new Error("supervised spawn needs :9741 free — `sovereign daemon stop`.");
  if (attach && !(await portInUse(9741)))
    throw new Error("--attach needs the dev daemon on :9741. Start it: `sovereign daemon start`.");
  const profileDir = path.join(ARTIFACTS, `${tag}-profile`);
  fs.rmSync(profileDir, { recursive: true, force: true });
  const home = bakeProfile(path.join(profileDir, "home"), {
    attach,
    supervisor,
    autoCollaborate,
    tag,
  });
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  const log = fs.openSync(appLog ?? path.join(ARTIFACTS, `${tag}-app.log`), "w");
  const env = {
    ...process.env,
    HOME: home,
    XDG_CONFIG_HOME: path.join(home, ".config"),
    XDG_DATA_HOME: path.join(home, ".local/share"),
    XDG_CACHE_HOME: path.join(home, ".cache"),
    SOVEREIGN_COMMAND_BRIDGE: "1",
    SOVEREIGN_COMMAND_BRIDGE_PORT: String(new URL(bridge.url).port || 9745),
    SOVEREIGN_COMMAND_BRIDGE_LEDGER: ledger ?? path.join(ARTIFACTS, `ledger-${tag}.jsonl`),
    RUST_LOG:
      rustLog ??
      process.env.RUST_LOG ??
      "sovereign_desktop=debug,sovereign_core=debug,sovereign_inference=info",
  };
  if (supervisor) {
    env.SOVEREIGN_USE_SUPERVISOR = "1";
    env.SOVEREIGN_CLI_PATH = CLI_BIN;
  }
  const child = spawn(APP_BIN, [], {
    env,
    cwd: os.homedir(),
    stdio: ["ignore", log, log],
    detached: true,
  });
  child.unref();
  const pid = child.pid;
  const deadline = Date.now() + 240_000;
  while (!(await bridge.healthz())) {
    if (Date.now() > deadline) throw new Error("spawned desktop never came up");
    await new Promise((r) => setTimeout(r, 2000));
  }
  async function killGroup() {
    const grp = (sig) => {
      try {
        process.kill(-pid, sig);
        return true;
      } catch {
        return false;
      }
    };
    if (!grp("SIGTERM")) return;
    const deadline2 = Date.now() + 8000;
    while (Date.now() < deadline2 && grp(0)) await new Promise((r) => setTimeout(r, 500));
    grp("SIGKILL");
  }
  return { pid, killGroup };
}

// Wait until the backend-ready sticky event replays — the app is usable.
export async function awaitBackendReady(bridge, timeoutMs = 240_000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const r = await bridge.listen("backend-ready");
    if (r.replayed) return;
    if (Date.now() > deadline) throw new Error("backend-ready never fired");
    await new Promise((res) => setTimeout(res, 2000));
  }
}

// ── daemon LLM (the user-brain / judges) ───────────────────────────
// /v1/models lists MESH-advertised models even when the advertising peer is
// offline (known liveness gap) — picking the first non-embed id can select a
// phantom that errors on every completion. Prefer the local slot ALIASES
// (always loadable) and PROBE before committing.
export async function discoverBrainModel() {
  let ids = [];
  try {
    const res = await fetch(`${DAEMON}/v1/models`, { signal: AbortSignal.timeout(5000) });
    ids = ((await res.json()).data ?? []).map((m) => m.id);
  } catch {
    return null;
  }
  const candidates = [
    ...["primary", "fast"].filter((a) => ids.includes(a)),
    ...ids.filter((id) => !/embed/i.test(id) && id !== "primary" && id !== "fast"),
  ];
  for (const id of candidates) {
    try {
      const res = await fetch(`${DAEMON}/v1/chat/completions`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ model: id, messages: [{ role: "user", content: "Say OK" }], max_tokens: 8 }),
        signal: AbortSignal.timeout(60_000),
      });
      const body = await res.json();
      if (body?.choices?.[0]?.message) return id;
    } catch {
      /* try next */
    }
  }
  return null;
}

export async function chatCompletion(model, messages, { temperature = 0.9, maxTokens = 240 } = {}) {
  if (!model) return null;
  try {
    const res = await fetch(`${DAEMON}/v1/chat/completions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ model, messages, temperature, max_tokens: maxTokens }),
      signal: AbortSignal.timeout(120_000),
    });
    const body = await res.json();
    let content = body?.choices?.[0]?.message?.content ?? null;
    if (content != null) {
      // Thinking-mode models (Qwen3.5 family) may prefix <think> blocks; strip
      // them. An UNCLOSED block means the token budget died inside the
      // reasoning — treat as no answer so callers retry/fallback rather than
      // consuming chain-of-thought as content.
      content = String(content).replace(/<think>[\s\S]*?<\/think>/g, "").trim();
      if (content === "" || content.startsWith("<think>")) content = null;
    }
    return content;
  } catch {
    return null;
  }
}

export function firstJson(text) {
  if (!text) return null;
  const m = text.match(/\{[\s\S]*\}/);
  if (!m) return null;
  try {
    return JSON.parse(m[0]);
  } catch {
    return null;
  }
}
