// TTFI scenario recorder — captures real backend timing during
// production use and emits a Scenario file the harness can replay.
//
// Activation (none of these have any effect on the bundle when off):
//   • URL param:    ?ttfi=record   (one-shot, per-tab)
//   • Storage flag: localStorage.setItem('ttfi_record', '1')
//   • Code:         window.__ttfi_recorder__?.enable()
//
// When active, the recorder hooks the same Tauri events the
// scenario-player fires (turn-narration, interpretation-proposed,
// clarification-request, document:operation, message-chunk,
// message-complete, message-error). The first click on a `.send-btn`
// anchors t0; subsequent events accumulate as ScenarioEvents.
// Recording finalizes on message-complete or message-error.
//
// To capture a scenario from a real chat turn:
//   1. Open the desktop app with `?ttfi=record` in the URL
//   2. Send your query
//   3. Wait for the answer to finish streaming
//   4. In devtools console: window.__ttfi_recorder__.download('my-scenario')
//   5. Drop the downloaded .ts file into tests/e2e/scenarios/

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  Scenario,
  ScenarioEvent,
  NarrationPhase,
} from "./types";

type RecorderStatus = "inactive" | "idle" | "recording" | "finalized";

interface DocumentOperationPayload {
  type: "Routing" | "Retrieving" | "AnalysingEntity" | "Synthesising" | string;
  operation?: string;
  name?: string;
}

interface TurnNarrationPayload {
  session_id: string;
  conversation_id: string;
  event: { phase: NarrationPhase; text: string; elapsed_ms: number };
}

interface InterpretationProposedPayload {
  session_id: string;
  conversation_id: string;
  interpretation: string;
  alternatives: { label: string; intent_hint: string }[];
  confidence: number;
}

interface ClarificationRequestPayload {
  session_id: string;
  conversation_id: string;
  question: string;
  options: { label: string; follow_up: string; intent_hint: string }[];
}

interface MessageChunkPayload {
  message_id: string;
  chunk: string;
}

interface MessageCompletePayload {
  message_id: string;
  full_text: string;
  metadata?: unknown;
}

interface ErrorPayload {
  message: string;
}

class TtfiRecorder {
  status: RecorderStatus = "inactive";
  events: ScenarioEvent[] = [];
  query = "";
  private t0: number | null = null;
  private unlisteners: UnlistenFn[] = [];
  private clickHandler: ((e: MouseEvent) => void) | null = null;
  private fullText = "";
  private completeMetadata: unknown = null;

  /** Activate the recorder. Idempotent. */
  enable(): void {
    if (this.status !== "inactive") return;
    this.status = "idle";
    this.installClickHook();
    void this.installEventListeners();
    // eslint-disable-next-line no-console
    console.log(
      "[ttfi-recorder] active — click Send to begin recording. Use window.__ttfi_recorder__.download('name') after the turn completes.",
    );
  }

  /** Stop and tear down. Cleared events stay on the instance for export. */
  disable(): void {
    this.unlisteners.forEach((u) => {
      try {
        u();
      } catch {
        /* noop */
      }
    });
    this.unlisteners = [];
    if (this.clickHandler) {
      document.removeEventListener("click", this.clickHandler, true);
      this.clickHandler = null;
    }
    if (this.status !== "finalized") this.status = "inactive";
  }

  /** Manually start (alternative to click-driven). Useful for tests. */
  start(query: string): void {
    if (this.status === "inactive") this.enable();
    this.t0 = performance.now();
    this.query = query;
    this.events = [];
    this.fullText = "";
    this.completeMetadata = null;
    this.status = "recording";
  }

  /** Manually stop (the Tauri message-complete normally finalizes; use
   *  this when recording a turn that doesn't complete naturally — e.g.
   *  a clarification flow that ends on the user picking an option). */
  stop(): void {
    if (this.status !== "recording") return;
    this.status = "finalized";
  }

  /** Reset to idle without disabling. Use between turns. */
  reset(): void {
    this.events = [];
    this.query = "";
    this.t0 = null;
    this.fullText = "";
    this.completeMetadata = null;
    if (this.status === "finalized" || this.status === "recording") {
      this.status = "idle";
    }
  }

  /** Build a Scenario from the captured events. Caller supplies a
   *  display name; the recorder fills the rest from observed state. */
  exportScenario(name: string): Scenario {
    const lastKind = this.events[this.events.length - 1]?.kind;
    const terminal: Scenario["terminal"] =
      lastKind === "clarification"
        ? { kind: "selector-visible", selector: ".clarification-card" }
        : { kind: "send-btn-visible" };
    return {
      name,
      description: `Recorded from real session at ${new Date().toISOString()}`,
      query: this.query,
      events: this.events.slice(),
      terminal,
    };
  }

  /** Serialise the scenario as a TypeScript module string, ready to
   *  drop into tests/e2e/scenarios/. */
  exportScenarioTs(name: string): string {
    const scenario = this.exportScenario(name);
    const safeIdent = name.replace(/[^a-zA-Z0-9]+/g, "_").replace(/^_+|_+$/g, "");
    const camel = safeIdent.replace(/_+(.)/g, (_, c) => c.toUpperCase());
    const exportName = camel.charAt(0).toLowerCase() + camel.slice(1);
    return `import type { Scenario } from "../../../src/lib/ttfi/types";\n\n// Recorded from a real session. Edit budgets / description as needed.\nexport const ${exportName}: Scenario = ${JSON.stringify(scenario, null, 2)};\n`;
  }

