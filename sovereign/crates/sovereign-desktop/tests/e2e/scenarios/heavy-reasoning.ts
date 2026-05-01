import type { Scenario } from "../fixtures/scenario-player";

// Heavy-reasoning: routing + retrieval are normal, but the primary
// model "thinks" for a long time before producing the first token
// (large reasoning model, high temperature, complex synthesis). This
// is the scenario where the user is most likely to assume the app has
// frozen — bare dots for 2+ seconds with no chunks coming.
//
// Expected TTFI shape:
//   • generic   — instant
//   • specific  — pre-tweak never; post-tweak ~150ms (routing narration)
//   • aux       — ~150ms (narration below)
//   • content   — ~2500ms (long synthesis pause before first token)
export const heavyReasoning: Scenario = {
  name: "heavy-reasoning",
  description:
    "Normal retrieval + long primary synthesis (the worst-case dot-stare)",
  query: "trace the lineage from Frankfurt cases to current debates on PAP",
  budgets: {
    generic: 200,
    aux: 400,
    specific: 500,
    visible: 500,
    content: 3000,
    // Gap is the open problem on this scenario. Today: ~2.4s of one
    // calm sentence sitting in the slot. Optimizations: rotating
    // narration text, pulsing accent, "still thinking" mode after
    // some threshold. Budget set generously; tighten as the UI tunes.
    gap: 2500,
  },
  events: [
    {
      atMs: 150,
      kind: "narration",
      phase: "routing_committed",
      text: "Deep query — pulling from your full philosophy corpus.",
    },
    {
      atMs: 600,
      kind: "narration",
      phase: "retrieval_complete",
      text: "Found 24 passages across 8 sources.",
    },
    {
      atMs: 800,
      kind: "narration",
      phase: "primary_synthesis_start",
      text: "Drafting — this one needs real thinking.",
    },
    // Long synthesis pause: 800ms → 2500ms with no chunks. The
    // dominant TTFI failure mode without the tweak.
    { atMs: 2500, kind: "chunk", text: "The " },
    { atMs: 2540, kind: "chunk", text: "Frankfurt " },
    { atMs: 2580, kind: "chunk", text: "cases " },
    ...Array.from({ length: 60 }, (_, i) => ({
      atMs: 2620 + i * 25,
      kind: "chunk" as const,
      text: `tok${i} `,
    })),
    {
      atMs: 2620 + 60 * 25 + 80,
      kind: "complete",
      fullText:
        "The Frankfurt cases " +
        Array.from({ length: 60 }, (_, i) => `tok${i} `).join(""),
    },
  ],
  terminal: { kind: "send-btn-visible" },
};
