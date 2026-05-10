// Dogfood tests for the runed `insightStore` module. Two purposes:
//
// 1. Exercise the Vitest + jsdom + Svelte 5 $state setup on a runed
//    module so the test pattern is proven before we write Phase 1 machine
//    tests.
// 2. Guard the `produce()` mutators against accidentally regressing
//    to in-place array mutation (which would re-introduce the class of
//    reactivity bugs Phase 0 was landed to fix).
import { describe, it, expect, beforeEach, vi } from "vitest";
import type { InsightNodeDto } from "../types";

// Mock the api module before importing the store — insightStore pulls
// listInsights / deleteInsight at module-load time.
vi.mock("../api", () => ({
  listInsights: vi.fn(),
  deleteInsight: vi.fn(),
}));

// Import lazily so the vi.mock hoist above takes effect first.
const { insightStore } = await import("./insights.svelte");
const api = await import("../api");

function fakeNode(id: string): InsightNodeDto {
  return {
    id,
    clipped_text: `clip-${id}`,
    message_id: `msg-${id}`,
    paragraph_index: 0,
    source: { corpus_id: null, article_title: null, conversation_id: "c1" },
    position: null,
    adjacent: [],
    created_at: "2026-01-01T00:00:00Z",
    sink_state: "Local",
  };
}

describe("insightStore", () => {
  beforeEach(async () => {
    // Reset the singleton's internal array by re-initing with []. Since
    // the module holds private $state, we can't reach in directly; the
    // cleanest reset is `init()` with a mocked empty list.
    vi.mocked(api.listInsights).mockResolvedValue([]);
    vi.mocked(api.deleteInsight).mockResolvedValue(undefined);
    await insightStore.init();
  });

  it("init() loads from the api and flips loading flag", async () => {
    const nodes = [fakeNode("a"), fakeNode("b")];
    vi.mocked(api.listInsights).mockResolvedValueOnce(nodes);
    await insightStore.init();
    expect(insightStore.count).toBe(2);
    expect(insightStore.loading).toBe(false);
  });

  it("add() produces a new array (structural sharing)", () => {
    const before = insightStore.items;
    insightStore.add(fakeNode("new"));
    const after = insightStore.items;
    expect(after).not.toBe(before);
    expect(after[0].id).toBe("new");
  });

  it("remove() produces a new array and calls the api", async () => {
    insightStore.add(fakeNode("keep"));
    insightStore.add(fakeNode("drop"));
    const before = insightStore.items;
    await insightStore.remove("drop");
    const after = insightStore.items;
    expect(after).not.toBe(before);
    expect(after.map((n) => n.id)).toEqual(["keep"]);
    expect(api.deleteInsight).toHaveBeenCalledWith("drop");
  });

  it("has() finds an added node by message id + paragraph index", () => {
    const n = fakeNode("p");
    insightStore.add(n);
    expect(insightStore.has(n.message_id, n.paragraph_index)).toBe(true);
    expect(insightStore.has("other-msg", 0)).toBe(false);
  });
});
