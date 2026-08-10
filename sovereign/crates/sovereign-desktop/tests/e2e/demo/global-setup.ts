// SPDX-License-Identifier: AGPL-3.0-or-later
// Demo-mode global setup.
//
// Reuses the real-mode setup wholesale (build → bake scratch profile →
// launch the desktop with the command bridge → wait for backend-ready)
// and only changes the world it attaches to:
//
//   SOVEREIGN_REAL_ALLOW_ATTACH=1 — attach to the operator's LIVE daemon
//     on :9741. The demo needs the real corpora (sep, enron, wikipedia,
//     commonwealth-ai) and the real primary; the managed fixture daemon
//     has three toy documents and a 2B.
//   SOVEREIGN_DEMO=1 — skip the fixture + governance corpus plants. They
//     would appear on camera as "E2E Fixture Corpus" / "Maple House
//     (E2E)", and they would be written into the operator's real index.
//   SOVEREIGN_REAL_KEEP_PROFILE=1 — keep the scratch profile between
//     takes, so re-shooting a beat doesn't re-do first-run setup.
//
// Both env vars are read at module scope inside the real setup, so they
// are set BEFORE the dynamic import — a static import would hoist above
// the assignments and land on managed mode.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../..");
const DEMO_DIR = path.join(CRATE_ROOT, "test-artifacts/demo");
const LEDGER = path.join(DEMO_DIR, "ledger.jsonl");

/** Read a `[models]` key out of the host `~/.svrnmesh/config.toml`.
 *
 *  The real harness hardcodes a default chat/embed GGUF pair, and those
 *  defaults go stale as models are swapped — on this machine the baked
 *  `Qwen3-Embedding-0.6B-Q8_0.gguf` no longer exists (the embedder is
 *  `qwen-embedding-0.6b.gguf`), and setup dies on the existence check
 *  before a single frame is captured.
 *
 *  Demo mode attaches to the daemon, so the models it should bake are
 *  definitionally the ones the DAEMON has loaded — the same file it
 *  routes by. Reading them here means the demo can't drift from the
 *  operator's actual setup, which is the whole point of attach mode.
 *  Deliberately a line-scan, not a TOML parser: `[models]` is a flat
 *  table of quoted paths and a dependency is not worth it. */
function hostModel(key: "primary" | "fast" | "embed"): string | null {
  const cfg = path.join(os.homedir(), ".sovereign", "config.toml");
  let text: string;
  try {
    text = fs.readFileSync(cfg, "utf8");
  } catch {
    return null;
  }
  let inModels = false;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line.startsWith("[")) {
      inModels = line === "[models]";
      continue;
    }
    if (!inModels || line.startsWith("#")) continue;
    const m = line.match(new RegExp(`^${key}\\s*=\\s*"([^"]+)"`));
    if (m) return m[1];
  }
  return null;
}

export default async function demoGlobalSetup(): Promise<void> {
  process.env.SOVEREIGN_REAL_ALLOW_ATTACH = "1";
  process.env.SOVEREIGN_DEMO = "1";
  process.env.SOVEREIGN_REAL_KEEP_PROFILE ??= "1";
  // Own scratch profile, NOT the real suite's. Sharing it would carry that
  // suite's local corpus registrations ("E2E Fixture Corpus", "Maple House
  // (E2E)") onto the Library shelf and into frame — the exact thing
  // SOVEREIGN_DEMO=1 skips planting.
  process.env.SOVEREIGN_REAL_PROFILE_DIR ??= "demo-profile";

  // One run id for the whole invocation, stamped HERE because global-setup
  // runs exactly once while the worker (and so beat.ts) reloads on every
  // test failure. Workers inherit this env, restarts included, which keeps
  // one take under one id — see the RUN_ID comment in beat.ts.
  process.env.SOVEREIGN_DEMO_RUN_ID = String(Date.now());

  // Bake the models the DAEMON actually runs, not the real harness's
  // hardcoded defaults (which go stale on every model swap and fail the
  // existence check in bakeProfile before anything is captured).
  // `primary` over `fast`: B5's authoring loop and B8's code answer both
  // need the capable model, and attach mode routes to the daemon anyway.
  const chat = hostModel("primary") ?? hostModel("fast");
  const embed = hostModel("embed");
  if (chat && fs.existsSync(chat)) process.env.SOVEREIGN_REAL_CHAT_MODEL ??= chat;
  if (embed && fs.existsSync(embed)) process.env.SOVEREIGN_REAL_EMBED_MODEL ??= embed;
  console.log(
    `[demo-setup] models from host config — chat=${process.env.SOVEREIGN_REAL_CHAT_MODEL ?? "(harness default)"}, ` +
      `embed=${process.env.SOVEREIGN_REAL_EMBED_MODEL ?? "(harness default)"}`,
  );

  // Truncate the ledger so a run's manifest is self-contained (the
  // exporter also filters by runId, but a fresh file makes a failed
  // run's leftovers impossible to mistake for this one's).
  fs.mkdirSync(DEMO_DIR, { recursive: true });
  fs.rmSync(LEDGER, { force: true });

  const real = (await import("../real/global-setup")).default;
  await real();
}
