import type { Scenario } from "../fixtures/scenario-player";

// Fast-local: a direct chat answer with no corpus retrieval. Router
// commits quickly to "direct answer" and the primary slot starts
// streaming almost immediately. The dominant cost is decode time per
// token — there's nothing to surface as "specific intelligence" before
// the content arrives, because content IS the intelligence.
//
// Expected TTFI shape:
//   • generic   — instant (typing indicator on SEND_INITIATED)
//   • specific  — likely never (no doc-op, no narration in the slot)
//   • aux       — at narration time (~120ms)
//   • content   — first chunk lands ~250ms in
export const fastLocal: Scenario = {
  name: "fast-local",
  description: "Direct chat answer, no retrieval, fast first token",
  query: "what is 2 + 2",
  budgets: {
    generic: 200,
    aux: 500,
    content: 600,
  },
  events: [
    {
      atMs: 120,
      kind: "narration",
      phase: "routing_committed",
      text: "Direct answer — no corpus needed.",
    },
    { atMs: 250, kind: "chunk", text: "The " },
    { atMs: 280, kind: "chunk", text: "answer " },
    { atMs: 310, kind: "chunk", text: "is " },
    { atMs: 340, kind: "chunk", text: "four. " },
    ...Array.from({ length: 16 }, (_, i) => ({
      atMs: 370 + i * 30,
      kind: "chunk" as const,
      text: `more${i} `,
    })),
    {
      atMs: 370 + 16 * 30 + 50,
      kind: "complete",
      fullText:
        "The answer is four. " +
        Array.from({ length: 16 }, (_, i) => `more${i} `).join(""),
    },
  ],
  terminal: { kind: "send-btn-visible" },
};
