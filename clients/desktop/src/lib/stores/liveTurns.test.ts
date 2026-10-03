// SPDX-License-Identifier: AGPL-3.0-or-later
// Reducer tests for the live-turns registry — the per-conversation
// streaming state that survives a conversation switch. The ordering
// edge cases (early-arrival chunk before begin; a superseding turn) are
// exactly the ones that would silently corrupt a re-attach, so they are
// pinned here alongside the happy path. The Tauri listener + re-attach
// wiring is covered end-to-end by tests/e2e/specs/chat-orphaned-turn.
import { describe, it, expect, beforeEach } from "vitest";
import { liveTurns } from "./liveTurns.svelte";

const A = "conv-A";

describe("liveTurns registry", () => {
  beforeEach(() => liveTurns.reset());

  it("accumulates chunks under a begun turn", () => {
    liveTurns.begin(A, "m1");
    liveTurns.chunk(A, "m1", "Hello ");
    liveTurns.chunk(A, "m1", "world");
    const t = liveTurns.get(A);
    expect(t).toMatchObject({ messageId: "m1", text: "Hello world", status: "streaming" });
  });

  it("upserts a chunk that races ahead of begin, and begin does not wipe it", () => {
    // Early-arrival: a fast handler emits before SEND_START calls begin.
    liveTurns.chunk(A, "m1", "early ");
    liveTurns.begin(A, "m1"); // same id → must NOT reset the accumulated text
    liveTurns.chunk(A, "m1", "late");
    expect(liveTurns.get(A)?.text).toBe("early late");
  });

  it("resets when a genuinely new turn supersedes the old one", () => {
    liveTurns.chunk(A, "m1", "old answer");
    liveTurns.begin(A, "m2"); // different id → new turn
    expect(liveTurns.get(A)).toMatchObject({ messageId: "m2", text: "", status: "streaming" });
    liveTurns.chunk(A, "m2", "new");
    expect(liveTurns.get(A)?.text).toBe("new");
  });

  it("completes with the accumulated text and carries metadata", () => {
    liveTurns.chunk(A, "m1", "answer text");
    liveTurns.complete(A, "m1", "answer text", { intent: "KnowledgeQuery" });
    const t = liveTurns.get(A);
    expect(t).toMatchObject({ status: "done", text: "answer text" });
    expect(t?.metadata).toEqual({ intent: "KnowledgeQuery" });
  });

  it("falls back to full_text on complete when no chunks were observed", () => {
    // A re-attach can miss the chunks but still see the terminal event.
    liveTurns.complete(A, "m1", "the whole answer");
    expect(liveTurns.get(A)).toMatchObject({ status: "done", text: "the whole answer" });
  });

  it("preserves the partial text when a turn errors", () => {
    liveTurns.chunk(A, "m1", "partial ");
    liveTurns.error(A, "m1", "peer connection reset");
    expect(liveTurns.get(A)).toMatchObject({
      status: "error",
      text: "partial ",
      error: "peer connection reset",
    });
  });

  it("get() is null-safe and clear() removes the entry", () => {
    expect(liveTurns.get(null)).toBeUndefined();
    expect(liveTurns.get("nope")).toBeUndefined();
    liveTurns.begin(A, "m1");
    expect(liveTurns.get(A)).toBeDefined();
    liveTurns.clear(A);
    expect(liveTurns.get(A)).toBeUndefined();
    // Idempotent.
    liveTurns.clear(A);
    expect(liveTurns.get(A)).toBeUndefined();
  });
});
