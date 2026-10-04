// SPDX-License-Identifier: AGPL-3.0-or-later
// The CLI as a SURFACE, for the parity sweep (campaign sv-surface, THE
// CLAIM construction 2: "one fixture, both surfaces, identical answers
// per family").
//
// What this module is. `svrn chat ask` is no longer a host — since
// TOPOLOGY phase 6 it opens no Runtime, no store and no corpus engine;
// it posts a turn to the daemon and renders the `TurnFrame::Complete`
// that comes back (chat_cmd/ask.rs, its own module docs). The desktop
// runs the same driver in-process. So "do the two surfaces answer the
// same question the same way" is answerable by running both against ONE
// fixture and comparing the metadata families — which is what this file
// exists to make possible from a Playwright spec.
//
// Three rules it holds to:
//
//   • ONE normalizer, both surfaces (§10.6). The desktop hands the spec
//     the RAW persisted metadata blob (`message-complete.metadata`); the
//     CLI hands it the TYPED projection of that same blob
//     (`projection::project_turn_metadata` / `project_citations`). They
//     are two renderings of one thing, so `normalizeCitations` below
//     implements the projection's rule ONCE — corpus-grounded only:
//     non-empty `corpus_id` plus a `chunk_id` that is a non-empty string
//     or a number — and both sides go through it. Two hand-rolled
//     filters would compare the filters, not the surfaces.
//
//   • The env is not re-derived. `fixtureProfileEnv()` comes from
//     global-setup, so the CLI resolves the harness's scratch profile.
//     A CLI spawned with the ambient HOME would ask the OPERATOR's
//     daemon a fixture question — the 2026-07-30 / 2026-09-10 incident
//     in reverse.
//
//   • The comparator is a value, not an assertion. `compareSets` returns
//     a verdict the caller asserts on, so the SAME function can be shown
//     failing on a planted twin in the same run (§18.1: a check with no
//     failing input you can name is not a check).
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { fixtureProfileEnv, REPO_ROOT_DIR } from "../global-setup";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CRATE_ROOT = path.resolve(__dirname, "../../../..");
/** Where the campaign cites the proof from. */
export const PARITY_RESULTS_DIR = path.join(CRATE_ROOT, "test-artifacts", "results");
export const PARITY_RECORD = path.join(PARITY_RESULTS_DIR, "surface-parity.json");

/** The daemon both surfaces talk to — the harness's fixture daemon.
 *  Passed explicitly as `--daemon` so the CLI's resolution order
 *  (guest link → setup config → default) cannot quietly send the twin
 *  turn somewhere else. */
export const FIXTURE_DAEMON_BASE = "http://127.0.0.1:9741";

/** `svrn chat ask` is dispatched by `sovereign-cli` into this sibling
 *  (AGENTS.md's verb→binary map); the dispatcher execs it with the same
 *  argv, so the sibling IS the surface. Invoked directly for the same
 *  reason global-setup invokes `sovereign-cli-daemon` directly: no
 *  dependency on the dispatcher's `dev-tools` feature having been built. */
const CLI_BIN = path.join(REPO_ROOT_DIR, "target/debug/sovereign-cli-llm");

// ── The shape both surfaces are normalized into ────────────────────────

/** One citation handle, as both surfaces spell it. */
export interface SourceEntry {
  key: string;
  corpus_id: string;
  chunk_id: string;
  title: string | null;
  snippet: string | null;
}

