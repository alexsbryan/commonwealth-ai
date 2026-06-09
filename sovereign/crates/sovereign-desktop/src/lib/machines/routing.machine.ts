// SPDX-License-Identifier: AGPL-3.0-or-later
// routingMachine — antifragile-routing UI side. Owns the three
// user-facing events the runtime emits when `decide_policy` steers
// away from a bare Commit move:
//
//   - `interpretation-proposed` (Moderate tier)  → proposing region
//   - `clarification-request`    (Low tier)       → clarifying region
//   - `turn-narration`           (phase boundary) → narrating region
//
// Shape intentionally mirrors `approval.machine.ts` so the patterns
// cross-pollinate cleanly: three parallel regions; each Tauri event
// arrives as an FSM event; user actions dispatch structured events
// that invoke Tauri commands as XState actors — components never
// call `invoke()` directly.
//
// Regions:
//
//   proposing:   idle ──(INTERPRETATION_PROPOSED)──▶ pending
//                pending ──(REDIRECT_SUBMIT)──▶ submitting
//                submitting ──(onDone|onError)──▶ idle  (also clears payload)
//                pending ──(DISMISS_PROPOSED)──▶ idle   (30s GC from chat.machine)
//
//   clarifying:  idle ──(CLARIFICATION_REQUESTED)──▶ pending
//                pending ──(CLARIFICATION_SUBMIT)──▶ submitting
//                submitting ──(onDone|onError)──▶ idle
//
//   narrating:   always-active log; TURN_NARRATION_EMITTED appends
//                to `context.narrationLog`. CLEAR_NARRATION resets
//                (fired by chat.machine on new user turn).
import { assign, fromPromise, setup } from "xstate";
import type {
  ClarificationRequestPayload,
  InterpretationProposedPayload,
  NarrationEvent,
  TurnNarrationPayload,
} from "../types";

/**
 * Pure reducer for `narrationLog`. Default behaviour is append;
 * `ToolInvocationComplete` frames look up the prior `ToolInvocationStart`
 * with matching `call_id` and replace it in place, so each tool call
 * surfaces as one chip that transitions from active → done rather than
 * two stacked chips. Defensive fallback: if no matching Start exists
 * (out-of-order delivery, stale Complete after CLEAR_NARRATION),
 * append so the user still sees something.
 *
 * Exported for unit tests; the FSM consumes it through the
 * `TURN_NARRATION_EMITTED` assign action.
 */
export function applyNarration(
  log: NarrationEvent[],
  incoming: NarrationEvent,
): NarrationEvent[] {
  const phase = incoming.phase;
  if (
    typeof phase === "object" &&
    phase !== null &&
    "tool_invocation_complete" in phase
  ) {
    const completeCallId = phase.tool_invocation_complete.call_id;
    const idx = log.findIndex((entry) => {
      const p = entry.phase;
      return (
        typeof p === "object" &&
        p !== null &&
        "tool_invocation_start" in p &&
        p.tool_invocation_start.call_id === completeCallId
      );
    });
    if (idx >= 0) {
      const next = log.slice();
      next[idx] = incoming;
      return next;
    }
  }
  return [...log, incoming];
}

export interface RoutingContext {
  proposed: InterpretationProposedPayload | null;
  clarification: ClarificationRequestPayload | null;
  narrationLog: NarrationEvent[];
  /** Set by submitRedirect's onDone to the `message_id` of the new
   *  assistant bubble the runtime just started streaming into.
   *  ChatView watches this and, on change, dispatches
   *  `REDIRECT_STARTED` to chat.machine so the chat FSM creates a
   *  placeholder bubble before the first chunk arrives. The ack
   *  event clears the field so the effect only fires once. */
  lastRedirectedMessageId: string | null;
  /** PR6 — clarification-submit's equivalent: when a user picks
   *  an option (or submits valid freeform text), the runtime
   *  returns a new `message_id` for the fresh stream. Same bridge
   *  mechanism as redirect — ChatView watches this and ensures
   *  chat.machine has a placeholder bubble before chunks arrive. */
  lastClarifiedMessageId: string | null;
}

