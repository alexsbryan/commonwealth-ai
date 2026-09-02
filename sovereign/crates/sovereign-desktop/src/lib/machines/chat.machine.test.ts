// SPDX-License-Identifier: AGPL-3.0-or-later
// Unit tests for chatMachine. Same no-DOM pattern as skills.machine.test.ts.
//
// These tests cover the exact race conditions that produced bugs in
// the old ad-hoc state:
//   - Provenance only appearing after conversation cycle
//   - Info-request cards being cleared by unrelated state updates
//   - Messages losing content after mid-stream error recovery
//   - Refinement clobbering an in-flight stream
//
// Each turns into a few lines of direct event-send + snapshot assertion.
import { describe, it, expect } from "vitest";
import { createActor } from "xstate";
import { chatMachine } from "./chat.machine";
import type {
  MessageEntry,
  InformationRequestPayload,
  LessonProposedPayload,
} from "../types";

function userMsg(id: string, content = "hello"): MessageEntry {
  return {
    id,
    role: "user",
    content,
    created_at: 0,
  };
}

function assistantMsg(id: string, content = ""): MessageEntry {
  return {
    id,
    role: "assistant",
    content,
    created_at: 0,
  };
}

function fakeInfoRequest(
  key = "r1",
): InformationRequestPayload {
  return {
    task_id: "t1",
    step_id: 0,
    key,
    current_understanding: "cu",
    gap: "gap text",
    relevance: "r",
    satisfying_source: "s",
    search_hints: [],
    kind: "refinement",
    task_title: "",
  };
}

function startActor() {
  const actor = createActor(chatMachine);
  actor.start();
  return actor;
}

describe("chatMachine — conversation lifecycle", () => {
  it("starts idle with empty state", () => {
    const actor = startActor();
    const s = actor.getSnapshot();
    expect(s.matches({ turn: "idle" })).toBe(true);
    expect(s.matches({ infoRequest: "idle" })).toBe(true);
    expect(s.context.messages).toEqual([]);
    expect(s.context.conversationId).toBeNull();
    expect(s.context.streamingMessageId).toBeNull();
    expect(s.context.pendingInfoRequest).toBeNull();
  });

  it("HYDRATE loads messages from disk", () => {
    const actor = startActor();
    const history = [userMsg("u1"), assistantMsg("a1", "answered")];
    actor.send({ type: "HYDRATE", conversationId: "c1", messages: history });
    expect(actor.getSnapshot().context.conversationId).toBe("c1");
    expect(actor.getSnapshot().context.messages).toEqual(history);
  });

  it("RESET wipes everything back to idle", () => {
    const actor = startActor();
    actor.send({
      type: "HYDRATE",
      conversationId: "c1",
      messages: [userMsg("u1")],
    });
    actor.send({ type: "RESET" });
    const s = actor.getSnapshot();
    expect(s.context.conversationId).toBeNull();
    expect(s.context.messages).toEqual([]);
  });
});

