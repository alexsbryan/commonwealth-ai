// SPDX-License-Identifier: AGPL-3.0-or-later
// Global setup for the real-mode suite (playwright.real.config.ts):
// build + launch a REAL sovereign-desktop process with the command
// bridge enabled, under a hermetic scratch profile, and wait until the
// backend signals ready. Specs then drive the Vite-served frontend
// (Chromium) against this process via tauri-shim-real.js.
//
// Hermeticity: HOME + XDG_* point into test-artifacts/real-profile/, so
// config, conversations DB, data dir, and caches are all scratch.
// (test-artifacts/, not test-results/ — Playwright wipes its outputDir
// at every run start, which would destroy ledgers and the profile.)
// Models are the one shared resource (multi-GB GGUFs, mmap'd
// read-only) — referenced by absolute path.
//
// Mode: MANAGED by default — the harness owns a fixture-scoped daemon on
// :9741 (see MANAGED_DAEMON below). Attach mode is the opt-out.
//
// Env knobs:
//   SOVEREIGN_REAL_ALLOW_ATTACH=1  — attach to an EXISTING daemon on :9741
//       instead of starting our own (the app Attaches to it: knowledge/
//       inference state is then the REAL daemon's, not hermetic). Trades the
//       fixture-scoped determinism for the operator's real model/corpora.
//   SOVEREIGN_REAL_KEEP_PROFILE=1  — don't wipe the scratch profile
//       (inspect state across runs / faster reboots).
//   SOVEREIGN_REAL_XVFB=1          — wrap the app in xvfb-run -a.
//   SOVEREIGN_REAL_CHAT_MODEL / SOVEREIGN_REAL_EMBED_MODEL — override
//       the GGUF paths baked into the scratch desktop.toml.
import { execSync, spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const REPO_ROOT = path.resolve(CRATE_ROOT, "../../..");
const RESULTS = path.join(CRATE_ROOT, "test-artifacts");
const PROFILE = path.join(RESULTS, "real-profile");
const HOME = path.join(PROFILE, "home");
const APP_BIN = path.join(REPO_ROOT, "target/debug/sovereign-desktop");
const APP_LOG = path.join(RESULTS, "real-app.log");
const PID_FILE = path.join(RESULTS, "real-app.pid");
export const LEDGER_REAL = path.join(RESULTS, "ledger-real.jsonl");
/** Specs read this to learn the fixture corpus id + doc paths. */
export const FIXTURE_INFO = path.join(RESULTS, "real-fixture.json");
const FIXTURE_CORPUS_DIR = path.join(__dirname, "fixtures/corpus");
const FIXTURE_ATTACH_DOC = path.join(__dirname, "fixtures/attach/expedition-notes.txt");
const FIXTURE_DISPLAY_NAME = "E2E Fixture Corpus";
const BRIDGE = "http://127.0.0.1:9745";
const DAEMON_BIN = path.join(REPO_ROOT, "target/debug/sovereign-cli-daemon");

// Managed-daemon mode is the DEFAULT for the whole real suite: the harness
// starts its OWN fixture-scoped daemon on the test-profile HOME, so the daemon's
// index dir IS the desktop's read path (`list_corpora`/`read_get_chunk` resolve
// locally) AND retrieval is scoped to the single fixture corpus. It loads a fixed
// local chat/embed pair (a dense, non-MTP primary + the small embedder) so every
// spec — chat, journeys, and the workflow-author loop — runs in one deterministic
// environment. Attach-to-the-dev-daemon is the opt-out (SOVEREIGN_REAL_ALLOW_ATTACH
// =1): it exercises the operator's actual model/corpora but is non-hermetic and
// can't provide the fixture-scoped determinism (many corpora + a different, MTP
// model whose cancellation timing the journeys aren't tuned to). The legacy
// SOVEREIGN_REAL_MANAGED_DAEMON env is no longer read — managed is the default, so
// a script that still sets it lands on managed anyway (a harmless no-op).
const MANAGED_DAEMON = process.env.SOVEREIGN_REAL_ALLOW_ATTACH !== "1";

// Managed profile: the smallest viable chat model + the standard embedder. The 2B
// is deliberate, not just frugal — the streaming/cancel specs are cadence-sensitive
// (they assert `stream tokens > 0` inside a short window and "return to idle after
// cancel"), and a larger model's slower time-to-first-token breaks them (measured:
// the dense 4B regresses real-cancel-stream + cancellation.journey to 0 tokens /
// cancel-hang). The 2B greens 18/19 deterministically; the one exception is the
// agentic workflow-author loop, which needs a capable primary the 2B can't provide
// and is therefore GATED (see real-workflow-author.spec.ts) — run it in attach mode
// against a 9B+ daemon, or via SOVEREIGN_REAL_CHAT_MODEL. Phase 3 bench replays also
// override the model via that env so score deltas isolate transport, not model.
const DEFAULT_CHAT_MODEL = path.join(REPO_ROOT, "sovereign/models/Qwen3.5-2B.Q6_K.gguf");
const DEFAULT_EMBED_MODEL = path.join(
  REPO_ROOT,
  "sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf",
);

function portInUse(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const sock = net.connect({ port, host: "127.0.0.1" });
    const done = (used: boolean) => {
      sock.destroy();
      resolve(used);
    };
    sock.once("connect", () => done(true));
    sock.once("error", () => done(false));
    sock.setTimeout(1500, () => done(false));
  });
}

