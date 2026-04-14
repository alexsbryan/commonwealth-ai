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
import type { MessageEntry, InformationRequestPayload } from "../types";

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
  it("SEND_START appends user msg + placeholder, enters streaming", () => {
    const actor = startActor();
    actor.send({
      type: "SEND_START",
      userMessage: userMsg("u1", "hi"),
      assistantMessageId: "a1",
    });
    const s = actor.getSnapshot();
    expect(s.matches({ turn: "streaming" })).toBe(true);
    expect(s.context.messages).toHaveLength(2);
    expect(s.context.messages[0].id).toBe("u1");
    expect(s.context.messages[1].id).toBe("a1");
    expect(s.context.messages[1].content).toBe("");
    expect(s.context.streamingMessageId).toBe("a1");
  });

  it("MESSAGE_CHUNK appends to the matching placeholder (structural share)", () => {
    const actor = startActor();
    actor.send({
      type: "SEND_START",
      userMessage: userMsg("u1"),
      assistantMessageId: "a1",
    });

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
    actor.send({
      type: "SEND_START",
      userMessage: userMsg("u1"),
      assistantMessageId: "a1",
    });
    actor.send({ type: "MESSAGE_CHUNK", messageId: "other", text: "noise" });
    expect(actor.getSnapshot().context.messages[1].content).toBe("");
  });

  it("MESSAGE_COMPLETE attaches metadata (the provenance-bug regression)", () => {
    const actor = startActor();
    actor.send({
      type: "SEND_START",
      userMessage: userMsg("u1"),
      assistantMessageId: "a1",
    });
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
    actor.send({
      type: "SEND_START",
      userMessage: userMsg("u1"),
      assistantMessageId: "a1",
    });
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

  it("MESSAGE_ERROR appends the error to the streamed content", () => {
    const actor = startActor();
    actor.send({
      type: "SEND_START",
      userMessage: userMsg("u1"),
      assistantMessageId: "a1",
    });
    actor.send({ type: "MESSAGE_CHUNK", messageId: "a1", text: "partial" });
    actor.send({ type: "MESSAGE_ERROR", error: "boom" });

    const s = actor.getSnapshot();
    expect(s.matches({ turn: "idle" })).toBe(true);
    expect(s.context.messages[1].content).toBe("partial\n\nError: boom");
    expect(s.context.streamingMessageId).toBeNull();
  });

  it("SEND_FAILED clears streamingMessageId and returns to idle", () => {
    const actor = startActor();
    actor.send({
      type: "SEND_START",
      userMessage: userMsg("u1"),
      assistantMessageId: "a1",
    });
    actor.send({
      type: "SEND_FAILED",
      assistantMessageId: "a1",
      error: "network",
    });
    expect(actor.getSnapshot().matches({ turn: "idle" })).toBe(true);
    expect(actor.getSnapshot().context.streamingMessageId).toBeNull();
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
      type: "SEND_START",
      userMessage: userMsg("u1"),
      assistantMessageId: "a1",
    });
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