describe("chatMachine — streaming lifecycle", () => {
  // Helper: dispatch the new two-step optimistic flow that ChatView
  // uses. SEND_INITIATED appends the user bubble + enters `preparing`;
  // SEND_START appends the assistant placeholder + enters `streaming`.
  function startTurn(actor: ReturnType<typeof startActor>, opts?: {
    userId?: string;
    userContent?: string;
    assistantId?: string;
  }) {
    const userId = opts?.userId ?? "u1";
    const assistantId = opts?.assistantId ?? "a1";
    actor.send({
      type: "SEND_INITIATED",
      userMessage: userMsg(userId, opts?.userContent ?? "hi"),
    });
    actor.send({ type: "SEND_START", assistantMessageId: assistantId });
  }

  it("SEND_INITIATED appends user msg, enters preparing", () => {
    const actor = startActor();
    actor.send({
      type: "SEND_INITIATED",
      userMessage: userMsg("u1", "hi"),
    });
    const s = actor.getSnapshot();
    expect(s.matches({ turn: "preparing" })).toBe(true);
    expect(s.context.messages).toHaveLength(1);
    expect(s.context.messages[0].id).toBe("u1");
    expect(s.context.streamingMessageId).toBeNull();
  });

  it("SEND_START appends placeholder, enters streaming (after SEND_INITIATED)", () => {
    const actor = startActor();
    startTurn(actor);
    const s = actor.getSnapshot();
    expect(s.matches({ turn: "streaming" })).toBe(true);
    expect(s.context.messages).toHaveLength(2);
    expect(s.context.messages[0].id).toBe("u1");
    expect(s.context.messages[1].id).toBe("a1");
    expect(s.context.messages[1].content).toBe("");
    expect(s.context.streamingMessageId).toBe("a1");
  });

  it("SEND_FAILED in preparing appends an error bubble, returns to idle", () => {
    // Lock-down for the cold-daemon flow: create_conversation or
    // send_message_stream throw before any stream began. The user
    // message stays; an "Error: ..." assistant bubble follows; FSM
    // returns to idle so the user can retry.
    const actor = startActor();
    actor.send({
      type: "SEND_INITIATED",
      userMessage: userMsg("u1", "hi"),
    });
    actor.send({ type: "SEND_FAILED", error: "daemon unavailable" });

    const s = actor.getSnapshot();
    expect(s.matches({ turn: "idle" })).toBe(true);
    expect(s.context.messages).toHaveLength(2);
    expect(s.context.messages[0].id).toBe("u1");
    expect(s.context.messages[1].role).toBe("assistant");
    expect(s.context.messages[1].content).toBe("Error: daemon unavailable");
    expect(s.context.streamingMessageId).toBeNull();
  });

  it("MESSAGE_ERROR in preparing also recovers cleanly", () => {
    // Edge case: backend errored mid-handshake (rare, but possible
    // if the stream errors out before the start response lands). Same
    // recovery shape as SEND_FAILED.
    const actor = startActor();
    actor.send({
      type: "SEND_INITIATED",
      userMessage: userMsg("u1"),
    });
    actor.send({ type: "MESSAGE_ERROR", error: "boom" });

    const s = actor.getSnapshot();
    expect(s.matches({ turn: "idle" })).toBe(true);
    expect(s.context.messages[1].content).toBe("Error: boom");
  });

  it("MESSAGE_CHUNK appends to the matching placeholder (structural share)", () => {
    const actor = startActor();
    startTurn(actor);

    const before = actor.getSnapshot().context.messages;
    actor.send({ type: "MESSAGE_CHUNK", messageId: "a1", text: "first " });
    actor.send({ type: "MESSAGE_CHUNK", messageId: "a1", text: "second." });

    const after = actor.getSnapshot().context.messages;
    expect(after[1].content).toBe("first second.");
    // The per-event produce() calls must yield new array references so
    // consumer $derived re-evaluates.
    expect(after).not.toBe(before);
    expect(after[1]).not.toBe(before[1]);
  });

  it("ignores MESSAGE_CHUNK for a non-current stream id", () => {
    const actor = startActor();
    startTurn(actor);
    actor.send({ type: "MESSAGE_CHUNK", messageId: "other", text: "noise" });
    expect(actor.getSnapshot().context.messages[1].content).toBe("");
  });

  it("MESSAGE_COMPLETE attaches metadata (the provenance-bug regression)", () => {
    const actor = startActor();
    startTurn(actor);
    actor.send({ type: "MESSAGE_CHUNK", messageId: "a1", text: "answer " });
    const before = actor.getSnapshot().context.messages;

    actor.send({
      type: "MESSAGE_COMPLETE",
      messageId: "a1",
      fullText: "unused in streaming path",
      pendingText: "tail.",
      metadata: { provenance: { intent: "SimpleQuery" } },
    });
    const s = actor.getSnapshot();
    expect(s.matches({ turn: "idle" })).toBe(true);
    expect(s.context.messages[1].content).toBe("answer tail.");
    expect(s.context.messages[1].metadata).toEqual({
      provenance: { intent: "SimpleQuery" },
    });
    expect(s.context.streamingMessageId).toBeNull();
    // Different reference — the nested write produced a new top-level
    // array and a new message object. Without this, AssistantMessage's
    // $derived would never re-evaluate (the original bug).
    expect(s.context.messages).not.toBe(before);
    expect(s.context.messages[1]).not.toBe(before[1]);
  });

  it("MESSAGE_COMPLETE falls back to fullText when buffer is empty", () => {
    // Non-streaming intents (document ops) sometimes deliver
    // message-complete without chunks. fullText must populate the
    // bubble. The old ad-hoc code had this exact conditional;
    // preserving it is load-bearing.
    const actor = startActor();
    startTurn(actor);
    actor.send({
      type: "MESSAGE_COMPLETE",
      messageId: "a1",
      fullText: "whole answer",
      pendingText: "",
      metadata: { x: 1 },
    });
    expect(actor.getSnapshot().context.messages[1].content).toBe(
      "whole answer",
    );
  });

  it("MESSAGE_ERROR in streaming appends the error to streamed content", () => {
    const actor = startActor();
    startTurn(actor);
    actor.send({ type: "MESSAGE_CHUNK", messageId: "a1", text: "partial" });
    actor.send({ type: "MESSAGE_ERROR", error: "boom" });

    const s = actor.getSnapshot();
    expect(s.matches({ turn: "idle" })).toBe(true);
    expect(s.context.messages[1].content).toBe("partial\n\nError: boom");
    expect(s.context.streamingMessageId).toBeNull();
  });
});