export type RoutingEvent =
  // Tauri-forwarded events (the listener wrapper converts raw
  // payloads into these FSM events).
  | {
      type: "INTERPRETATION_PROPOSED";
      payload: InterpretationProposedPayload;
    }
  | {
      type: "CLARIFICATION_REQUESTED";
      payload: ClarificationRequestPayload;
    }
  | { type: "TURN_NARRATION_EMITTED"; payload: TurnNarrationPayload }
  // User-driven submissions. REDIRECT_SUBMIT carries the session_id
  // + chosen alternative's intent_hint; the runtime pulls the
  // original message text from the SessionStore, cancels the
  // sampler, and starts a replacement stream.
  // CLARIFICATION_SUBMIT carries the follow_up text + intent_hint
  // chosen by the user (from a clicked option OR free-text input).
  | { type: "REDIRECT_SUBMIT"; sessionId: string; intentHint: string }
  | {
      type: "CLARIFICATION_SUBMIT";
      sessionId: string;
      conversationId: string;
      followUp: string;
      intentHint: string;
    }
  // Life-cycle events from chat.machine.
  | { type: "CLEAR_NARRATION" }
  | { type: "DISMISS_PROPOSED" }
  /** PR6 — explicit dismiss of a pending ClarificationCard without
   *  submitting anything to the runtime. Fired by the card's
   *  "Never mind" button OR automatically when the user types a
   *  fresh message in the main input while a card is still open.
   *  Clears the clarifying region's context without invoking the
   *  resumeSession actor. */
  | { type: "DISMISS_CLARIFICATION" }
  /** Dispatched by ChatView after it has consumed
   *  `lastRedirectedMessageId` and dispatched its own
   *  `REDIRECT_STARTED` to chat.machine. Clears the field so the
   *  effect doesn't re-fire on next snapshot. */
  | { type: "ACKNOWLEDGE_REDIRECT" }
  /** Dispatched by ChatView after it has consumed
   *  `lastClarifiedMessageId`. Same shape as
   *  `ACKNOWLEDGE_REDIRECT` — resets the bridge field. */
  | { type: "ACKNOWLEDGE_CLARIFIED" };