/** What a surface answered, in the families the campaign compares. */
export interface SurfaceAnswer {
  surface: string;
  /** `metadata.routed_intent` — the route the turn actually took, by
   *  variant name. Written by `streaming.rs` into the persisted blob and
   *  projected verbatim by the CLI. `null` when the key is absent. */
  routedIntent: string | null;
  /** `metadata.grounding_gate.action` — the gate's exit
   *  (`citation_grounded`, `abstained_fragment`, …). `null` when the
   *  gate did not run (off / out of scope), which is itself a fact the
   *  two surfaces must agree on. */
  gateAction: string | null;
  /** True when a `grounding_gate` block was present at all — keeps
   *  "gate ran and reported nothing" apart from "no gate block"
   *  (§18.3). */
  gateRan: boolean;
  /** Corpus-grounded citations, deduped, sorted by key. */
  sources: SourceEntry[];
  /** `sources` keys only — what `compareSets` is run over. */
  sourceKeys: string[];
  /** Distinct document titles behind those chunks, sorted. */
  sourceTitles: string[];
  /** The answer as the surface streamed it, reasoning included. */
  fullText: string;
  /** Where this surface's answer was persisted, when it says. */
  conversationId: string | null;
  messageId: string | null;
  /** The whole grounding-gate block, verbatim, so a red run carries the
   *  quote/claim evidence without a re-run. */
  gate: unknown;
}

// ── The one normalizer ─────────────────────────────────────────────────

interface RawChunk {
  corpus_id?: unknown;
  chunk_id?: unknown;
  title?: unknown;
  snippet?: unknown;
}

/** Project a surface's chunk list into comparable handles, applying the
 *  wire projection's rule (see the module header). Duplicates collapse:
 *  a chunk cited twice is one source. */
export function normalizeCitations(chunks: unknown): SourceEntry[] {
  if (!Array.isArray(chunks)) return [];
  const byKey = new Map<string, SourceEntry>();
  for (const raw of chunks as RawChunk[]) {
    if (!raw || typeof raw !== "object") continue;
    const corpus = typeof raw.corpus_id === "string" ? raw.corpus_id : "";
    if (corpus.length === 0) continue;
    let chunk: string | null = null;
    if (typeof raw.chunk_id === "string" && raw.chunk_id.length > 0) chunk = raw.chunk_id;
    else if (typeof raw.chunk_id === "number" && Number.isFinite(raw.chunk_id)) {
      chunk = String(raw.chunk_id);
    }
    if (chunk === null) continue;
    const key = `${corpus}#${chunk}`;
    if (byKey.has(key)) continue;
    byKey.set(key, {
      key,
      corpus_id: corpus,
      chunk_id: chunk,
      title: typeof raw.title === "string" && raw.title.length > 0 ? raw.title : null,
      snippet:
        typeof raw.snippet === "string" && raw.snippet.length > 0
          ? raw.snippet.slice(0, 400)
          : null,
    });
  }
  return [...byKey.values()].sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
}

function titlesOf(sources: SourceEntry[]): string[] {
  return [...new Set(sources.map((s) => s.title).filter((t): t is string => !!t))].sort();
}

function gateOf(meta: Record<string, unknown> | null | undefined): {
  gate: unknown;
  gateRan: boolean;
  gateAction: string | null;
} {
  const gate = meta?.grounding_gate ?? null;
  if (!gate || typeof gate !== "object") {
    return { gate: null, gateRan: false, gateAction: null };
  }
  const action = (gate as { action?: unknown }).action;
  return {
    gate,
    gateRan: true,
    gateAction: typeof action === "string" ? action : null,
  };
}

/** The DESKTOP answer, from the `message-complete` payload the invariant
 *  pack already captured. `metadata` is the raw persisted blob. */
export function desktopAnswer(complete: {
  message_id?: string;
  full_text: string;
  metadata: Record<string, unknown> | null;
}): SurfaceAnswer {
  const meta = complete.metadata ?? {};
  const routed = meta.routed_intent;
  const sources = normalizeCitations(meta.retrieved_chunks);
  return {
    surface: "desktop",
    routedIntent: typeof routed === "string" && routed.length > 0 ? routed : null,
    ...gateOf(meta),
    sources,
    sourceKeys: sources.map((s) => s.key),
    sourceTitles: titlesOf(sources),
    fullText: complete.full_text,
    conversationId: null,
    messageId: complete.message_id ?? null,
  };
}

/** The CLI answer, from `chat ask --format json`. `metadata` is the
 *  TYPED projection (`projection::TurnMetadata`) and is ABSENT — not
 *  null, not `{}` — when the turn reported none of its three facts; the
 *  distinction is deliberate (ask.rs `render_json_typed`) so it is
 *  preserved here rather than defaulted away. */