describe("chatMachine — single-flight dispatch", () => {
  // covers: UI-9
  //
  // Dispatch is single-flight because `SEND_INITIATED` is accepted ONLY in
  // `turn.idle`. Nothing in the UI enforces that — the send button's disabled
  // state is a courtesy, and a fast double-click, an Enter keypress landing on
  // the same frame, or an automation driving the input all arrive before the
  // bridge has answered. The machine is the only place the guarantee lives.
  //
  // A refactor of the nested turn region that hoisted `SEND_INITIATED` to the
  // machine's top-level `on:` block would compile, pass every other test in
  // this file, and start two turns from one click — two user bubbles, two
  // assistant placeholders, and a `streamingMessageId` pointing at whichever
  // stream answered last.
  function send(actor: ReturnType<typeof startActor>, id: string, text: string) {
    actor.send({ type: "SEND_INITIATED", userMessage: userMsg(id, text) });
  }

  it("a second SEND_INITIATED during preparing appends nothing and starts no turn", () => {
    const actor = startActor();
    send(actor, "u1", "first");
    send(actor, "u2", "double-click");

    const s = actor.getSnapshot();
    expect(s.matches({ turn: "preparing" })).toBe(true);
    expect(s.context.messages).toHaveLength(1);
    expect(s.context.messages[0].id).toBe("u1");
    expect(s.context.streamingMessageId).toBeNull();
  });

  it("a second SEND_INITIATED during streaming appends nothing and starts no turn", () => {
    const actor = startActor();
    send(actor, "u1", "first");
    actor.send({ type: "SEND_START", assistantMessageId: "a1" });
    send(actor, "u2", "impatient");

    const s = actor.getSnapshot();
    expect(s.matches({ turn: "streaming" })).toBe(true);
    // The user bubble and the ONE assistant placeholder, nothing else.
    expect(s.context.messages.map((m) => m.id)).toEqual(["u1", "a1"]);
    expect(s.context.streamingMessageId).toBe("a1");
  });

  it("the in-flight user message survives a binding that lands between the two sends", () => {
    // The real sequence on a first turn in a new conversation: the user's
    // bubble is optimistic, `ensureConversation` resolves mid-flight, and a
    // second click arrives while the machine is still preparing.
    const actor = startActor();
    send(actor, "u1", "first");
    actor.send({ type: "CONVERSATION_BOUND", conversationId: "conv-new" });
    send(actor, "u2", "double-click");

    const s = actor.getSnapshot();
    expect(s.context.conversationId).toBe("conv-new");
    expect(s.context.messages).toHaveLength(1);
    expect(s.context.messages[0].content).toBe("first");
    expect(s.matches({ turn: "preparing" })).toBe(true);
  });

  it("the machine accepts the NEXT turn once the first one lands", () => {
    // The control: single-flight must not be a permanent lock. Without this,
    // a machine that dropped SEND_INITIATED everywhere would pass all three
    // assertions above.
    const actor = startActor();
    send(actor, "u1", "first");
    actor.send({ type: "SEND_START", assistantMessageId: "a1" });
    actor.send({
      type: "MESSAGE_COMPLETE",
      messageId: "a1",
      fullText: "done",
      pendingText: "",
    });
    expect(actor.getSnapshot().matches({ turn: "idle" })).toBe(true);

    send(actor, "u2", "second");
    const s = actor.getSnapshot();
    expect(s.matches({ turn: "preparing" })).toBe(true);
    expect(s.context.messages.map((m) => m.id)).toEqual(["u1", "a1", "u2"]);
  });
});

