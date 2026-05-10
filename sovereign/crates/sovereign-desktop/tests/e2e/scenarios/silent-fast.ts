import type { Scenario } from "../fixtures/scenario-player";

// Silent-fast: the actual dot-stare case. No narration, no doc-op, no
// interpretation, no clarification — the runtime sends nothing into
// the loading slot. Just chunks arrive, eventually. This shape is
// real: the runtime suppresses narration below ~5s elapsed (per the
// NarrationChip suppression rule), and for short fast-path queries
// (think_budget=0, no retrieval) NOTHING fires before the first chunk.
//
// Without UI work to surface a placeholder, the user sees only typing
// dots from click until first content. THAT is dot-stare — and it's
// what every existing scenario in the harness happens to mask, because
// they all emit SOMETHING into the slot.
//
// Expected TTFI shape on the unmodified UI:
//   • generic   — instant (typing dots)
//   • specific  — NULL (nothing to render in the slot)
//   • aux       — NULL
//   • visible   — NULL (no specific/aux element to observe)
//   • thinking  — NULL
//   • content   — when first chunk renders (~750ms here)
//   • gap       — NULL (specific never fired)
//
// What we'd LIKE to see after a placeholder fix:
//   • specific  — fires around the placeholder threshold (~420ms)
//   • gap       — populates: content − placeholder
//
// This scenario is the dot-stare regression test from now on.
export const silentFast: Scenario = {
  name: "silent-fast",
  description:
    "No narration, no doc-op — only chunks. The actual dot-stare case the harness was missing.",
  query: "what time is it",
  budgets: {
    generic: 200,
    // After the placeholder fix lands, specific should fire ~430ms.
    // Pre-fix this stays null. The advisory budget tracks the target.
    specific: 600,
    visible: 700,
    content: 1000,
    gap: 700,
  },
  events: [
    // Long initial silence — no signal at all. This is the dot-stare
    // window. The runtime is busy (routing + retrieval + sampling) but
    // hasn't emitted anything user-visible.
    { atMs: 750, kind: "chunk", text: "It's " },
    { atMs: 800, kind: "chunk", text: "currently " },
    { atMs: 850, kind: "chunk", text: "2:30 " },
    { atMs: 900, kind: "chunk", text: "PM. " },
    {
      atMs: 1050,
      kind: "complete",
      fullText: "It's currently 2:30 PM. ",
    },
  ],
  terminal: { kind: "send-btn-visible" },
};
