import type { Scenario } from "../fixtures/scenario-player";

// Knowledge-grounded: the typical "ask a real question against a corpus"
// shape. Router takes a moment to classify, retrieval runs, narration
// reports retrieval done, then primary synthesis kicks in and tokens
// stream. This is the scenario where TTFI most clearly shows the cost
// of the bare typing-dot indicator: ~1.2s of nothing-but-dots between
// click and first token, even though the system is doing real work.
//
// Expected TTFI shape:
//   • generic   — instant
//   • specific  — depends on UI: today never (no doc-op); after the
//                 narration-in-slot tweak, ~200ms (when routing-committed
//                 narration lands in the indicator slot)
//   • aux       — ~200ms (narration chip below bubble)
//   • content   — ~1200ms (first chunk after retrieval + synthesis)
export const knowledgeGrounded: Scenario = {
  name: "knowledge-grounded",
  description:
    "Corpus-backed query: routing → retrieval → synthesis → stream",
  query: "what does Frankfurt say about coercion and moral responsibility",
  budgets: {
    generic: 200,
    aux: 400,
    // Pre-tweak this never fires; post-tweak it should track narration.
    specific: 500,
    content: 1500,
  },
  events: [
    {
      atMs: 200,
      kind: "narration",
      phase: "routing_committed",
      text: "Reading your philosophy corpus.",
    },
    {
      atMs: 250,
      kind: "interpretation",
      interpretation:
        "Looking for Frankfurt's account of coercion and how it bears on moral responsibility.",
      alternatives: [
        { label: "Free will more broadly", intent_hint: "deep_query" },
        { label: "Coercion in legal philosophy", intent_hint: "deep_query" },
      ],
      confidence: 0.78,
    },
    {
      atMs: 800,
      kind: "narration",
      phase: "retrieval_complete",
      text: "Found 12 passages from 4 sources.",
    },
    {
      atMs: 1000,
      kind: "narration",
      phase: "primary_synthesis_start",
      text: "Drafting a grounded answer.",
    },
    { atMs: 1200, kind: "chunk", text: "Frankfurt " },
    { atMs: 1240, kind: "chunk", text: "argues " },
    { atMs: 1280, kind: "chunk", text: "that " },
    { atMs: 1320, kind: "chunk", text: "coercion " },
    { atMs: 1360, kind: "chunk", text: "undermines " },
    { atMs: 1400, kind: "chunk", text: "responsibility " },
    { atMs: 1440, kind: "chunk", text: "only " },
    { atMs: 1480, kind: "chunk", text: "when " },
    ...Array.from({ length: 30 }, (_, i) => ({
      atMs: 1520 + i * 25,
      kind: "chunk" as const,
      text: `body${i} `,
    })),
    {
      atMs: 1520 + 30 * 25 + 80,
      kind: "complete",
      fullText:
        "Frankfurt argues that coercion undermines responsibility only when " +
        Array.from({ length: 30 }, (_, i) => `body${i} `).join(""),
      metadata: {
        provenance: {
          intent: "deep_query",
          search_method: "atlas-tier",
          sources: [
            { title: "Frankfurt 1969", score: 0.91 },
            { title: "Frankfurt 1971", score: 0.86 },
          ],
          inference_backend: "local",
          total_latency_ms: 2400,
        },
      },
    },
  ],
  terminal: { kind: "send-btn-visible" },
};