export const routingMachine = setup({
  types: {
    context: {} as RoutingContext,
    events: {} as RoutingEvent,
  },
  actors: {
    // Cancel the sampler for a proposed-banner redirect AND start a
    // replacement stream against the chosen alternative intent. The
    // returned `message_id` identifies the fresh assistant bubble
    // the caller should render `message-chunk` events into.
    submitRedirect: fromPromise(
      async (_: {
        input: { sessionId: string; intentHint: string };
      }): Promise<{ message_id: string }> => {
        throw new Error("submitRedirect actor not provided");
      },
    ),
    // Resume a session with an explicit intent (from a
    // ClarificationCard click). Returns the new message_id so the
    // caller can correlate `message-chunk` / `message-complete`.
    submitClarification: fromPromise(
      async (_: {
        input: {
          sessionId: string;
          conversationId: string;
          followUp: string;
          intentHint: string;
        };
      }): Promise<{ message_id: string }> => {
        throw new Error("submitClarification actor not provided");
      },
    ),
  },
}).createMachine({
  id: "routing",
  type: "parallel",
  context: {
    proposed: null,
    clarification: null,
    narrationLog: [],
    lastRedirectedMessageId: null,
    lastClarifiedMessageId: null,
  },
  on: {
    ACKNOWLEDGE_REDIRECT: {
      actions: assign({ lastRedirectedMessageId: () => null }),
    },
    ACKNOWLEDGE_CLARIFIED: {
      actions: assign({ lastClarifiedMessageId: () => null }),
    },
  },
  states: {
    proposing: {
      initial: "idle",
      states: {
        idle: {
          on: {
            INTERPRETATION_PROPOSED: {
              target: "pending",
              actions: assign({
                proposed: ({ event }) => event.payload,
              }),
            },
          },
        },
        pending: {
          on: {
            // Last-write-wins if a second interpretation arrives
            // while one is pending (e.g. a redirect resolved and a
            // fresh Propose fired). Matches approval.machine.
            INTERPRETATION_PROPOSED: {
              actions: assign({
                proposed: ({ event }) => event.payload,
              }),
            },
            REDIRECT_SUBMIT: { target: "submitting" },
            DISMISS_PROPOSED: {
              target: "idle",
              actions: assign({ proposed: () => null }),
            },
          },
        },
        submitting: {
          invoke: {
            src: "submitRedirect",
            input: ({ event }) => {
              const e = event as Extract<
                RoutingEvent,
                { type: "REDIRECT_SUBMIT" }
              >;
              return { sessionId: e.sessionId, intentHint: e.intentHint };
            },
            onDone: {
              target: "idle",
              actions: assign({
                proposed: () => null,
                // Expose the new assistant `message_id` so ChatView
                // can wire up the chat.machine placeholder before
                // the first chunk streams in.
                lastRedirectedMessageId: ({ event }) => event.output.message_id,
              }),
            },
            onError: {
              // If the Tauri command fails (session already GC'd,
              // etc.), still clear the banner — we've told the user
              // we'd cancel; pretending otherwise would confuse.
              target: "idle",
              actions: assign({ proposed: () => null }),
            },
          },
        },
      },
    },
    clarifying: {
      initial: "idle",
      states: {
        idle: {
          on: {
            CLARIFICATION_REQUESTED: {
              target: "pending",
              actions: assign({
                clarification: ({ event }) => event.payload,
              }),
            },
          },
        },
        pending: {
          on: {
            CLARIFICATION_REQUESTED: {
              actions: assign({
                clarification: ({ event }) => event.payload,
              }),
            },
            CLARIFICATION_SUBMIT: { target: "submitting" },
            // PR6 — explicit dismiss: clear the card, don't invoke
            // resumeSession. Used by the "Never mind" button and by
            // ChatView when the user types a fresh message in the
            // main input (implicit dismiss).
            DISMISS_CLARIFICATION: {
              target: "idle",
              actions: assign({ clarification: () => null }),
            },
          },
        },
        submitting: {
          invoke: {
            src: "submitClarification",
            input: ({ event }) => {
              const e = event as Extract<
                RoutingEvent,
                { type: "CLARIFICATION_SUBMIT" }
              >;
              return {
                sessionId: e.sessionId,
                conversationId: e.conversationId,
                followUp: e.followUp,
                intentHint: e.intentHint,
              };
            },
            onDone: {
              target: "idle",
              actions: assign({
                clarification: () => null,
                // PR6 — expose the new assistant `message_id` so
                // ChatView can install a chat.machine placeholder
                // bubble before MESSAGE_CHUNK events arrive.
                // Otherwise chunks for the freshly-started stream
                // get dropped by the guard and the UI never shows
                // the response.
                lastClarifiedMessageId: ({ event }) =>
                  event.output.message_id,
              }),
            },
            onError: {
              // Clarification submit failure is recoverable from the
              // user's side — they can just type a fresh message. We
              // clear the card so the UI doesn't stay stuck.
              target: "idle",
              actions: assign({ clarification: () => null }),
            },
          },
        },
      },
    },
    narrating: {
      // Narration is a log, not a request/response. Append on event
      // and reset on new-turn. Tool-invocation Start/Complete pairs
      // are reconciled via `applyNarration` so the chip updates in
      // place rather than rendering two entries per tool call.
      on: {
        TURN_NARRATION_EMITTED: {
          actions: assign({
            narrationLog: ({ context, event }) =>
              applyNarration(context.narrationLog, event.payload.event),
          }),
        },
        CLEAR_NARRATION: {
          actions: assign({ narrationLog: () => [] }),
        },
      },
    },
  },
});
