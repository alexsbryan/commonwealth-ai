// Unit tests for routingMachine. Drive the actor directly — no
// DOM, no Svelte, no singleton. Mirrors approval.machine.test.ts.
import { describe, it, expect, vi } from "vitest";
import { createActor, fromPromise } from "xstate";
import { routingMachine } from "./routing.machine";
import type {
  ClarificationRequestPayload,
  InterpretationProposedPayload,
  TurnNarrationPayload,
} from "../types";

function proposedPayload(
  sessionId = "sess-1",
): InterpretationProposedPayload {
  return {
    session_id: sessionId,
    conversation_id: "conv-1",
    interpretation: "I'm reading this as a quick factual answer.",
    alternatives: [
      { label: "Walk me through it in depth", intent_hint: "deep_query" },
      { label: "Check my knowledge base", intent_hint: "knowledge_query" },
    ],
    confidence: 0.65,
  };
}

function clarificationPayload(
  sessionId = "sess-2",
): ClarificationRequestPayload {
  return {
    session_id: sessionId,
    conversation_id: "conv-1",
    question: "I could approach this a few ways — pick one?",
    options: [
      {
        label: "Walk me through it",
        follow_up: "walk me through the scheduler",
        intent_hint: "deep_query",
      },
      {
        label: "Look in my knowledge base",
        follow_up: "search for the scheduler docs",
        intent_hint: "knowledge_query",
      },
    ],
  };
}

function narrationPayload(elapsedMs = 6_000): TurnNarrationPayload {
  return {
    session_id: "sess-3",
    conversation_id: "conv-1",
    event: {
      phase: "retrieval_complete",
      text: "Found 8 chunks — 3 from one source.",
      elapsed_ms: elapsedMs,
    },
  };
}

function waitFor(
  actor: ReturnType<typeof createActor>,
  predicate: (snap: ReturnType<typeof actor.getSnapshot>) => boolean,
  timeoutMs = 1000,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const start = Date.now();
    const check = () => {
      if (predicate(actor.getSnapshot())) return resolve();
      if (Date.now() - start > timeoutMs) {
        return reject(
          new Error(
            `waitFor timeout. Last state: ${JSON.stringify(
              actor.getSnapshot().value,
            )}`,
          ),
        );
      }
      setTimeout(check, 5);
    };
    check();
  });
}

function makeMachine(opts: {
  submitRedirect?: (input: {
    sessionId: string;
    intentHint: string;
  }) => Promise<{ message_id: string }>;
  submitClarification?: (input: {
    sessionId: string;
    conversationId: string;
    followUp: string;
    intentHint: string;
  }) => Promise<{ message_id: string }>;
} = {}) {
  const redirectImpl =
    opts.submitRedirect ??
    (async () => ({ message_id: "msg-redirect" }) as const);
  const clarImpl =
    opts.submitClarification ??
    (async () => ({ message_id: "msg-x" }) as const);
  return routingMachine.provide({
    actors: {
      submitRedirect: fromPromise(
        ({ input }: { input: { sessionId: string; intentHint: string } }) =>
          redirectImpl(input),
      ),
      submitClarification: fromPromise(
        ({
          input,
        }: {
          input: {
            sessionId: string;
            conversationId: string;
            followUp: string;
            intentHint: string;
          };
        }) => clarImpl(input),
      ),
    },
  });
}

describe("routingMachine — proposing region", () => {
  it("starts idle with no proposed interpretation", () => {
    const actor = createActor(makeMachine());
    actor.start();
    expect(actor.getSnapshot().matches({ proposing: "idle" })).toBe(true);
    expect(actor.getSnapshot().context.proposed).toBeNull();
  });

  it("INTERPRETATION_PROPOSED transitions idle → pending with payload", () => {
    const actor = createActor(makeMachine());
    actor.start();
    const p = proposedPayload();
    actor.send({ type: "INTERPRETATION_PROPOSED", payload: p });
    expect(actor.getSnapshot().matches({ proposing: "pending" })).toBe(true);
    expect(actor.getSnapshot().context.proposed).toEqual(p);
  });

  it("REDIRECT_SUBMIT invokes submitRedirect and clears the banner", async () => {
    const submitRedirect = vi.fn(async () => ({ message_id: "m-redir" }));
    const actor = createActor(makeMachine({ submitRedirect }));
    actor.start();
    actor.send({
      type: "INTERPRETATION_PROPOSED",
      payload: proposedPayload("sess-A"),
    });
    actor.send({
      type: "REDIRECT_SUBMIT",
      sessionId: "sess-A",
      intentHint: "deep_query",
    });
    await waitFor(actor, (s) => s.matches({ proposing: "idle" }));
    expect(submitRedirect).toHaveBeenCalledWith({
      sessionId: "sess-A",
      intentHint: "deep_query",
    });
    expect(actor.getSnapshot().context.proposed).toBeNull();
  });

  it("clears banner even when submitRedirect rejects", async () => {
    const actor = createActor(
      makeMachine({
        submitRedirect: async () => {
          throw new Error("session gone");
        },
      }),
    );
    actor.start();
    actor.send({
      type: "INTERPRETATION_PROPOSED",
      payload: proposedPayload(),
    });
    actor.send({
      type: "REDIRECT_SUBMIT",
      sessionId: "sess-1",
      intentHint: "knowledge_query",
    });
    await waitFor(actor, (s) => s.matches({ proposing: "idle" }));
    expect(actor.getSnapshot().context.proposed).toBeNull();
  });

  it("DISMISS_PROPOSED from pending returns to idle and clears payload", () => {
    const actor = createActor(makeMachine());
    actor.start();
    actor.send({
      type: "INTERPRETATION_PROPOSED",
      payload: proposedPayload(),
    });
    actor.send({ type: "DISMISS_PROPOSED" });
    expect(actor.getSnapshot().matches({ proposing: "idle" })).toBe(true);
    expect(actor.getSnapshot().context.proposed).toBeNull();
  });

  it("last-wins when a second INTERPRETATION_PROPOSED arrives while pending", () => {
    const actor = createActor(makeMachine());
    actor.start();
    actor.send({
      type: "INTERPRETATION_PROPOSED",
      payload: proposedPayload("first"),
    });
    actor.send({
      type: "INTERPRETATION_PROPOSED",
      payload: proposedPayload("second"),
    });
    expect(actor.getSnapshot().context.proposed?.session_id).toBe("second");
  });
});

