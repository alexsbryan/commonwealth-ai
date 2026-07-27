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
  // `synthesis_progress` frames are a live token-count heartbeat, not a
  // log entry — they arrive throttled (~4/s) during the gated synthesis
  // hold and would flood the chip stack. They land in a separate
  // `synthesisProgress` context field via `applySynthesisProgress`; the
  // log is left untouched here.
  if (isSynthesisProgress(phase) || isDraftDelta(phase)) {
    // Heartbeat-cadence frames (token counts, draft deltas) would flood
    // the chip stack — they land in dedicated context fields instead.
    return log;
  }
  if (isCounterFrame(phase)) {
    // Claim-check frames drive the verification counter (`counter`
    // field via `applyCounter`); mirrored chips would double-render
    // the same progress.
    return log;
  }
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

/** Narrowing guard for the `synthesis_progress` struct variant. */
function isSynthesisProgress(
  phase: NarrationEvent["phase"],
): phase is { synthesis_progress: { tokens: number } } {
  return (
    typeof phase === "object" &&
    phase !== null &&
    "synthesis_progress" in phase
  );
}

/** Narrowing guard for the `draft_delta` struct variant (draft-preview
 *  experiment — see types.ts). */
function isDraftDelta(
  phase: NarrationEvent["phase"],
): phase is { draft_delta: { delta: string } } {
  return typeof phase === "object" && phase !== null && "draft_delta" in phase;
}

/**
 * Pure reducer for the `draftPreview` accumulator. `draft_delta` frames
 * APPEND; every other frame leaves the preview untouched — the draft must
 * stay visible while the gate verifies (the whole point is bridging that
 * window), so only CLEAR_NARRATION (new user turn) resets it. ChatView
 * owns the visual state transitions (drafting → verifying → collapsed).
 */
export function applyDraftPreview(
  prev: string | null,
  incoming: NarrationEvent,
): string | null {
  const phase = incoming.phase;
  if (isDraftDelta(phase)) {
    return (prev ?? "") + phase.draft_delta.delta;
  }
  return prev;
}

/** Live token-count heartbeat during a gated synthesis hold. */
export interface SynthesisProgress {
  tokens: number;
  elapsedMs: number;
}

/**
 * Pure reducer for the `synthesisProgress` heartbeat field. A
 * `synthesis_progress` frame REPLACES the prior value (monotone token
 * count ticking up); ANY other narration frame means synthesis has
 * handed off to the next phase (grounding-verify, persist) or a new
 * turn began — clear the heartbeat so the normal chip stack takes over.
 *
 * Exported for unit tests; the FSM consumes it through the
 * `TURN_NARRATION_EMITTED` assign action.
 */
export function applySynthesisProgress(
  prev: SynthesisProgress | null,
  incoming: NarrationEvent,
): SynthesisProgress | null {
  const phase = incoming.phase;
  if (isSynthesisProgress(phase)) {
    return {
      tokens: phase.synthesis_progress.tokens,
      elapsedMs: incoming.elapsed_ms,
    };
  }
  // Draft-delta frames interleave with the heartbeat at the same cadence
  // (each beat may carry both) — they must not clear the live counter.
  if (isDraftDelta(phase)) {
    return prev;
  }
  return null;
}

/** One claim row on the verification counter. */
export interface ClaimRow {
  text: string;
  verdict: "pending" | "supported" | "unsupported";
}

/** The verification-counter state a grounded turn accumulates —
 *  everything CounterCard needs to render the Gather → Draft → Check
 *  stations. Built exclusively from live narration frames (glassbox:
 *  the card can never claim progress the backend didn't report). */