export function cliAnswer(payload: Record<string, unknown>, label: string): SurfaceAnswer {
  const meta = (payload.metadata ?? null) as Record<string, unknown> | null;
  const routed = meta?.routed_intent;
  const sources = normalizeCitations(payload.citations);
  return {
    surface: label,
    routedIntent: typeof routed === "string" && routed.length > 0 ? routed : null,
    ...gateOf(meta),
    sources,
    sourceKeys: sources.map((s) => s.key),
    sourceTitles: titlesOf(sources),
    fullText: typeof payload.raw === "string" ? payload.raw : "",
    conversationId:
      typeof payload.conversation_id === "string" ? payload.conversation_id : null,
    messageId: typeof payload.message_id === "string" ? payload.message_id : null,
  };
}

// ── The comparator (a value, not an assertion) ─────────────────────────

export interface SetVerdict {
  equal: boolean;
  onlyA: string[];
  onlyB: string[];
  a: string[];
  b: string[];
}

/** Set comparison over sorted handle lists. Returns the verdict so the
 *  caller can assert `equal` for the parity pair AND `!equal` for the
 *  planted twin — the same code path proven live in both directions. */
export function compareSets(a: string[], b: string[]): SetVerdict {
  const sa = new Set(a);
  const sb = new Set(b);
  const onlyA = [...sa].filter((k) => !sb.has(k)).sort();
  const onlyB = [...sb].filter((k) => !sa.has(k)).sort();
  return { equal: onlyA.length === 0 && onlyB.length === 0, onlyA, onlyB, a: [...sa].sort(), b: [...sb].sort() };
}

/** Where two strings first diverge, for the TRACKED text row. `null`
 *  when identical. */
export function firstDivergence(
  a: string,
  b: string,
): { index: number; aTail: string; bTail: string } | null {
  if (a === b) return null;
  let i = 0;
  const n = Math.min(a.length, b.length);
  while (i < n && a[i] === b[i]) i++;
  return { index: i, aTail: a.slice(i, i + 160), bTail: b.slice(i, i + 160) };
}

// ── Driving the CLI ────────────────────────────────────────────────────

export interface CliRun {
  argv: string[];
  code: number | null;
  stdout: string;
  stderr: string;
  durationMs: number;
}

function run(argv: string[], timeoutMs: number): Promise<CliRun> {
  if (!fs.existsSync(CLI_BIN)) {
    throw new Error(
      `surface-parity: the CLI surface is not built — ${CLI_BIN} does not exist. ` +
        `Build it with \`cargo build -p sovereign-cli-llm\` (debug; see AGENTS.md).`,
    );
  }
  fs.mkdirSync(PARITY_RESULTS_DIR, { recursive: true });
  const started = Date.now();
  return new Promise((resolve, reject) => {
    const child = spawn(CLI_BIN, argv, {
      env: fixtureProfileEnv(),
      // Not the repo: the CLI must not resolve anything cwd-relative,
      // and global-setup makes the same choice for the app.
      cwd: PARITY_RESULTS_DIR,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (d) => (stdout += d.toString()));
    child.stderr.on("data", (d) => (stderr += d.toString()));
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      reject(
        new Error(
          `surface-parity: \`${argv.join(" ")}\` did not finish within ${timeoutMs}ms. ` +
            `stderr so far:\n${stderr}`,
        ),
      );
    }, timeoutMs);
    child.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({ argv, code, stdout, stderr, durationMs: Date.now() - started });
    });
  });
}

/** Pull the `--format json` payload out of stdout.
 *
 *  `render_json_typed` prints ONE pretty object on stdout and keeps every
 *  other line (the conversation banner, narration, corpora echo) on
 *  stderr — but the process also runs the rebrand startup migration
 *  before `main`, so a stray stdout line is possible. Parse the whole
 *  buffer first; only if that fails, take the span from the first
 *  line-initial `{` to the last `}`. A parse that still fails RAISES with
 *  the raw stdout — never a `{}` default (§18.3). */
