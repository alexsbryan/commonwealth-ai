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