async function fetchJson(url: string, init?: RequestInit, timeoutMs = 3000) {
  const ctrl = new AbortController();
  const t = setTimeout(() => ctrl.abort(), timeoutMs);
  try {
    const res = await fetch(url, { ...init, signal: ctrl.signal });
    return (await res.json()) as Record<string, unknown>;
  } finally {
    clearTimeout(t);
  }
}

async function invoke<T = unknown>(
  cmd: string,
  args: Record<string, unknown> = {},
  timeoutMs = 30_000,
): Promise<T> {
  const body = await fetchJson(
    `${BRIDGE}/invoke`,
    {
      method: "POST",
      headers: { "content-type": "application/json", "x-sovereign-spec": "global-setup" },
      body: JSON.stringify({ cmd, args }),
    },
    timeoutMs,
  );
  if (!body.ok) throw new Error(`setup invoke ${cmd} failed: ${JSON.stringify(body.error)}`);
  return body.result as T;
}

/// Ingest the 3-file fixture corpus through the same lc_* commands the
/// watched-folder UI uses, and verify the DYNAMIC progress channel
/// (`local-corpus://progress/{job_id}`) actually delivers over the
/// bridge's lazy listen_any path — static event registration can't
/// cover per-job channel names, so this doubles as that contract's
/// regression check.
async function ingestFixtureCorpus(): Promise<void> {
  const existing = await invoke<Array<{ corpus_id: string; display_name?: string }>>(
    "lc_list",
  );
  let corpusId = existing.find((c) => c.display_name === FIXTURE_DISPLAY_NAME)?.corpus_id;

  if (!corpusId) {
    const validation = await invoke<{ exists: boolean; is_dir: boolean }>(
      "lc_validate_path",
      { path: FIXTURE_CORPUS_DIR },
    );
    if (!validation.exists || !validation.is_dir) {
      throw new Error(`fixture corpus dir invalid: ${FIXTURE_CORPUS_DIR}`);
    }
    const pre = await invoke<{ corpus_id: string; job_id: string }>(
      "lc_pre_scan",
      { path: FIXTURE_CORPUS_DIR, sourceType: "folder", displayName: FIXTURE_DISPLAY_NAME },
      60_000,
    );
    corpusId = pre.corpus_id;

    const jobId = await invoke<string>("lc_ingest", { corpusId, withOcr: false }, 60_000);
    const channel = `local-corpus://progress/${jobId}`;
    // Register the dynamic channel with the bridge; rows published
    // from here on land in the replay ring (`/events/recent`), so a
    // fast ingest can't outrun a streaming consumer. This is also the
    // regression check for lazy listen_any on per-job channel names.
    await fetchJson(`${BRIDGE}/listen`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ event: channel }),
    });

    // Completion authority: the job's own terminal event
    // (Complete / Error) read from the replay ring.
    const deadline = Date.now() + 180_000;
    for (;;) {
      const recent = await fetchJson(`${BRIDGE}/events/recent`);
      const rows = (recent.rows as Array<{ event: string; payload: unknown }>).filter(
        (r) => r.event === channel,
      );
      const terminal = rows
        .map((r) => JSON.stringify(r.payload))
        .find((p) => /complete|error/i.test(p));
      if (terminal) {
        if (/error/i.test(terminal)) {
          throw new Error(`fixture corpus ingest failed: ${terminal}`);
        }
        break;
      }
      if (Date.now() > deadline) {
        throw new Error(
          rows.length === 0
            ? `fixture ingest emitted nothing on ${channel} within 180s — ` +
              `bridge listen_any regression or stalled job; see ${APP_LOG}`
            : `fixture ingest never reached a terminal event within 180s — see ${APP_LOG}`,
        );
      }
      await new Promise((r) => setTimeout(r, 1000));
    }
    console.log(`[real-setup] fixture corpus ingested ✓ (${corpusId})`);
  } else {
    console.log(`[real-setup] fixture corpus already present (${corpusId})`);
  }

  fs.writeFileSync(
    FIXTURE_INFO,
    JSON.stringify(
      {
        corpus_id: corpusId,
        display_name: FIXTURE_DISPLAY_NAME,
        corpus_dir: FIXTURE_CORPUS_DIR,
        attach_doc: FIXTURE_ATTACH_DOC,
      },
      null,
      2,
    ),
  );
}