describe("routingMachine — clarifying region", () => {
  it("CLARIFICATION_REQUESTED + CLARIFICATION_SUBMIT roundtrip", async () => {
    const submitClarification = vi.fn(async () => ({ message_id: "m-1" }));
    const actor = createActor(makeMachine({ submitClarification }));
    actor.start();
    actor.send({
      type: "CLARIFICATION_REQUESTED",
      payload: clarificationPayload("sess-B"),
    });
    expect(actor.getSnapshot().matches({ clarifying: "pending" })).toBe(true);

    actor.send({
      type: "CLARIFICATION_SUBMIT",
      sessionId: "sess-B",
      conversationId: "conv-1",
      followUp: "walk me through the scheduler",
      intentHint: "deep_query",
    });
    await waitFor(actor, (s) => s.matches({ clarifying: "idle" }));
    expect(submitClarification).toHaveBeenCalledWith({
      sessionId: "sess-B",
      conversationId: "conv-1",
      followUp: "walk me through the scheduler",
      intentHint: "deep_query",
    });
    expect(actor.getSnapshot().context.clarification).toBeNull();
  });

  it("clears card even when submitClarification rejects", async () => {
    const actor = createActor(
      makeMachine({
        submitClarification: async () => {
          throw new Error("runtime failed");
        },
      }),
    );
    actor.start();
    actor.send({
      type: "CLARIFICATION_REQUESTED",
      payload: clarificationPayload(),
    });
    actor.send({
      type: "CLARIFICATION_SUBMIT",
      sessionId: "sess-2",
      conversationId: "conv-1",
      followUp: "anything",
      intentHint: "deep_query",
    });
    await waitFor(actor, (s) => s.matches({ clarifying: "idle" }));
    expect(actor.getSnapshot().context.clarification).toBeNull();
  });
});

describe("routingMachine — narrating region", () => {
  it("TURN_NARRATION_EMITTED appends to narrationLog", () => {
    const actor = createActor(makeMachine());
    actor.start();
    actor.send({ type: "TURN_NARRATION_EMITTED", payload: narrationPayload(5_200) });
    actor.send({ type: "TURN_NARRATION_EMITTED", payload: narrationPayload(7_500) });
    const log = actor.getSnapshot().context.narrationLog;
    expect(log).toHaveLength(2);
    expect(log[0].elapsed_ms).toBe(5_200);
    expect(log[1].elapsed_ms).toBe(7_500);
  });

  it("CLEAR_NARRATION empties the log", () => {
    const actor = createActor(makeMachine());
    actor.start();
    actor.send({ type: "TURN_NARRATION_EMITTED", payload: narrationPayload() });
    actor.send({ type: "TURN_NARRATION_EMITTED", payload: narrationPayload() });
    expect(actor.getSnapshot().context.narrationLog).toHaveLength(2);
    actor.send({ type: "CLEAR_NARRATION" });
    expect(actor.getSnapshot().context.narrationLog).toHaveLength(0);
  });
});

describe("routingMachine — parallel regions", () => {
  it("proposed and clarification can be pending concurrently", () => {
    const actor = createActor(makeMachine());
    actor.start();
    actor.send({
      type: "INTERPRETATION_PROPOSED",
      payload: proposedPayload("sp"),
    });
    actor.send({
      type: "CLARIFICATION_REQUESTED",
      payload: clarificationPayload("sc"),
    });
    expect(actor.getSnapshot().matches({ proposing: "pending" })).toBe(true);
    expect(actor.getSnapshot().matches({ clarifying: "pending" })).toBe(true);
    expect(actor.getSnapshot().context.proposed?.session_id).toBe("sp");
    expect(actor.getSnapshot().context.clarification?.session_id).toBe("sc");
  });

  it("resolving one region leaves the other alone", async () => {
    const actor = createActor(makeMachine());
    actor.start();
    actor.send({
      type: "INTERPRETATION_PROPOSED",
      payload: proposedPayload("sp"),
    });
    actor.send({
      type: "CLARIFICATION_REQUESTED",
      payload: clarificationPayload("sc"),
    });
    actor.send({
      type: "REDIRECT_SUBMIT",
      sessionId: "sp",
      intentHint: "deep_query",
    });
    await waitFor(actor, (s) => s.matches({ proposing: "idle" }));
    // Clarification should still be pending.
    expect(actor.getSnapshot().matches({ clarifying: "pending" })).toBe(true);
    expect(actor.getSnapshot().context.clarification?.session_id).toBe("sc");
  });

  it("narration survives across routing events", () => {
    const actor = createActor(makeMachine());
    actor.start();
    actor.send({ type: "TURN_NARRATION_EMITTED", payload: narrationPayload() });
    actor.send({
      type: "INTERPRETATION_PROPOSED",
      payload: proposedPayload(),
    });
    // Narration log shouldn't reset because proposing fires.
    expect(actor.getSnapshot().context.narrationLog).toHaveLength(1);
  });
});
