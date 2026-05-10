import type { Page } from "@playwright/test";
import type {
  NarrationPhase,
  Scenario as SharedScenario,
  ScenarioEvent as SharedScenarioEvent,
  ScenarioBudgets,
  ScenarioTerminal,
} from "../../../src/lib/ttfi/types";

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

// Re-exported from src/lib/ttfi/types so production-side recorder and
// harness-side player stay wire-compatible by construction. The
// shared-types module is the single source of truth for the on-the-wire
// Scenario shape; this fixture adds Playwright-specific helpers.
export type { NarrationPhase };
export type ScenarioEvent = SharedScenarioEvent;
export type Scenario = SharedScenario;
export type { ScenarioBudgets, ScenarioTerminal };

export type TtfiReport = {
  /** First .typing-indicator paint — any "we got your input" feedback. */
  generic: number | null;
  /** First .doc-progress-indicator paint — query-aware signal in the
   *  loading slot. The optimization target. */
  specific: number | null;
  /** First .narration-stack | .interpretation-banner | .clarification-card
   *  paint — query-aware signal anywhere. */
  aux: number | null;
  /** First time a specific-or-aux element actually intersects the
   *  viewport (IntersectionObserver). DOM presence ≠ visibility — a
   *  chip rendered far below the fold fires `aux` but not `visible`. */
  visible: number | null;
  /** First .think-block paint. Reasoning models stream <think>...</think>
   *  before prose; without this tier `content` understates first-content
   *  by the entire thinking duration. */
  thinking: number | null;
  /** First non-empty token text — traditional TTFT. */
  content: number | null;
  /** Derived: content − specific. The user-perceived wait window
   *  between "we have something specific to say" and "actual content
   *  arrives". null when either input is null. */
  gap: number | null;
  /** Max ms the loading-slot text was static (no change). Bounds
   *  sentence-stare: even when the slot has specific text, how long
   *  did the user see the same exact text without any update? null
   *  when the slot never appeared. */
  staleness: number | null;
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
