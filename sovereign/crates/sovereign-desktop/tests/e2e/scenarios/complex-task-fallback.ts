// SPDX-License-Identifier: AGPL-3.0-or-later
import type { Scenario } from "../fixtures/scenario-player";

// Complex-task-fallback: the desktop bridge's send_message_stream has
// a non-streaming fallback for ComplexTask intents (commands.rs:372).
// When the runtime's `handle_message_stream` returns "not streamable",
// the bridge calls `runtime.handle_message` — a single async call —
// and emits a single message-complete with the full text. No chunks.
//
// From the user's seat: typing dots → silence → wall of text. This is
// the worst-case TTFI shape current production can produce, and the
// harness needs to model it honestly so we know what the floor looks
// like.
//
// What this scenario surfaces:
//   • `specific` may or may not fire (depends on whether the runtime
//     emitted a narration before bailing to non-streaming). Modelled
//     as one routing-committed event so we still see SOMETHING in the
//     slot — without it, `specific` would be null and the slot would
//     show bare dots for the entire wait.
//   • `content` fires only at message-complete — long.
//   • `gap` is large by construction. This is the open optimization
//     target for the non-streaming path.
//   • `thinking` is null (no <think> tags in a single message-complete).
export const complexTaskFallback: Scenario = {
  name: "complex-task-fallback",
  description:
    "Non-streaming fallback inside send_message_stream — no chunks, message-complete after a long wait",
  query: "synthesise the case for compatibilism across all my sources",
  budgets: {
    generic: 200,
    specific: 500,
    aux: 500,
    visible: 600,
    content: 3500,
    // Documenting the shape, not aspiring to it. Without UI work this
    // path will always have a multi-second gap; the budget tracks
    // "how bad is the worst case today" rather than "what should it be".
    gap: 3500,
  },
  events: [
    {
      atMs: 150,
      kind: "narration",
      phase: "routing_committed",
      text: "Complex task — handing off to the deep path.",
    },
    // Long wait with NO chunks. Models the runtime crunching through
    // a non-streaming code path. The user sees only the narration in
    // the slot, holding for ~3 seconds.
    {
      atMs: 3200,
      kind: "complete",
      fullText:
        "Compatibilism, in its strongest form, holds that free will and " +
        "determinism are not in conflict. The argument runs as follows... " +
        "(this would be a 200-word answer in production). Sources: Frankfurt " +
        "1969, Strawson 1962, Dennett 1984.",
    },
  ],
  terminal: { kind: "send-btn-visible" },
};