  /** Trigger a download of the .ts file (browser only). */
  download(name: string): void {
    const content = this.exportScenarioTs(name);
    const blob = new Blob([content], { type: "text/typescript" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${name}.ts`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
  }

  // ── Internals ───────────────────────────────────────────────

  private installClickHook(): void {
    if (this.clickHandler) return;
    this.clickHandler = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      const send = target?.closest?.(".send-btn");
      if (!send) return;
      // Ignore Stop button or anything else that happens to share
      // the class somehow — the chat surface only uses .send-btn for
      // the actual send.
      const textarea = document.querySelector(
        ".input-area textarea",
      ) as HTMLTextAreaElement | null;
      const q = textarea?.value?.trim() ?? "";
      if (!q) return; // empty submit, ignore
      this.start(q);
    };
    // Capture phase so we run before any handler that might stop
    // propagation. We don't preventDefault; we only observe.
    document.addEventListener("click", this.clickHandler, true);
  }

  private async installEventListeners(): Promise<void> {
    const wireEvent = async <T>(
      eventName: string,
      record: (payload: T) => void,
    ) => {
      try {
        const un = await listen<T>(eventName, (e) => {
          if (this.status !== "recording" || this.t0 == null) return;
          try {
            record(e.payload);
          } catch (err) {
            console.warn(`[ttfi-recorder] record(${eventName}) failed:`, err);
          }
        });
        this.unlisteners.push(un);
      } catch (e) {
        // listen() rejects outside a Tauri runtime (vitest harness, SSR).
        // Recorder degrades to inert; harness tests drive it via start().
        console.debug(
          `[ttfi-recorder] listen(${eventName}) unavailable`,
          e,
        );
      }
    };

    await wireEvent<DocumentOperationPayload>(
      "document:operation",
      (payload) => {
        if (
          payload.type === "Routing" ||
          payload.type === "Retrieving" ||
          payload.type === "AnalysingEntity" ||
          payload.type === "Synthesising"
        ) {
          this.events.push({
            atMs: this.elapsed(),
            kind: "doc-op",
            type: payload.type,
            operation: payload.operation,
            name: payload.name,
          });
        }
      },
    );

    await wireEvent<TurnNarrationPayload>("turn-narration", (payload) => {
      this.events.push({
        atMs: this.elapsed(),
        kind: "narration",
        phase: payload.event.phase,
        text: payload.event.text,
      });
    });

    await wireEvent<InterpretationProposedPayload>(
      "interpretation-proposed",
      (payload) => {
        this.events.push({
          atMs: this.elapsed(),
          kind: "interpretation",
          interpretation: payload.interpretation,
          alternatives: payload.alternatives,
          confidence: payload.confidence,
        });
      },
    );

    await wireEvent<ClarificationRequestPayload>(
      "clarification-request",
      (payload) => {
        this.events.push({
          atMs: this.elapsed(),
          kind: "clarification",
          question: payload.question,
          options: payload.options,
        });
        // Clarification is a terminal state for synthesis-suppressed
        // turns. Mark finalized so the user can immediately export.
        this.status = "finalized";
      },
    );

    await wireEvent<MessageChunkPayload>("message-chunk", (payload) => {
      this.fullText += payload.chunk;
      this.events.push({
        atMs: this.elapsed(),
        kind: "chunk",
        text: payload.chunk,
      });
    });

    await wireEvent<MessageCompletePayload>("message-complete", (payload) => {
      this.events.push({
        atMs: this.elapsed(),
        kind: "complete",
        fullText: payload.full_text || this.fullText,
        metadata: payload.metadata,
      });
      this.completeMetadata = payload.metadata;
      this.status = "finalized";
    });

    await wireEvent<ErrorPayload>("message-error", (payload) => {
      this.events.push({
        atMs: this.elapsed(),
        kind: "error",
        message: payload.message,
      });
      this.status = "finalized";
    });
  }

  private elapsed(): number {
    return this.t0 == null ? 0 : performance.now() - this.t0;
  }
}

declare global {
  interface Window {
    __ttfi_recorder__?: TtfiRecorder;
  }
}

function shouldActivate(): boolean {
  if (typeof window === "undefined") return false;
  try {
    const url = new URL(window.location.href);
    if (url.searchParams.get("ttfi") === "record") return true;
  } catch {
    /* noop */
  }
  try {
    if (window.localStorage?.getItem("ttfi_record") === "1") return true;
  } catch {
    /* noop */
  }
  return false;
}

// Singleton — exported and also bound to window so devtools and tests
// can drive it without import.
export const ttfiRecorder = new TtfiRecorder();
if (typeof window !== "undefined") {
  window.__ttfi_recorder__ = ttfiRecorder;
  if (shouldActivate()) {
    ttfiRecorder.enable();
  }
}
