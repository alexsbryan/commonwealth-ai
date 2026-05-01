import type { Scenario } from "../fixtures/scenario-player";

// With-thinking: a reasoning-heavy turn where the model emits
// <think>...</think> tokens BEFORE prose. The desktop bridge passes
// every token through unchanged; AssistantMessage's parseAssistantContent
// is streaming-safe and renders an in-progress <ThinkBlock> as soon
// as the first chunk after `<think>` lands.
//
// What this scenario surfaces:
//   • `ttfi.thinking` should fire early (when the first think-tagged
//     chunk renders).
//   • `ttfi.content` should fire LATE — only after `</think>` plus the
//     first prose token. Without the thinking tier, `content` would
//     overstate "time to first content" by the entire thinking phase.
//   • `gap` is computed from `content - specific`, but for thinking
//     turns the more interesting value is `content - thinking` — open
//     follow-up: surface that delta directly in a future tier.
//
// Word-buffer note: WordBufferedStream flushes on whitespace, so each
// chunk here ends with a trailing space to ensure prompt rendering.
export const withThinking: Scenario = {
  name: "with-thinking",
  description:
    "Model streams <think>...</think> tokens before prose — thinking tier fires early, content fires late",
  query: "explain Frankfurt cases against the principle of alternate possibilities",
  budgets: {
    generic: 200,
    specific: 500,
    aux: 500,
    visible: 600,
    thinking: 700,
    content: 3000,
    // Big gap is expected here — the user sees the loading slot text
    // for ~2.5s while reasoning streams. Future UI work: rotate the
    // slot text, surface thinking content briefly, etc.
    gap: 3000,
  },
  events: [
    {
      atMs: 200,
      kind: "narration",
      phase: "primary_synthesis_start",
      text: "Drafting — this one needs real thinking.",
    },
    // Open think block. Trailing space flushes the buffer immediately.
    { atMs: 500, kind: "chunk", text: "<think>Considering " },
    { atMs: 550, kind: "chunk", text: "Frankfurt's " },
    { atMs: 600, kind: "chunk", text: "argument " },
    { atMs: 650, kind: "chunk", text: "carefully. " },
    { atMs: 700, kind: "chunk", text: "He claims " },
    { atMs: 750, kind: "chunk", text: "alternate " },
    { atMs: 800, kind: "chunk", text: "possibilities " },
    { atMs: 850, kind: "chunk", text: "are " },
    { atMs: 900, kind: "chunk", text: "neither " },
    { atMs: 950, kind: "chunk", text: "necessary " },
    { atMs: 1000, kind: "chunk", text: "nor " },
    { atMs: 1050, kind: "chunk", text: "sufficient " },
    { atMs: 1100, kind: "chunk", text: "for " },
    { atMs: 1150, kind: "chunk", text: "responsibility. " },
    ...Array.from({ length: 24 }, (_, i) => ({
      atMs: 1200 + i * 40,
      kind: "chunk" as const,
      text: `tok${i} `,
    })),
    // Close think block AND start prose in the same chunk (trailing
    // space flushes everything). After this lands, parseAssistantContent
    // sees a complete <think>...</think> block + a prose paragraph.
    { atMs: 1200 + 24 * 40 + 50, kind: "chunk", text: "</think>Yes, " },
    { atMs: 1200 + 24 * 40 + 90, kind: "chunk", text: "Frankfurt " },
    { atMs: 1200 + 24 * 40 + 130, kind: "chunk", text: "is " },
    { atMs: 1200 + 24 * 40 + 170, kind: "chunk", text: "right " },
    { atMs: 1200 + 24 * 40 + 210, kind: "chunk", text: "that " },
    ...Array.from({ length: 20 }, (_, i) => ({
      atMs: 1200 + 24 * 40 + 250 + i * 30,
      kind: "chunk" as const,
      text: `prose${i} `,
    })),
    {
      atMs: 1200 + 24 * 40 + 250 + 20 * 30 + 80,
      kind: "complete",
      fullText:
        "<think>full thinking content here</think>Yes, Frankfurt is right that " +
        Array.from({ length: 20 }, (_, i) => `prose${i} `).join(""),
    },
  ],
  terminal: { kind: "send-btn-visible" },
};