describe("chatMachine — conversation binding", () => {
  it("CONVERSATION_BOUND sets conversationId without touching messages", () => {
    // Lock-down: when ensureConversation creates a conversation
    // mid-turn (after the user has already optimistically pushed
    // their message via SEND_INITIATED), binding the new id MUST
    // NOT wipe the in-flight bubble. HYDRATE would; CONVERSATION_BOUND
    // is the surgical alternative.
    const actor = startActor();
    actor.send({
      type: "SEND_INITIATED",
      userMessage: userMsg("u1", "hi"),
    });
    actor.send({ type: "CONVERSATION_BOUND", conversationId: "conv-new" });

    const s = actor.getSnapshot();
    expect(s.context.conversationId).toBe("conv-new");
    expect(s.context.messages).toHaveLength(1);
    expect(s.context.messages[0].id).toBe("u1");
    // Still in preparing — binding doesn't change turn region.
    expect(s.matches({ turn: "preparing" })).toBe(true);
  });
});

describe("chatMachine — post-stream refinement", () => {
  it("MESSAGE_REFINED rewrites a completed bubble in place", () => {
    const actor = startActor();
    actor.send({
      type: "HYDRATE",
      conversationId: "c1",
      messages: [
        userMsg("u1"),
        assistantMsg("a1", "Initial corpus-only answer."),
      ],
    });
    actor.send({
      type: "MESSAGE_REFINED",
      conversationId: "c1",
      messageId: "a1",
      newContent: "Refined with user-pasted source.",
    });
    expect(actor.getSnapshot().context.messages[1].content).toBe(
      "Refined with user-pasted source.",
    );
  });

  it("ignores MESSAGE_REFINED for the currently-streaming message", () => {
    // Chaos invariant: refinement is a post-stream concept. If the
    // backend fires it for a message that's still streaming, accepting
    // it would replace partial content with the refined version and
    // subsequent chunks would append on top of that. Drop it.
    const actor = startActor();
    actor.send({
      type: "HYDRATE",
      conversationId: "c1",
      messages: [],
    });
    actor.send({ type: "SEND_INITIATED", userMessage: userMsg("u1") });
    actor.send({ type: "SEND_START", assistantMessageId: "a1" });
    actor.send({ type: "MESSAGE_CHUNK", messageId: "a1", text: "partial " });

    actor.send({
      type: "MESSAGE_REFINED",
      conversationId: "c1",
      messageId: "a1",
      newContent: "REFINED MID-STREAM",
    });
    // Still in streaming, content unchanged.
    expect(actor.getSnapshot().context.messages[1].content).toBe("partial ");
    expect(actor.getSnapshot().matches({ turn: "streaming" })).toBe(true);

    // After completion, refinement IS accepted (the streaming guard
    // now passes — streamingMessageId is null).
    actor.send({
      type: "MESSAGE_COMPLETE",
      messageId: "a1",
      fullText: "partial ",
      pendingText: "",
    });
    actor.send({
      type: "MESSAGE_REFINED",
      conversationId: "c1",
      messageId: "a1",
      newContent: "REFINED POST-STREAM",
    });
    expect(actor.getSnapshot().context.messages[1].content).toBe(
      "REFINED POST-STREAM",
    );
  });

  it("ignores MESSAGE_REFINED for a different conversation", () => {
    // Racy switch: user moved to conv B, but refinement for conv A
    // lands late. Would overwrite the wrong message if we didn't guard.
    const actor = startActor();
    actor.send({
      type: "HYDRATE",
      conversationId: "convB",
      messages: [assistantMsg("different-id", "B's answer")],
    });
    actor.send({
      type: "MESSAGE_REFINED",
      conversationId: "convA",
      messageId: "different-id",
      newContent: "Should not appear",
    });
    expect(actor.getSnapshot().context.messages[0].content).toBe("B's answer");
  });

  it("MESSAGE_REFINING sets refining=true on the targeted bubble", () => {
    const actor = startActor();
    actor.send({
      type: "HYDRATE",
      conversationId: "c1",
      messages: [
        userMsg("u1"),
        assistantMsg("a1", "Initial corpus-only answer."),
      ],
    });
    actor.send({ type: "MESSAGE_REFINING", messageId: "a1" });
    expect(actor.getSnapshot().context.messages[1].refining).toBe(true);
  });

  it("MESSAGE_REFINED clears the refining flag", () => {
    const actor = startActor();
    actor.send({
      type: "HYDRATE",
      conversationId: "c1",
      messages: [
        userMsg("u1"),
        assistantMsg("a1", "Initial corpus-only answer."),
      ],
    });
    actor.send({ type: "MESSAGE_REFINING", messageId: "a1" });
    actor.send({
      type: "MESSAGE_REFINED",
      conversationId: "c1",
      messageId: "a1",
      newContent: "Refined.",
    });
    const m = actor.getSnapshot().context.messages[1];
    expect(m.refining).toBe(false);
    expect(m.content).toBe("Refined.");
  });

  it("SEARCH_AUGMENTED stashes the augmentation on the targeted bubble", () => {
    const actor = startActor();
    actor.send({
      type: "HYDRATE",
      conversationId: "c1",
      messages: [
        userMsg("u1"),
        assistantMsg("a1", "Initial."),
      ],
    });
    actor.send({
      type: "SEARCH_AUGMENTED",
      messageId: "a1",
      augmentation: {
        query: "Mac Studio next gen",
        backend_id: "duckduckgo",
        sources: [
          { title: "Wikipedia", url: "https://en.wikipedia.org/wiki/Mac_Studio" },
        ],
      },
    });
    expect(actor.getSnapshot().context.messages[1].searchAugmentation).toEqual({
      query: "Mac Studio next gen",
      backend_id: "duckduckgo",
      sources: [
        { title: "Wikipedia", url: "https://en.wikipedia.org/wiki/Mac_Studio" },
      ],
    });
  });

  it("MESSAGE_REFINED preserves searchAugmentation set earlier", () => {
    // Order: SEARCH_AUGMENTED then MESSAGE_REFINED. The refine must
    // NOT wipe the augmentation footer — that's how the bubble keeps
    // its "this was a web-search refinement" provenance after the
    // content swap.
    const actor = startActor();
    actor.send({
      type: "HYDRATE",
      conversationId: "c1",
      messages: [userMsg("u1"), assistantMsg("a1", "Initial.")],
    });
    actor.send({
      type: "SEARCH_AUGMENTED",
      messageId: "a1",
      augmentation: {
        query: "q",
        backend_id: "duckduckgo",
        sources: [{ title: "T", url: "https://example.test/" }],
      },
    });
    actor.send({
      type: "MESSAGE_REFINED",
      conversationId: "c1",
      messageId: "a1",
      newContent: "Web-augmented answer.",
    });
    const m = actor.getSnapshot().context.messages[1];
    expect(m.content).toBe("Web-augmented answer.");
    expect(m.searchAugmentation?.query).toBe("q");
  });
});

