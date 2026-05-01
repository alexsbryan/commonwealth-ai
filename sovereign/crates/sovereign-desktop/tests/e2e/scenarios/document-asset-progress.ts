import type { Scenario } from "../fixtures/scenario-player";

// Document-asset-progress: models the UX of the ask_document path.
// When a DocumentAsset is attached, ChatView routes through askDocument
// (commands.rs:2138, ChatView.svelte:534-580) which is non-streaming —
// returns the full response in one shot. During the wait, the
// DocumentAssetManager emits document:operation events at each stage
// (Routing → Retrieving → AnalysingEntity → Synthesising) which the
// ChatView listener turns into rotating docProgressText.
//
// We can't cleanly drive the actual ask_document invoke from the
// harness without faking an attached asset (which requires going
// through the DocumentPicker). So we model the USER-PERCEIVED UX
// instead: doc-op events flow through the same listener and update
// the same loading slot. The metric is what the user sees, not which
// code path produced it.
//
// What this scenario surfaces:
//   • `specific` fires reliably and updates multiple times — doc-op
//     paths have the richest stage signal in the codebase today.
//   • `content` is delayed until the final message lands — the user
//     waits through the whole map/reduce before seeing prose.
//   • `gap` is large but moderated by the rotating slot text — the
//     UI feels alive even though the answer hasn't arrived.
//   • `aux`, `thinking` are null on this path.
export const documentAssetProgress: Scenario = {
  name: "document-asset-progress",
  description:
    "Doc-asset path UX: rich document:operation events fill the slot, then full answer arrives at once",
  query: "summarize the key arguments in this paper",
  budgets: {
    generic: 200,
    specific: 400,
    visible: 500,
    content: 3500,
    gap: 3500,
  },
  events: [
    { atMs: 150, kind: "doc-op", type: "Routing", operation: "Routing" },
    { atMs: 800, kind: "doc-op", type: "Retrieving" },
    {
      atMs: 1600,
      kind: "doc-op",
      type: "AnalysingEntity",
      name: "Frankfurt 1969",
    },
    { atMs: 2400, kind: "doc-op", type: "Synthesising" },
    {
      atMs: 3200,
      kind: "complete",
      fullText:
        "The paper makes three central claims: (1) alternate possibilities " +
        "are not necessary for moral responsibility, (2) coercion undermines " +
        "responsibility only when it bypasses the agent's reasoning, and " +
        "(3) Frankfurt cases provide a counterexample to PAP.",
    },
  ],
  terminal: { kind: "send-btn-visible" },
};
