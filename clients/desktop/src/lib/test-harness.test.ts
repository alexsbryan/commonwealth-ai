// SPDX-License-Identifier: AGPL-3.0-or-later
// Smoke test for the Vitest + jsdom + jest-dom setup. Kept tiny — if this
// fails the whole test runner is broken and there's no point investigating
// feature-level tests.
import { describe, it, expect } from "vitest";
import { produce } from "immer";

describe("test harness", () => {
  it("jsdom is available", () => {
    const el = document.createElement("div");
    el.textContent = "hello";
    document.body.appendChild(el);
    expect(el).toBeInTheDocument();
    expect(el).toHaveTextContent("hello");
    document.body.removeChild(el);
  });

  it("immer produces new references on nested writes", () => {
    // Canonical pattern we'll apply to fix the provenance bug and to
    // every XState context update. Guard against the dependency regressing.
    const before = { messages: [{ id: "m1", metadata: null as unknown }] };
    const after = produce(before, (draft) => {
      draft.messages[0].metadata = { provenance: "x" };
    });
    expect(after).not.toBe(before);
    expect(after.messages).not.toBe(before.messages);
    expect(after.messages[0]).not.toBe(before.messages[0]);
    expect(before.messages[0].metadata).toBe(null);
    expect(after.messages[0].metadata).toEqual({ provenance: "x" });
  });
});