describe("chatMachine — non-streaming assistant responses", () => {
  it("ASSISTANT_MESSAGE_RECEIVED appends a prebuilt message", () => {
    // Document-ask / web-search flows produce a fully-formed assistant
    // message without going through SEND_START → streaming.
    const actor = startActor();
    actor.send({
      type: "HYDRATE",
      conversationId: "c1",
      messages: [userMsg("u1")],
    });
    actor.send({
      type: "ASSISTANT_MESSAGE_RECEIVED",
      message: assistantMsg("a1", "document answer"),
    });
    expect(actor.getSnapshot().context.messages).toHaveLength(2);
    expect(actor.getSnapshot().context.messages[1].content).toBe(
      "document answer",
    );
  });
});

describe("chatMachine — info-request parallel region", () => {
  it("INFO_REQUEST_ARRIVED puts the region into pending", () => {
    const actor = startActor();
    const req = fakeInfoRequest();
    actor.send({ type: "INFO_REQUEST_ARRIVED", payload: req });
    expect(actor.getSnapshot().matches({ infoRequest: "pending" })).toBe(true);
    expect(actor.getSnapshot().context.pendingInfoRequest).toEqual(req);
  });

  it("CLEAR_INFO returns to idle and clears the payload", () => {
    const actor = startActor();
    actor.send({ type: "INFO_REQUEST_ARRIVED", payload: fakeInfoRequest() });
    actor.send({ type: "CLEAR_INFO" });
    expect(actor.getSnapshot().matches({ infoRequest: "idle" })).toBe(true);
    expect(actor.getSnapshot().context.pendingInfoRequest).toBeNull();
  });

  it("runs in parallel with streaming — info card survives chunk events", () => {
    // The old component had separate $state for each listener; a mid-
    // stream info-request could interact with a SetSomethingElse call
    // and clobber the pending card. Parallel regions make that race
    // structurally impossible.
    const actor = startActor();
    actor.send({
      type: "SEND_INITIATED",
      userMessage: userMsg("u1"),
    });
    actor.send({ type: "SEND_START", assistantMessageId: "a1" });
    actor.send({ type: "INFO_REQUEST_ARRIVED", payload: fakeInfoRequest() });
    actor.send({ type: "MESSAGE_CHUNK", messageId: "a1", text: "chunk" });
    actor.send({
      type: "MESSAGE_COMPLETE",
      messageId: "a1",
      fullText: "chunk",
      pendingText: "",
    });
    const s = actor.getSnapshot();
    expect(s.matches({ turn: "idle" })).toBe(true);
    expect(s.matches({ infoRequest: "pending" })).toBe(true);
    expect(s.context.pendingInfoRequest).not.toBeNull();
  });

  it("a second INFO_REQUEST_ARRIVED overwrites the pending one (last wins)", () => {
    const actor = startActor();
    actor.send({
      type: "INFO_REQUEST_ARRIVED",
      payload: fakeInfoRequest("first"),
    });
    actor.send({
      type: "INFO_REQUEST_ARRIVED",
      payload: fakeInfoRequest("second"),
    });
    expect(actor.getSnapshot().context.pendingInfoRequest?.key).toBe("second");
  });
});

