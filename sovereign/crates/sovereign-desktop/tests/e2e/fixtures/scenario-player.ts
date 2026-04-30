import type { Page } from "@playwright/test";

// Scenario player — replays a backend timing scenario at the Tauri
// event boundary, on a controllable in-page timeline. Used by the TTFI
// harness to compare how quickly the UI surfaces each tier of
// "intelligence signal" under different backend shapes.
//
// Timing model: every event has an `atMs` offset from the click anchor
// (`window.__ttfi__.getT0()`). The player schedules each event with
// setTimeout inside the page, so timing matches what the chat machine
// and probe see. The probe uses the same anchor, so latencies line up.
//
// Why we don't use a Node-side scheduler with `page.waitForTimeout`:
// per-event Playwright IPC adds ~50ms of jitter that swamps small TTFI
// deltas. In-page setTimeout is sub-millisecond.

export type NarrationPhase =
  | "routing_committed"
  | "retrieval_complete"
  | "primary_synthesis_start"
  | "gap_check_fired";

export type ScenarioEvent =
  | {
      atMs: number;
      kind: "doc-op";
      type: "Routing" | "Retrieving" | "AnalysingEntity" | "Synthesising";
      operation?: string;
      name?: string;
    }
  | { atMs: number; kind: "narration"; phase: NarrationPhase; text: string }
  | {
      atMs: number;
      kind: "interpretation";
      interpretation: string;
      alternatives: { label: string; intent_hint: string }[];
      confidence: number;
    }
  | {
      atMs: number;
      kind: "clarification";
      question: string;
      options: { label: string; follow_up: string; intent_hint: string }[];
    }
  | { atMs: number; kind: "chunk"; text: string }
  | { atMs: number; kind: "complete"; fullText: string; metadata?: unknown }
  | { atMs: number; kind: "error"; message: string };

export type Scenario = {
  /** Stable identifier — used in report rows + test titles. */
  name: string;
  /** One-line description for human readers of the report. */
  description: string;
  /** User-input text — typed into the textarea before clicking Send. */
  query: string;
  /** Timeline of backend-emitted events. atMs is from the click anchor. */
  events: ScenarioEvent[];
  /** What state to wait for before reading the TTFI report. */
  terminal:
    | { kind: "send-btn-visible" }
    | { kind: "selector-visible"; selector: string };
  /** Optional advisory budgets (ms). The spec console.warns when a
   *  marker exceeds its budget; tests don't fail on overrun. Promote
   *  to hard `expect()` once the metric stabilizes. */
  budgets?: {
    generic?: number;
    specific?: number;
    aux?: number;
    content?: number;
  };
};

export type TtfiReport = {
  generic: number | null;
  specific: number | null;
  aux: number | null;
  content: number | null;
};

export type PlayContext = {
  conversationId: string;
  messageId: string;
  /** Optional override; auto-generated if omitted. */
  sessionId?: string;
};

declare global {
  interface Window {
    __ttfi__: {
      markStart(): void;
      getReport(): TtfiReport;
      getT0(): number | null;
      reset(): void;
    };
  }
}

/** Run a scenario in-page. Schedules every event relative to the
 *  click anchor (`window.__ttfi__.getT0()`) and resolves once every
 *  event has fired. The caller is then responsible for waiting on the
 *  scenario's terminal selector before reading the TTFI report. */
export async function playScenario(
  page: Page,
  ctx: PlayContext,
  scenario: Scenario,
): Promise<void> {
  await page.evaluate(
    async (args: { ctx: PlayContext; events: ScenarioEvent[] }) => {
      const { ctx, events } = args;

      const t0 = window.__ttfi__.getT0();
      // Anchor the schedule at t0 if available; otherwise use now.
      // markStart() is supposed to fire just before the Send click, so
      // by the time playScenario runs we've already crossed a few ms;
      // the schedule compensates by computing `wait = atMs - elapsed`.
      const anchor = t0 ?? performance.now();
      const sessionId =
        ctx.sessionId ?? `session-${Math.random().toString(36).slice(2, 10)}`;

      const fireEvent = (ev: ScenarioEvent) => {
        const api = window.__sovereign_test__;
        switch (ev.kind) {
          case "doc-op":
            api.emit("document:operation", {
              type: ev.type,
              operation: ev.operation,
              name: ev.name,
            });
            break;
          case "narration":
            api.emit("turn-narration", {
              session_id: sessionId,
              conversation_id: ctx.conversationId,
              event: {
                phase: ev.phase,
                text: ev.text,
                elapsed_ms: ev.atMs,
              },
            });
            break;
          case "interpretation":
            api.emit("interpretation-proposed", {
              session_id: sessionId,
              conversation_id: ctx.conversationId,
              interpretation: ev.interpretation,
              alternatives: ev.alternatives,
              confidence: ev.confidence,
            });
            break;
          case "clarification":
            api.emit("clarification-request", {
              session_id: sessionId,
              conversation_id: ctx.conversationId,
              question: ev.question,
              options: ev.options,
            });
            break;
          case "chunk":
            api.emit("message-chunk", {
              message_id: ctx.messageId,
              chunk: ev.text,
            });
            break;
          case "complete":
            api.completeMessage(ctx.messageId, ev.fullText, ev.metadata);
            break;
          case "error":
            api.errorMessage(ev.message);
            break;
        }
      };

      const promises = events.map(
        (ev) =>
          new Promise<void>((resolve) => {
            const elapsed = performance.now() - anchor;
            const wait = Math.max(0, ev.atMs - elapsed);
            setTimeout(() => {
              try {
                fireEvent(ev);
              } catch (e) {
                // Don't throw inside the scheduled callback — the
                // pageerror watcher would catch it and fail the test
                // with the wrong cause. Log and continue.
                console.error("[scenario-player] fireEvent failed:", e);
              }
              resolve();
            }, wait);
          }),
      );

      await Promise.all(promises);
    },
    { ctx, events: scenario.events },
  );
}
