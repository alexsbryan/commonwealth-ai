// SPDX-License-Identifier: AGPL-3.0-or-later
// Unit tests for routingMachine. Drive the actor directly — no
// DOM, no Svelte, no singleton. Mirrors approval.machine.test.ts.
import { describe, it, expect, vi } from "vitest";
import { createActor, fromPromise } from "xstate";
import {
  applyNarration,
  applySynthesisProgress,
  routingMachine,
} from "./routing.machine";
import type {
  ClarificationRequestPayload,
  InterpretationProposedPayload,
  NarrationEvent,
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

describe("applyNarration — tool-invocation pairing", () => {
  function toolStart(
    callId: string,
    elapsed = 100,
    toolId = "document_operation",
  ): NarrationEvent {
    return {
      phase: {
        tool_invocation_start: {
          call_id: callId,
          tool_id: toolId,
          summary: `Analyzing ${callId}`,
        },
      },
      text: `Analyzing ${callId}`,
      elapsed_ms: elapsed,
    };
  }

  function toolComplete(
    callId: string,
    elapsed = 2000,
    ok = true,
    toolId = "document_operation",
  ): NarrationEvent {
    return {
      phase: {
        tool_invocation_complete: {
          call_id: callId,
          tool_id: toolId,
          ok,
          result_summary: ok ? `Done ${callId}` : `Failed ${callId}`,
        },
      },
      text: ok ? `Done ${callId}` : `Failed ${callId}`,
      elapsed_ms: elapsed,
    };
  }

  function retrievalStart(elapsed = 0): NarrationEvent {
    return {
      phase: "retrieval_start",
      text: "Searching your knowledge…",
      elapsed_ms: elapsed,
    };
  }

  it("appends a non-tool phase unchanged", () => {
    const next = applyNarration([], retrievalStart(50));
    expect(next).toHaveLength(1);
    expect(next[0].phase).toBe("retrieval_start");
  });

  it("appends a ToolInvocationStart on its own", () => {
    const next = applyNarration([], toolStart("c1"));
    expect(next).toHaveLength(1);
    expect(next[0].phase).toMatchObject({ tool_invocation_start: { call_id: "c1" } });
  });

  it("replaces matching Start when Complete arrives", () => {
    const afterStart = applyNarration([], toolStart("c1", 100));
    const afterComplete = applyNarration(afterStart, toolComplete("c1", 1800));
    expect(afterComplete).toHaveLength(1);
    expect(afterComplete[0].phase).toMatchObject({
      tool_invocation_complete: { call_id: "c1", ok: true },
    });
    expect(afterComplete[0].elapsed_ms).toBe(1800);
  });

  it("preserves order when Complete pairs with the middle of three Starts", () => {
    const log = [
      toolStart("a", 100),
      toolStart("b", 200),
      toolStart("c", 300),
    ];
    const next = applyNarration(log, toolComplete("b", 900));
    expect(next).toHaveLength(3);
    expect(next[0].phase).toMatchObject({ tool_invocation_start: { call_id: "a" } });
    expect(next[1].phase).toMatchObject({ tool_invocation_complete: { call_id: "b" } });
    expect(next[2].phase).toMatchObject({ tool_invocation_start: { call_id: "c" } });
  });

  it("appends Complete when no matching Start exists (defensive fallback)", () => {
    const next = applyNarration([retrievalStart()], toolComplete("orphan"));
    expect(next).toHaveLength(2);
    expect(next[1].phase).toMatchObject({
      tool_invocation_complete: { call_id: "orphan" },
    });
  });

  it("does not double-replace when a second Complete arrives for the same call_id", () => {
    // Once Start has been replaced by Complete, a later duplicate
    // Complete should append (defensive) rather than replace itself,
    // since the matching Start is gone. Prevents silent overwrite of
    // the original Complete metadata.
    const log = applyNarration([], toolStart("c1"));
    const afterFirst = applyNarration(log, toolComplete("c1", 1000, true));
    const afterDup = applyNarration(afterFirst, toolComplete("c1", 1100, false));
    expect(afterDup).toHaveLength(2);
    expect(afterDup[0].phase).toMatchObject({
      tool_invocation_complete: { call_id: "c1", ok: true },
    });
    expect(afterDup[1].phase).toMatchObject({
      tool_invocation_complete: { call_id: "c1", ok: false },
    });
  });

  it("appends ToolInvocationStart even when an unrelated Start exists", () => {
    const log = applyNarration([], toolStart("a"));
    const next = applyNarration(log, toolStart("b"));
    expect(next).toHaveLength(2);
    expect(next[0].phase).toMatchObject({ tool_invocation_start: { call_id: "a" } });
    expect(next[1].phase).toMatchObject({ tool_invocation_start: { call_id: "b" } });
  });

  it("does not mutate the input log array", () => {
    const original = [toolStart("c1")];
    const snapshot = [...original];
    applyNarration(original, toolComplete("c1"));
    expect(original).toEqual(snapshot);
  });

  it("does NOT append synthesis_progress heartbeat frames to the log", () => {
    // The heartbeat is a separate ticking field, not a stacked chip —
    // otherwise a 20s hold at ~4 frames/s floods the narration stack.
    const start: NarrationEvent = {
      phase: "primary_synthesis_start",
      text: "Writing your answer…",
      elapsed_ms: 5_000,
    };
    const hb: NarrationEvent = {
      phase: { synthesis_progress: { tokens: 42 } },
      text: "",
      elapsed_ms: 5_250,
    };
    const log = applyNarration([start], hb);
    expect(log).toHaveLength(1);
    expect(log[0].phase).toBe("primary_synthesis_start");
  });
});

describe("applySynthesisProgress — heartbeat reducer", () => {
  function heartbeat(tokens: number, elapsed: number): NarrationEvent {
    return {
      phase: { synthesis_progress: { tokens } },
      text: "",
      elapsed_ms: elapsed,
    };
  }

  it("sets the ticker from a synthesis_progress frame", () => {
    const next = applySynthesisProgress(null, heartbeat(12, 5_250));
    expect(next).toEqual({ tokens: 12, elapsedMs: 5_250 });
  });

  it("REPLACES the prior value as the count ticks up", () => {
    const first = applySynthesisProgress(null, heartbeat(12, 5_250));
    const second = applySynthesisProgress(first, heartbeat(58, 5_500));
    expect(second).toEqual({ tokens: 58, elapsedMs: 5_500 });
  });

  it("clears the ticker when a non-heartbeat phase hands off (grounding-verify)", () => {
    const active = applySynthesisProgress(null, heartbeat(140, 7_000));
    const handoff: NarrationEvent = {
      phase: "grounding_verify_start",
      text: "Checking every claim against your sources…",
      elapsed_ms: 7_200,
    };
    expect(applySynthesisProgress(active, handoff)).toBeNull();
  });

  it("stays null for an unrelated phase when no synthesis is active", () => {
    const retrieval: NarrationEvent = {
      phase: "retrieval_start",
      text: "Searching your knowledge…",
      elapsed_ms: 100,
    };
    expect(applySynthesisProgress(null, retrieval)).toBeNull();
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