export function parseAskJson(stdout: string): Record<string, unknown> {
  const attempts: string[] = [stdout];
  const start = stdout.search(/^\{/m);
  const end = stdout.lastIndexOf("}");
  if (start >= 0 && end > start) attempts.push(stdout.slice(start, end + 1));
  for (const text of attempts) {
    try {
      const v = JSON.parse(text);
      if (v && typeof v === "object" && !Array.isArray(v)) return v as Record<string, unknown>;
    } catch {
      /* try the next span */
    }
  }
  throw new Error(
    `surface-parity: \`chat ask --format json\` produced no parseable JSON object on ` +
      `stdout. Raw stdout was:\n${stdout.slice(0, 2000)}`,
  );
}

/** Ask the fixture daemon one question through the CLI surface.
 *
 *  `--corpus` scopes retrieval on the daemon side and forces the daemon
 *  to mint the conversation (it is refused together with
 *  `--conversation`, ask.rs `CORPUS_WITH_CONVERSATION`), which is what
 *  keeps every CLI turn in its own row. */
export async function chatAsk(opts: {
  question: string;
  corpusIds: string[];
  label: string;
  timeoutMs?: number;
}): Promise<{ answer: SurfaceAnswer; run: CliRun; payload: Record<string, unknown> }> {
  const argv = [
    "chat",
    "ask",
    "--daemon",
    FIXTURE_DAEMON_BASE,
    "--format",
    "json",
    ...opts.corpusIds.flatMap((id) => ["--corpus", id]),
    opts.question,
  ];
  const r = await run(argv, opts.timeoutMs ?? 240_000);
  if (r.code !== 0) {
    throw new Error(
      `surface-parity: \`${argv.join(" ")}\` exited ${r.code}.\n` +
        `--- stderr ---\n${r.stderr}\n--- stdout ---\n${r.stdout.slice(0, 2000)}`,
    );
  }
  const payload = parseAskJson(r.stdout);
  return { answer: cliAnswer(payload, opts.label), run: r, payload };
}

/** What the DAEMON says it has installed. Both the proof that the two
 *  surfaces are backed by the same fixture corpus and the source of the
 *  twin id — chosen from live state rather than assumed, so the negative
 *  control cannot quietly degrade into "the twin corpus wasn't there". */
export async function daemonCorpora(): Promise<
  Array<{ corpus_id: string; display_name: string; chunk_count: number }>
> {
  // The daemon's ONE corpus-status decider (rung 1: the CLI's status rows
  // moved down into corpus-engine and are served here — the same rows
  // `svrn corpus status` prints). There is no /v1/corpora.
  const url = `${FIXTURE_DAEMON_BASE}/internal/corpus/status`;
  const res = await fetch(url);
  if (!res.ok) {
    throw new Error(
      `surface-parity: GET ${url} → ${res.status}. The fixture daemon must ` +
        `serve the corpus status rows for the parity sweep to name what both ` +
        `surfaces are asking.`,
    );
  }
  const rows = (await res.json()) as Array<{
    corpus_id?: string;
    state?: string;
    chunk_count?: number | null;
  }>;
  return rows
    .filter(
      (c): c is { corpus_id: string; state?: string; chunk_count?: number | null } =>
        typeof c.corpus_id === "string" && c.corpus_id.length > 0,
    )
    .map((c) => ({
      corpus_id: c.corpus_id,
      // The status row carries no display name; the id is what the CLI's
      // --corpus takes and what the desktop's allow-list stores.
      display_name: c.corpus_id,
      chunk_count: typeof c.chunk_count === "number" ? c.chunk_count : 0,
    }));
}

/** Write the campaign's citable artifact. Called BEFORE the assertions
 *  run, so a red sweep still lands its evidence (the refusal ships its
 *  data — AGENTS.md "Ship code, not prose"). */
export function writeParityRecord(rec: Record<string, unknown>): void {
  fs.mkdirSync(PARITY_RESULTS_DIR, { recursive: true });
  fs.writeFileSync(PARITY_RECORD, JSON.stringify(rec, null, 2) + "\n");
}
