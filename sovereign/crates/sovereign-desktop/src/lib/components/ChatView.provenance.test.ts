// Regression test for the provenance bug: when `message-complete` fires
// with metadata, the in-place mutation + array-spread pattern used
// previously failed to propagate the new metadata reference to
// AssistantMessage's `$derived(metadata?.provenance)`. Phase 0
// replaced it with a single `produce()` call. This test locks the
// canonical pattern in place so a future "simplification" can't
// quietly regress it.
//
// We test the reducer shape in isolation (rather than mounting
// ChatView end-to-end) because mounting requires 8 Tauri listener
// mocks, a store, a runtime, conversation data — all overhead that
// would add fragility without catching anything the shape test misses.
// Phase 2's chat machine will make this a first-class actor test.
import { describe, it, expect } from "vitest";
import { produce } from "immer";
import type { MessageEntry } from "../types";

type Metadata = Record<string, unknown>;

interface CompletePayload {
  message_id: string;
  full_text: string;
  metadata?: Metadata;
}

/**
 * Mirror of the `message-complete` handler's write logic in
 * `ChatView.svelte`. Kept in its own file purely for testability; the
 * component inlines the `produce()` block. If that inline block changes,
 * this mirror must change too — otherwise the test guards a stale
 * pattern. Treat a mismatch between the two as a refactoring smell
 * that should move the reducer out of the component.
 */
export function applyMessageComplete(
  messages: MessageEntry[],
  p: CompletePayload,
  remaining: string,
): MessageEntry[] {
  const idx = messages.findIndex((m) => m.id === p.message_id);
  if (idx === -1) return messages;
  return produce(messages, (draft) => {
    if (remaining) draft[idx].content += remaining;
    if (draft[idx].content.length === 0) draft[idx].content = p.full_text;
    if (p.metadata) draft[idx].metadata = p.metadata;
  });
}

function seedMessage(id: string, content = ""): MessageEntry {
  return {
    id,
    role: "assistant",
    content,
    created_at: 0,
  };
}

describe("ChatView message-complete reducer (provenance bug regression)", () => {
  it("produces a new top-level array on metadata write", () => {
    const before = [seedMessage("m1", "hello")];
    const after = applyMessageComplete(
      before,
      { message_id: "m1", full_text: "hello", metadata: { provenance: "x" } },
      "",
    );
    // The headline invariant: a new array reference. Without this,
    // `{#each messages}` wouldn't re-key and the MessageBubble prop
    // would keep pointing at the old metadata object — the original bug.
    expect(after).not.toBe(before);
  });

  it("produces a new message object at the updated index", () => {
    const before = [seedMessage("m1"), seedMessage("m2")];
    const after = applyMessageComplete(
      before,
      { message_id: "m2", full_text: "done", metadata: { provenance: "p" } },
      "",
    );
    expect(after[1]).not.toBe(before[1]);
    // Structural sharing: untouched messages keep their identity so
    // their downstream `$derived` don't needlessly re-evaluate.
    expect(after[0]).toBe(before[0]);
  });

  it("attaches metadata without losing streamed content", () => {
    const before = [seedMessage("m1", "partial ")];
    const after = applyMessageComplete(
      before,
      { message_id: "m1", full_text: "ignored", metadata: { p: 1 } },
      "tail.",
    );
    expect(after[0].content).toBe("partial tail.");
    expect(after[0].metadata).toEqual({ p: 1 });
  });

  it("falls back to full_text when the streamed buffer was empty", () => {
    // Non-streaming intents (e.g. document ops) emit message-complete
    // without preceding chunks. The handler must fill the bubble from
    // `full_text` in that case.
    const before = [seedMessage("m1", "")];
    const after = applyMessageComplete(
      before,
      { message_id: "m1", full_text: "whole answer", metadata: { p: 2 } },
      "",
    );
    expect(after[0].content).toBe("whole answer");
  });

  it("no-ops on an unknown message id", () => {
    const before = [seedMessage("m1")];
    const after = applyMessageComplete(
      before,
      { message_id: "unknown", full_text: "x" },
      "",
    );
    // Same reference — no churn when nothing matches.
    expect(after).toBe(before);
  });
});
