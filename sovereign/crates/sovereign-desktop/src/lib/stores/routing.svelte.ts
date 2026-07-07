// SPDX-License-Identifier: AGPL-3.0-or-later
// Singleton wrapper around `routingMachine`. Mirrors
// `stores/approval.svelte.ts` to the letter — same lifecycle
// (lazy-start, long-lived), same $state subscription pattern, same
// actor-provide shape for real-Tauri vs test wiring.
//
// Every consumer (ChatView, InterpretationBanner, ClarificationCard,
// NarrationChip, FSM tests) reads from `routingStore.*` and dispatches
// via `routingStore.send(...)`. Components never call `invoke()` on
// the redirect / clarification paths; the machine invokes Tauri
// commands as actors so errors transition cleanly.
import { createActor, fromPromise, type Actor } from "xstate";
import { listen } from "@tauri-apps/api/event";
import { routingMachine } from "../machines/routing.machine";
import { redirectTurn, resumeSession } from "../api";
import type {
  ClarificationRequestPayload,
  InterpretationProposedPayload,
  TurnNarrationPayload,
} from "../types";

const wired = routingMachine.provide({
  actors: {
    submitRedirect: fromPromise(
      async ({
        input,
      }: {
        input: { sessionId: string; intentHint: string };
      }): Promise<{ message_id: string }> => {
        const res = await redirectTurn(input.sessionId, input.intentHint);
        return { message_id: res.message_id };
      },
    ),
    submitClarification: fromPromise(
      async ({
        input,
      }: {
        input: {
          sessionId: string;
          conversationId: string;
          followUp: string;
          intentHint: string;
        };
      }): Promise<{ message_id: string }> => {
        const res = await resumeSession(
          input.followUp,
          input.conversationId,
          input.sessionId,
          input.intentHint,
        );
        return { message_id: res.message_id };
      },
    ),
  },
});

const _actor: Actor<typeof wired> = createActor(wired);
type RoutingSnapshot = ReturnType<typeof _actor.getSnapshot>;

let _snapshot: RoutingSnapshot = $state(_actor.getSnapshot());
_actor.subscribe((snap) => {
  _snapshot = snap;
});
_actor.start();

// Install Tauri listeners at module load. The routing events fan
// into FSM events — components never call `listen()` themselves.
// `void` swallows the Promise; the listen handles persist for the
// app's lifetime (we never tear the store down).
//
// Testing note: in the Vitest harness there is no Tauri backend, so
// `listen()` rejects. We swallow the rejection silently — FSM unit
// tests drive events directly via `routingStore.send(...)` and don't
// need the listener bridge.
void (async () => {
  try {
    await listen<InterpretationProposedPayload>(
      "interpretation-proposed",
      (event) =>
        _actor.send({
          type: "INTERPRETATION_PROPOSED",
          payload: event.payload,
        }),
    );
    await listen<ClarificationRequestPayload>(
      "clarification-request",
      (event) =>
        _actor.send({
          type: "CLARIFICATION_REQUESTED",
          payload: event.payload,
        }),
    );
    await listen<TurnNarrationPayload>("turn-narration", (event) =>
      _actor.send({
        type: "TURN_NARRATION_EMITTED",
        payload: event.payload,
      }),
    );
  } catch (e) {
    // Expected outside Tauri (vitest harness, SSR). Components
    // still get a working store; just without live events.
    console.debug("routing.store: tauri listen() unavailable", e);
  }
})();

export const routingStore = {
  /** Reactive snapshot — reads `$state`, so consumers in .svelte
   *  components automatically re-render on updates. */
  get snapshot() {
    return _snapshot;
  },
  get proposed() {
    return _snapshot.context.proposed;
  },
  get clarification() {
    return _snapshot.context.clarification;
  },
  get narrationLog() {
    return _snapshot.context.narrationLog;
  },
  /** Live token-count heartbeat during the gated synthesis hold, or
   *  `null` when no synthesis is actively holding tokens. NarrationChip
   *  renders this as a ticking "writing… N tokens" pulse so a long
   *  grounded turn shows movement without leaking the held content. */
  get synthesisProgress() {
    return _snapshot.context.synthesisProgress;
  },
  /** New-assistant-message-id produced by the most recent successful
   *  redirect. ChatView watches this to wire up the chat.machine
   *  placeholder before the first chunk arrives; after consuming it,
   *  ChatView dispatches `ACKNOWLEDGE_REDIRECT` to clear. */
  get lastRedirectedMessageId() {
    return _snapshot.context.lastRedirectedMessageId;
  },
  /** Same bridge, clarification-submit side — produced when the
   *  user picks an option or submits valid freeform on a
   *  ClarificationCard. ChatView dispatches `REDIRECT_STARTED`
   *  to chat.machine (the event semantics are the same: install a
   *  new placeholder bubble) then acknowledges via
   *  `ACKNOWLEDGE_CLARIFIED`. */
  get lastClarifiedMessageId() {
    return _snapshot.context.lastClarifiedMessageId;
  },
  send(event: Parameters<Actor<typeof wired>["send"]>[0]) {
    _actor.send(event);
  },
};
