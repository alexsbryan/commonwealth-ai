// Daemon client for POST /v1/completions (extension plan §client).
// Rich wire shape, streaming SSE accumulated once (no early render —
// an InlineCompletionItem wants the final text anyway, but SSE still
// wins: abort mid-decode stops the model immediately instead of
// letting it run to max_tokens, and the terminal frame carries the
// real finish_reason). Debug is always requested; the glassbox
// record feeds "Explain Last Suggestion".

import { SseParser } from "./sse";

export interface FimRequest {
  prefix: string;
  suffix: string;
  path?: string;
  language?: string;
}

export interface SovereignDebug {
  model_id?: string;
  slot?: string;
  fim_style?: string;
  mode?: string;
  prompt_chars?: number;
  emitted_chars?: number;
  stop_rule?: string;
  trimmed_chars?: number;
  finish_reason?: string;
  timings_ms?: { ttft?: number; total?: number };
}

export interface FimResult {
  text: string;
  finishReason: string;
  debug: SovereignDebug | null;
  wallMs: number;
}

export class DaemonError extends Error {
  constructor(
    message: string,
    readonly status?: number,
  ) {
    super(message);
    this.name = "DaemonError";
  }
}

export async function completeFim(
  endpoint: string,
  req: FimRequest,
  signal: AbortSignal,
): Promise<FimResult> {
  const started = Date.now();
  let resp: Response;
  try {
    resp = await fetch(`${endpoint}/v1/completions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        prefix: req.prefix,
        suffix: req.suffix,
        path: req.path,
        language: req.language,
        stream: true,
        debug: true,
      }),
      signal,
    });
  } catch (e) {
    if ((e as Error).name === "AbortError") throw e;
    throw new DaemonError(
      `daemon unreachable at ${endpoint} — is 'sovereign daemon run' up? (${(e as Error).message})`,
    );
  }
  if (!resp.ok) {
    let detail = "";
    try {
      const j = (await resp.json()) as { error?: { message?: string } };
      detail = j?.error?.message ?? "";
    } catch {
      /* non-JSON error body */
    }
    throw new DaemonError(
      detail || `daemon returned HTTP ${resp.status}`,
      resp.status,
    );
  }
  if (!resp.body) throw new DaemonError("daemon returned no body");

  const parser = new SseParser();
  const reader = resp.body.getReader();
  const decoder = new TextDecoder();
  let text = "";
  let finishReason = "stop";
  let debug: SovereignDebug | null = null;

  const handleEvent = (data: string) => {
    if (data === "[DONE]") return;
    let chunk: {
      choices?: { text?: string; finish_reason?: string | null }[];
      sovereign_debug?: SovereignDebug;
      error?: { message?: string };
    };
    try {
      chunk = JSON.parse(data);
    } catch {
      return; // malformed event — skip, the stream continues
    }
    if (chunk.error?.message) {
      throw new DaemonError(chunk.error.message);
    }
    if (chunk.sovereign_debug) debug = chunk.sovereign_debug;
    for (const choice of chunk.choices ?? []) {
      if (choice.text) text += choice.text;
      if (choice.finish_reason) finishReason = choice.finish_reason;
    }
  };

  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    for (const ev of parser.feed(decoder.decode(value, { stream: true }))) {
      handleEvent(ev.data);
    }
  }
  for (const ev of parser.end()) handleEvent(ev.data);

  return { text, finishReason, debug, wallMs: Date.now() - started };
}

// ---- next-edit rule lane (NEXT_EDIT.md §3) ---------------------------

export interface HistoryUnitWire {
  before: string;
  after: string;
  left: string;
  right: string;
}

export interface EditPredictionRequest {
  history: HistoryUnitWire[];
  text: string;
  /** UTF-16 code units into `text` — JS string offsets verbatim. */
  cursor: number;
  path?: string;
  language?: string;
  /** Opt into the model lane (P2) — off until its eval bank gates green. */
  model_lane?: boolean;
}

export interface EditPredictionEdit {
  start: number;
  end: number;
  new_text: string;
}

export interface EditPredictionDebug {
  rule_find?: string | null;
  rule_replace?: string | null;
  rule_key?: string | null;
  support?: number;
  sites?: number;
  edits_capped?: boolean;
  reason_silent?: string | null;
  timings_ms?: { total?: number };
  /** Model-lane glassbox (present when the request opted in). */
  model?: {
    consulted?: boolean;
    reason?: string | null;
    skipped?: string | null;
    needle?: string | null;
    dropped?: string | null;
    model_id?: string | null;
  } | null;
}

export interface EditPredictionResult {
  edits: EditPredictionEdit[];
  engine: string;
  debug: EditPredictionDebug | null;
  wallMs: number;
}

/** Hard ceiling on one prediction round-trip. Keystrokes abort in
 *  flight, but an idle user typing nothing more would otherwise leave
 *  a request against a wedged daemon pending forever — and a reply
 *  arriving minutes later still passes the caller's document-version
 *  check and surfaces a proposal out of nowhere. Comfortably above the
 *  daemon's own 15s model-lane budget, so a real slow consult still
 *  lands. */
const PREDICT_TIMEOUT_MS = 20_000;

/** POST /v1/edit_predictions — plain JSON in/out, no streaming; an
 *  empty `edits` array is the healthy "nothing to suggest" case. */
export async function predictEdits(
  endpoint: string,
  req: EditPredictionRequest,
  signal: AbortSignal,
): Promise<EditPredictionResult> {
  const started = Date.now();
  // Caller's signal OR our deadline. Built by hand rather than with
  // AbortSignal.any so this works on every VS Code runtime we support.
  const deadline = new AbortController();
  const onAbort = () => deadline.abort();
  signal.addEventListener("abort", onAbort, { once: true });
  const timer = setTimeout(() => deadline.abort(), PREDICT_TIMEOUT_MS);
  try {
    return await predictEditsOnce(endpoint, req, deadline.signal, signal, started);
  } finally {
    clearTimeout(timer);
    signal.removeEventListener("abort", onAbort);
  }
}

async function predictEditsOnce(
  endpoint: string,
  req: EditPredictionRequest,
  signal: AbortSignal,
  caller: AbortSignal,
  started: number,
): Promise<EditPredictionResult> {
  let resp: Response;
  try {
    resp = await fetch(`${endpoint}/v1/edit_predictions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ ...req, debug: true }),
      signal,
    });
  } catch (e) {
    // Our deadline, not the caller's abort: the daemon is wedged
    // rather than superseded. Report it as a daemon fault so it is
    // never mistaken for "the user kept typing".
    if ((e as Error).name === "AbortError" && !caller.aborted) {
      throw new DaemonError(
        `daemon at ${endpoint} did not answer within ${PREDICT_TIMEOUT_MS}ms`,
      );
    }
    if ((e as Error).name === "AbortError") throw e;
    throw new DaemonError(
      `daemon unreachable at ${endpoint} — is 'sovereign daemon run' up? (${(e as Error).message})`,
    );
  }
  if (!resp.ok) {
    let detail = "";
    try {
      const j = (await resp.json()) as { error?: { message?: string } };
      detail = j?.error?.message ?? "";
    } catch {
      /* non-JSON error body */
    }
    throw new DaemonError(detail || `daemon returned HTTP ${resp.status}`, resp.status);
  }
  const body = (await resp.json()) as {
    engine?: string;
    edits?: EditPredictionEdit[];
    sovereign_debug?: EditPredictionDebug;
  };
  return {
    edits: body.edits ?? [],
    engine: body.engine ?? "rule",
    debug: body.sovereign_debug ?? null,
    wallMs: Date.now() - started,
  };
}

export interface FimStatus {
  slot: string;
  model_id: string;
  fim_style: string;
  aliased_to_fast: boolean;
}

export interface StatusProbe {
  daemonUp: boolean;
  fim: FimStatus | null;
}

/** Probe GET /status for the status bar and the diagnose command. */
export async function probeStatus(
  endpoint: string,
  timeoutMs = 3000,
): Promise<StatusProbe> {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeoutMs);
  try {
    const resp = await fetch(`${endpoint}/status`, { signal: ctrl.signal });
    if (!resp.ok) return { daemonUp: false, fim: null };
    const j = (await resp.json()) as { inference?: { fim?: FimStatus | null } };
    return { daemonUp: true, fim: j?.inference?.fim ?? null };
  } catch {
    return { daemonUp: false, fim: null };
  } finally {
    clearTimeout(timer);
  }
}
