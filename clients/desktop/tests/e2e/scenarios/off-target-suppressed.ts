// SPDX-License-Identifier: AGPL-3.0-or-later
import type { Scenario } from "../fixtures/scenario-player";

// Off-target-suppressed: retrieval ran but came back dispersed (no
// repeat sources, low scores). The runtime narrates the shape and the
// primary slot composes a "I couldn't find a confident answer" reply
// instead of synthesizing on shaky evidence.
//
// This scenario tests the ANTI-FLATTERING path — when the system has
// nothing solid to say, "we got this" should not mean "we faked it".
// TTFI here is about how quickly the user learns the system tried and
// didn't find what they need.
//
// Expected TTFI shape:
//   • generic   — instant
//   • specific  — pre-tweak never; post-tweak ~150ms (routing narration)
//   • aux       — ~700ms (off-target narration chip)
//   • content   — ~1200ms (the "I couldn't find" reply)
export const offTargetSuppressed: Scenario = {
  name: "off-target-suppressed",
  description:
    "Retrieval came back off-target — system reports the gap honestly",
  query: "what does the corpus say about quantum gravity",
  budgets: {
    generic: 200,
    aux: 900,
    specific: 500,
    visible: 500,
    content: 1500,
    gap: 1500,
  },
  events: [
    {
      atMs: 150,
      kind: "narration",
      phase: "routing_committed",
      text: "Searching, but this looks outside the corpus.",
    },
    {
      atMs: 700,
      kind: "narration",
      phase: "retrieval_complete",
      text: "Found 4 passages — but they're scattered and low-confidence.",
    },
    { atMs: 1100, kind: "chunk", text: "I " },
    { atMs: 1130, kind: "chunk", text: "didn't " },
    { atMs: 1160, kind: "chunk", text: "find " },
    { atMs: 1190, kind: "chunk", text: "a " },
    { atMs: 1220, kind: "chunk", text: "confident " },
    { atMs: 1250, kind: "chunk", text: "answer " },
    { atMs: 1280, kind: "chunk", text: "in " },
    { atMs: 1310, kind: "chunk", text: "your " },
    { atMs: 1340, kind: "chunk", text: "corpus. " },
    {
      atMs: 1450,
      kind: "complete",
      fullText: "I didn't find a confident answer in your corpus. ",
    },
  ],
  terminal: { kind: "send-btn-visible" },
};