function bakeProfile(): void {
  if (!process.env.SOVEREIGN_REAL_KEEP_PROFILE) {
    fs.rmSync(PROFILE, { recursive: true, force: true });
  }
  // dirs::config_dir() is platform-specific — XDG (~/.config) on Linux,
  // ~/Library/Application Support on macOS — and the Rust desktop reads
  // desktop.toml from whichever applies (state/config.rs). Bake to both
  // so the hermetic harness works cross-platform; the off-platform copy
  // is harmless scratch under the wiped HOME.
  const configDirs = [
    path.join(HOME, ".config/sovereign"),
    path.join(HOME, "Library/Application Support/sovereign"),
  ];
  for (const d of configDirs) fs.mkdirSync(d, { recursive: true });
  fs.mkdirSync(path.join(HOME, ".local/share"), { recursive: true });
  fs.mkdirSync(path.join(HOME, ".cache"), { recursive: true });

  const chatModel = process.env.SOVEREIGN_REAL_CHAT_MODEL ?? DEFAULT_CHAT_MODEL;
  const embedModel = process.env.SOVEREIGN_REAL_EMBED_MODEL ?? DEFAULT_EMBED_MODEL;
  for (const m of [chatModel, embedModel]) {
    if (!fs.existsSync(m)) {
      throw new Error(
        `real-mode setup: model not found: ${m}\n` +
          `Point SOVEREIGN_REAL_CHAT_MODEL / SOVEREIGN_REAL_EMBED_MODEL at real GGUFs.`,
      );
    }
  }

  // Tilde-relative model paths in any daemon-side config resolve under
  // the scratch HOME — keep them valid via a symlink to the real dir.
  const sovDir = path.join(HOME, ".sovereign");
  fs.mkdirSync(sovDir, { recursive: true });
  const modelsLink = path.join(sovDir, "models");
  if (!fs.existsSync(modelsLink)) {
    fs.symlinkSync(path.join(REPO_ROOT, "sovereign/models"), modelsLink);
  }

  // Attach mode reads `~/.sovereign/config.toml` (SetupConfig) to learn which
  // model ids to route to the running daemon (state/builders/inference.rs). The
  // hermetic profile only bakes desktop.toml (Local-mode config), so attach boot
  // fails without this. Mirror the HOST's real config — same machine, so its
  // model ids match the daemon's loaded slots. Default (non-attach) mode loads
  // its own model from desktop.toml and doesn't need this.
  if (MANAGED_DAEMON) {
    // The harness owns the daemon: this file is BOTH the daemon's config (which
    // models to load, where its data lives) and the desktop's attach SetupConfig.
    // A fixed local model pair + the test HOME's data dir → a fixture-scoped
    // daemon whose index dir is the desktop's read path. The desktop derives its
    // routed model stems from desktop.toml (baked to the SAME chatModel below),
    // so the ids line up with the daemon's loaded slots.
    const daemonConfig = [
      `mcp_servers = []`,
      ``,
      `[models]`,
      `primary = ${JSON.stringify(chatModel)}`,
      `fast = ${JSON.stringify(chatModel)}`,
      `embed = ${JSON.stringify(embedModel)}`,
      `context_size = 32768`,
      ``,
      `[daemon]`,
      `client_port = 9741`,
      `internal_port = 9742`,
      `autostart = false`,
      `client_bind = "127.0.0.1"`,
      ``,
      `[data]`,
      `dir = ${JSON.stringify(sovDir)}`,
      ``,
      `[iroh]`,
      `enabled = false`,
      ``,
    ].join("\n");
    fs.writeFileSync(path.join(sovDir, "config.toml"), daemonConfig);
    console.log("[real-setup] managed-daemon: wrote fixture-scoped daemon config (small models, test HOME)");
  } else if (process.env.SOVEREIGN_REAL_ALLOW_ATTACH === "1") {
    const realConfig = path.join(os.homedir(), ".sovereign", "config.toml");
    if (fs.existsSync(realConfig)) {
      fs.copyFileSync(realConfig, path.join(sovDir, "config.toml"));
      console.log("[real-setup] attach mode: mirrored host ~/.sovereign/config.toml for daemon routing");
    } else {
      console.warn(`[real-setup] attach mode: host ${realConfig} missing — daemon routing may fail`);
    }
  }

  const desktopToml = [
    `# Generated by tests/e2e/real/global-setup.ts — hermetic real-mode profile.`,
    `model_path = ${JSON.stringify(chatModel)}`,
    `primary_model_path = ${JSON.stringify(chatModel)}`,
    `embed_model_path = ${JSON.stringify(embedModel)}`,
    `setup_complete = true`,
    `auto_collaborate = false`,
    ``,
  ].join("\n");
  for (const d of configDirs) fs.writeFileSync(path.join(d, "desktop.toml"), desktopToml);
}