// ─── TEACHABLE lesson-proposal parallel region ─────────────────────

function fakeLessonProposal(id = "draft-1"): LessonProposedPayload {
  return {
    id,
    conversation_id: "c1",
    message_id: "m1",
    display: "Keep answers short.",
    prompt_form: "",
    enforcement: "param",
    params: { soft_target_cap: 300 },
    taught_from: "keep answers shorter from now on",
  };
}

describe("chatMachine — lesson-proposal parallel region", () => {
  it("LESSON_PROPOSED puts the region into pending", () => {
    const actor = startActor();
    const proposal = fakeLessonProposal();
    actor.send({ type: "LESSON_PROPOSED", payload: proposal });
    expect(actor.getSnapshot().matches({ lessonProposal: "pending" })).toBe(
      true,
    );
    expect(actor.getSnapshot().context.pendingLessonProposal).toEqual(
      proposal,
    );
  });

  it("CLEAR_LESSON returns to idle and clears the payload", () => {
    const actor = startActor();
    actor.send({ type: "LESSON_PROPOSED", payload: fakeLessonProposal() });
    actor.send({ type: "CLEAR_LESSON" });
    expect(actor.getSnapshot().matches({ lessonProposal: "idle" })).toBe(true);
    expect(actor.getSnapshot().context.pendingLessonProposal).toBeNull();
  });

  it("survives a full streaming turn (parallel with `turn`)", () => {
    const actor = startActor();
    actor.send({ type: "SEND_INITIATED", userMessage: userMsg("u1") });
    actor.send({ type: "SEND_START", assistantMessageId: "a1" });
    actor.send({ type: "LESSON_PROPOSED", payload: fakeLessonProposal() });
    actor.send({ type: "MESSAGE_CHUNK", messageId: "a1", text: "chunk" });
    actor.send({
      type: "MESSAGE_COMPLETE",
      messageId: "a1",
      fullText: "chunk",
      pendingText: "",
    });
    const s = actor.getSnapshot();
    expect(s.matches({ turn: "idle" })).toBe(true);
    expect(s.matches({ lessonProposal: "pending" })).toBe(true);
    expect(s.context.pendingLessonProposal).not.toBeNull();
  });

  it("a second LESSON_PROPOSED overwrites the pending one (last wins)", () => {
    const actor = startActor();
    actor.send({
      type: "LESSON_PROPOSED",
      payload: fakeLessonProposal("first"),
    });
    actor.send({
      type: "LESSON_PROPOSED",
      payload: fakeLessonProposal("second"),
    });
    expect(actor.getSnapshot().context.pendingLessonProposal?.id).toBe(
      "second",
    );
  });

  it("HYDRATE while pending clears the card (no cross-conversation leak)", () => {
    // Pins the enumerated-reset discipline: every HYDRATE/RESET assign
    // must include pendingLessonProposal, or a card taught in one
    // conversation would follow the user into the next.
    const actor = startActor();
    actor.send({ type: "LESSON_PROPOSED", payload: fakeLessonProposal() });
    actor.send({ type: "HYDRATE", conversationId: "c2", messages: [] });
    const s = actor.getSnapshot();
    expect(s.matches({ lessonProposal: "idle" })).toBe(true);
    expect(s.context.pendingLessonProposal).toBeNull();
  });

  it("coexists with a pending info-request (both cards at once)", () => {
    // The only genuinely new region interaction: a gap card and a
    // lesson card pending simultaneously must not clobber each other.
    const actor = startActor();
    actor.send({ type: "INFO_REQUEST_ARRIVED", payload: fakeInfoRequest() });
    actor.send({ type: "LESSON_PROPOSED", payload: fakeLessonProposal() });
    let s = actor.getSnapshot();
    expect(s.matches({ infoRequest: "pending" })).toBe(true);
    expect(s.matches({ lessonProposal: "pending" })).toBe(true);
    // Clearing one leaves the other pending.
    actor.send({ type: "CLEAR_LESSON" });
    s = actor.getSnapshot();
    expect(s.matches({ lessonProposal: "idle" })).toBe(true);
    expect(s.matches({ infoRequest: "pending" })).toBe(true);
    expect(s.context.pendingInfoRequest).not.toBeNull();
  });
});