export interface CounterState {
  /** Retrieval station. Set by `retrieval_start` (searching) and the
   *  struct-form `retrieval_complete` (counts + titles). */
  retrieval: {
    complete: boolean;
    chunksIn: number;
    corpora: string[];
    topTitles: string[];
  } | null;
  /** Claim-check station. Opens on `claim_check_start` (possibly with
   *  an empty list — the audit-open frame precedes extraction), rows
   *  stamp via `claim_verdict`, `claim_revision_start` marks the
   *  corrective rewrite, `claim_check_complete` closes the pass. */
  check: {
    recheck: boolean;
    claims: ClaimRow[];
    revising: number | null;
    complete: { confirmed: number; flagged: number } | null;
  } | null;
  /** Evidence-shape verdict (2026-07-21 decline-UX work): what
   *  retrieval actually found, shown the moment it's known — the
   *  longest formerly-silent stretch of a turn. `earlyDecline` means
   *  the backend measured the evidence off-topic on both independent
   *  axes and took the fast honest-answer path without it. */
  evidence: {
    chunks: number;
    sources: number;
    topSimilarity: number | null;
    coverage: number;
    earlyDecline: boolean;
  } | null;
  /** Cold-slot load in progress. The primary model is being paged off
   *  disk before synthesis can produce a single token — measured at
   *  57-95s against 0.44s warm. Set by `model_load`; never cleared,
   *  because once a turn has paid this wait the explanation stays
   *  true for that turn's timeline. Absent on every warm turn. */
  modelLoad: { modelId: string; sizeBytes: number | null } | null;
  /** elapsed_ms of the most recent counter-relevant frame. */
  elapsedMs: number;
}

/** Narrowing guards for the counter frames. */
function isClaimCheckStart(
  phase: NarrationEvent["phase"],
): phase is { claim_check_start: { claims: string[]; recheck: boolean } } {
  return (
    typeof phase === "object" && phase !== null && "claim_check_start" in phase
  );
}
function isClaimVerdict(
  phase: NarrationEvent["phase"],
): phase is { claim_verdict: { index: number; supported: boolean } } {
  return typeof phase === "object" && phase !== null && "claim_verdict" in phase;
}
function isClaimRevisionStart(
  phase: NarrationEvent["phase"],
): phase is { claim_revision_start: { failed: number } } {
  return (
    typeof phase === "object" &&
    phase !== null &&
    "claim_revision_start" in phase
  );
}
function isClaimCheckComplete(
  phase: NarrationEvent["phase"],
): phase is { claim_check_complete: { confirmed: number; flagged: number } } {
  return (
    typeof phase === "object" &&
    phase !== null &&
    "claim_check_complete" in phase
  );
}
function isRetrievalComplete(phase: NarrationEvent["phase"]): phase is {
  retrieval_complete: {
    chunks_in: number;
    corpora: string[];
    top_titles?: string[];
  };
} {
  return (
    typeof phase === "object" && phase !== null && "retrieval_complete" in phase
  );
}

function isEvidenceCheck(phase: NarrationEvent["phase"]): phase is {
  evidence_check: {
    chunks: number;
    sources: number;
    top_similarity: number | null;
    coverage: number;
    early_decline: boolean;
  };
} {
  return (
    typeof phase === "object" && phase !== null && "evidence_check" in phase
  );
}
function isModelLoad(
  phase: NarrationEvent["phase"],
): phase is { model_load: { model_id: string; size_bytes: number | null } } {
  return typeof phase === "object" && phase !== null && "model_load" in phase;
}

/** True when the frame belongs to the verification counter (routed to
 *  `counter`, kept out of `narrationLog` — same contract as the
 *  synthesis heartbeat). */
export function isCounterFrame(phase: NarrationEvent["phase"]): boolean {
  return (
    isClaimCheckStart(phase) ||
    isClaimVerdict(phase) ||
    isClaimRevisionStart(phase) ||
    isClaimCheckComplete(phase) ||
    isEvidenceCheck(phase) ||
    isModelLoad(phase)
  );
}

/**
 * Pure reducer for the `counter` field. Only counter-relevant frames
 * mutate it; everything else passes through. Reset on CLEAR_NARRATION
 * (new user turn), like the rest of the narrating region.
 *
 * Exported for unit tests; the FSM consumes it through the
 * `TURN_NARRATION_EMITTED` assign action.
 */