/** Start the harness-owned daemon (HOME = test profile) and wait until it serves
 *  `/v1/models`. `daemon start` blocks until the daemon reports ready, but we
 *  re-probe the client port so a mis-started daemon fails here, loudly, rather
 *  than as a confusing mid-journey inference error. Stopped in global-teardown. */
async function startManagedDaemon(): Promise<void> {
  console.log("[real-setup] managed-daemon: starting fixture-scoped daemon (HOME=test profile)…");
  const env = {
    ...process.env,
    HOME,
    XDG_CONFIG_HOME: path.join(HOME, ".config"),
    XDG_DATA_HOME: path.join(HOME, ".local/share"),
    XDG_CACHE_HOME: path.join(HOME, ".cache"),
  };
  execSync(`${JSON.stringify(DAEMON_BIN)} daemon start`, {
    env,
    stdio: "inherit",
    timeout: 5 * 60 * 1000,
  });
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    try {
      const r = await fetchJson("http://127.0.0.1:9741/v1/models");
      if (Array.isArray((r as { data?: unknown }).data)) {
        console.log("[real-setup] managed-daemon: ready on :9741");
        return;
      }
    } catch {
      /* not up yet */
    }
    await new Promise((res) => setTimeout(res, 1000));
  }
  throw new Error("managed-daemon: not serving /v1/models on :9741 within 60s of `daemon start`");
}

