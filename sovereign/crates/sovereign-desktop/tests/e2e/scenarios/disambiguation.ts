import type { Scenario } from "../fixtures/scenario-player";

// Disambiguation: low-confidence routing produces a clarification
// request — the runtime suppresses synthesis until the user picks an
// option or types freeform. From a TTFI perspective, the
// ClarificationCard IS the intelligence: the system understood the
// ambiguity and asked instead of guessing.
//
// Terminal state: clarification card visible. Test ends there; we
// don't simulate the user picking an option (that's a separate flow
// covered elsewhere).
//
// Expected TTFI shape:
//   • generic   — instant
//   • specific  — pre-tweak never; post-tweak ~150ms (routing narration)
//   • aux       — ~600ms (clarification card lands, also a narration chip)
//   • content   — never (no chunks)
export const disambiguation: Scenario = {
  name: "disambiguation",
  description:
    "Low-confidence routing emits a clarification card, suppressing synthesis",
  query: "what about that one",
  budgets: {
    generic: 200,
    aux: 800,
    specific: 500,
    visible: 800,
    // No content tier here — the scenario terminates at clarification.
    // Gap is therefore null and not budgeted.
  },
  events: [
    {
      atMs: 150,
      kind: "narration",
      phase: "routing_committed",
      text: "Not enough signal — asking for one detail.",
    },
    {
      atMs: 600,
      kind: "clarification",
      question:
        "I'm not sure which thread you mean. Which would you like me to follow up on?",
      options: [
        {
          label: "Frankfurt cases",
          follow_up: "tell me more about the Frankfurt cases",
          intent_hint: "deep_query",
        },
        {
          label: "Compatibilism",
          follow_up: "tell me more about compatibilism",
          intent_hint: "deep_query",
        },
        {
          label: "Free will more broadly",
          follow_up: "tell me more about free will",
          intent_hint: "deep_query",
        },
      ],
    },
  ],
  terminal: { kind: "selector-visible", selector: ".clarification-card" },
};