export function applyCounter(
  prev: CounterState | null,
  incoming: NarrationEvent,
): CounterState | null {
  const phase = incoming.phase;
  const base: CounterState = prev ?? {
    retrieval: null,
    check: null,
    evidence: null,
    modelLoad: null,
    elapsedMs: 0,
  };
  if (isModelLoad(phase)) {
    const p = phase.model_load;
    return {
      ...base,
      modelLoad: { modelId: p.model_id, sizeBytes: p.size_bytes },
      elapsedMs: incoming.elapsed_ms,
    };
  }
  if (isEvidenceCheck(phase)) {
    const p = phase.evidence_check;
    return {
      ...base,
      evidence: {
        chunks: p.chunks,
        sources: p.sources,
        topSimilarity: p.top_similarity,
        coverage: p.coverage,
        earlyDecline: p.early_decline,
      },
      elapsedMs: incoming.elapsed_ms,
    };
  }
  if (phase === "retrieval_start") {
    return {
      ...base,
      retrieval: base.retrieval ?? {
        complete: false,
        chunksIn: 0,
        corpora: [],
        topTitles: [],
      },
      elapsedMs: incoming.elapsed_ms,
    };
  }
  if (isRetrievalComplete(phase)) {
    const p = phase.retrieval_complete;
    return {
      ...base,
      retrieval: {
        complete: true,
        chunksIn: p.chunks_in,
        corpora: p.corpora,
        topTitles: p.top_titles ?? [],
      },
      elapsedMs: incoming.elapsed_ms,
    };
  }
  if (isClaimCheckStart(phase)) {
    const p = phase.claim_check_start;
    // The audit-open frame (empty claims) must not wipe an already
    // extracted list — it only ensures the station is open.
    const claims: ClaimRow[] =
      p.claims.length > 0
        ? p.claims.map((text) => ({ text, verdict: "pending" as const }))
        : (base.check?.claims ?? []);
    return {
      ...base,
      check: {
        recheck: p.recheck,
        claims,
        revising: null,
        complete: null,
      },
      elapsedMs: incoming.elapsed_ms,
    };
  }
  if (isClaimVerdict(phase)) {
    if (!base.check) return prev;
    const { index, supported } = phase.claim_verdict;
    const claims = base.check.claims.slice();
    if (index < claims.length) {
      claims[index] = {
        ...claims[index],
        verdict: supported ? "supported" : "unsupported",
      };
    }
    return {
      ...base,
      check: { ...base.check, claims },
      elapsedMs: incoming.elapsed_ms,
    };
  }
  if (isClaimRevisionStart(phase)) {
    if (!base.check) return prev;
    return {
      ...base,
      check: { ...base.check, revising: phase.claim_revision_start.failed },
      elapsedMs: incoming.elapsed_ms,
    };
  }
  if (isClaimCheckComplete(phase)) {
    if (!base.check) return prev;
    return {
      ...base,
      check: {
        ...base.check,
        revising: null,
        complete: phase.claim_check_complete,
      },
      elapsedMs: incoming.elapsed_ms,
    };
  }
  return prev;
}

export interface RoutingContext {
  proposed: InterpretationProposedPayload | null;
  clarification: ClarificationRequestPayload | null;
  narrationLog: NarrationEvent[];
  /** Live token-count heartbeat during the gated synthesis hold. Set
   *  by throttled `synthesis_progress` frames (REPLACE, not append —
   *  see `applySynthesisProgress`), cleared when the next distinct
   *  narration phase arrives or a new turn starts. `null` when no
   *  synthesis is actively holding tokens. */
  synthesisProgress: SynthesisProgress | null;
  /** Draft-preview experiment: the accumulated UNVERIFIED draft text
   *  during a gated hold (`draft_delta` frames appended in order). Stays
   *  populated through the verify window; reset on CLEAR_NARRATION.
   *  `null` = experiment off or no draft in flight. */
  draftPreview: string | null;
  /** Verification-counter state for the in-flight grounded turn (see
   *  `CounterState`). `null` until a counter-relevant frame arrives;
   *  reset on CLEAR_NARRATION. */
  counter: CounterState | null;
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
    synthesisProgress: null,
    draftPreview: null,
    counter: null,
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
            synthesisProgress: ({ context, event }) =>
              applySynthesisProgress(
                context.synthesisProgress,
                event.payload.event,
              ),
            draftPreview: ({ context, event }) =>
              applyDraftPreview(context.draftPreview, event.payload.event),
            counter: ({ context, event }) =>
              applyCounter(context.counter, event.payload.event),
          }),
        },
        CLEAR_NARRATION: {
          actions: assign({
            narrationLog: () => [],
            synthesisProgress: () => null,
            draftPreview: () => null,
            counter: () => null,
          }),
        },
      },
    },
  },
});