export default async function globalSetup(): Promise<void> {
  fs.mkdirSync(RESULTS, { recursive: true });
  fs.rmSync(LEDGER_REAL, { force: true });

  // ── Guard: managed mode (the default) needs :9741 free to start its own
  // fixture-scoped daemon; attach mode (opt-out) needs a daemon already there.
  if (await portInUse(9741)) {
    if (MANAGED_DAEMON) {
      throw new Error(
        "real-mode setup: managed mode (the default) needs :9741 free to start " +
          "its own fixture-scoped daemon, but something is already listening " +
          "there. Stop it (`sovereign daemon stop`) first, or set " +
          "SOVEREIGN_REAL_ALLOW_ATTACH=1 to attach to it instead (non-hermetic).",
      );
    } else {
      console.warn(
        "[real-setup] :9741 is occupied — proceeding in ATTACH mode " +
          "(SOVEREIGN_REAL_ALLOW_ATTACH=1). Knowledge/inference state is the " +
          "REAL daemon's; conversations/config remain scratch.",
      );
    }
  } else if (!MANAGED_DAEMON) {
    // Attach requested but nothing is listening — fail loudly rather than
    // silently booting an embedded backend that can't ingest (the daemon owns
    // ingest+enrich; see local_corpus_commands.rs).
    throw new Error(
      "real-mode setup: SOVEREIGN_REAL_ALLOW_ATTACH=1 but no daemon is " +
        "listening on :9741. Start one (`sovereign daemon start`) or unset " +
        "the flag to use the default managed daemon.",
    );
  }

  // ── Build: embedded frontend assets + the debug binary ──
  if (!fs.existsSync(path.join(CRATE_ROOT, "dist/index.html"))) {
    console.log("[real-setup] dist/ missing — npm run build");
    execSync("npm run build", { cwd: CRATE_ROOT, stdio: "inherit" });
  }
  console.log("[real-setup] cargo build -p sovereign-desktop (debug)");
  execSync("cargo build -p sovereign-desktop", {
    cwd: REPO_ROOT,
    stdio: "inherit",
    timeout: 15 * 60 * 1000,
  });

  bakeProfile();

  // ── Managed daemon: start the harness-owned fixture-scoped daemon before the
  // desktop boots, so the app Attaches to it and the fixture ingest lands in the
  // shared (test-HOME) index dir. ──
  if (MANAGED_DAEMON) {
    await startManagedDaemon();
  }

  // ── Launch ──
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    HOME,
    XDG_CONFIG_HOME: path.join(HOME, ".config"),
    XDG_DATA_HOME: path.join(HOME, ".local/share"),
    XDG_CACHE_HOME: path.join(HOME, ".cache"),
    SOVEREIGN_COMMAND_BRIDGE: "1",
    SOVEREIGN_COMMAND_BRIDGE_LEDGER: LEDGER_REAL,
    RUST_LOG: process.env.RUST_LOG ?? "sovereign_desktop=info",
  };
  const useXvfb = process.env.SOVEREIGN_REAL_XVFB === "1";
  const cmd = useXvfb ? "xvfb-run" : APP_BIN;
  const args = useXvfb ? ["-a", APP_BIN] : [];

  const log = fs.openSync(APP_LOG, "w");
  const child = spawn(cmd, args, {
    env,
    cwd: os.homedir(), // not the repo — catch accidental cwd-relative writes
    stdio: ["ignore", log, log],
    detached: true,
  });
  child.unref();
  if (!child.pid) throw new Error("real-mode setup: failed to spawn app");
  fs.writeFileSync(PID_FILE, String(child.pid));
  console.log(`[real-setup] app pid=${child.pid}, log=${APP_LOG}`);

  // ── Readiness: bridge up, then backend-ready (sticky replay) ──
  const deadline = Date.now() + 180_000;
  let bridgeUp = false;
  while (Date.now() < deadline) {
    try {
      const h = await fetchJson(`${BRIDGE}/healthz`);
      if (h.ok) {
        bridgeUp = true;
        break;
      }
    } catch {
      /* not up yet */
    }
    await new Promise((r) => setTimeout(r, 1000));
  }
  if (!bridgeUp) throw new Error(`real-mode setup: bridge never came up — see ${APP_LOG}`);

  let ready = false;
  while (Date.now() < deadline) {
    try {
      const r = await fetchJson(`${BRIDGE}/listen`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ event: "backend-ready" }),
      });
      if (r.replayed) {
        console.log("[real-setup] backend-ready ✓");
        ready = true;
        break;
      }
      const s = await fetchJson(`${BRIDGE}/listen`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ event: "setup-required" }),
      });
      if (s.replayed) {
        throw new Error(
          "real-mode setup: app routed to the setup wizard — the baked " +
            "desktop.toml didn't satisfy the boot guard (model path? " +
            `setup_complete?). See ${APP_LOG}`,
        );
      }
    } catch (e) {
      if (e instanceof Error && e.message.startsWith("real-mode setup:")) throw e;
    }
    await new Promise((r) => setTimeout(r, 1000));
  }
  if (!ready) {
    throw new Error(
      `real-mode setup: backend-ready never fired within 180s — see ${APP_LOG}`,
    );
  }
  // Outside the readiness retry loop: an ingest failure is a hard
  // setup error, never silently retried.
  await ingestFixtureCorpus();
}
