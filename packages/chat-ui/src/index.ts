// SPDX-License-Identifier: AGPL-3.0-or-later
// Barrel for the @sovereign/chat-ui shared package — the transport-
// agnostic chat render surface used by both sovereign-desktop and
// sovereign-mobile. Consumed as source via a Vite/tsconfig alias (no
// build step); see each app's vite.config + tsconfig `paths`.

export * from "./types";
export * from "./parse-message"; // BlockType, ContentBlock, parseAssistantContent
export * from "./stream-buffer"; // WordBufferedStream
export * from "./announce"; // completionAnnouncement (a11y: per-turn live-region wording)
export * from "./actions/dialog-focus"; // dialogFocus action (a11y: trap + focus restore)

// NOTE: files that import npm packages (the xstate chat FSM
// `chat.machine.ts`; the `marked`/`katex`/`highlight.js`-based
// `markdown.ts`) are intentionally NOT shared here. This package is
// consumed as *source* via a Vite/tsconfig alias, so its files resolve
// bare imports from the consuming app at bundle time — but `tsc`
// type-checks them from THIS directory, where there is no `node_modules`
// (xstate's generic inference collapses to `any`; `marked` is
// "module not found"). Sharing those cleanly needs hoisted deps (an
// npm workspace) — deferred. Each app keeps its own copy. What is
// shared here — the prop-driven leaf components + intra-package utils +
// types — is the bulk of the render surface and the highest-value
// duplication to eliminate.

export { default as RoutingMeta } from "./components/RoutingMeta.svelte";
export { default as SourceAttribution } from "./components/SourceAttribution.svelte";
export { default as SourcePopover } from "./components/SourcePopover.svelte";
export { default as ThinkBlock } from "./components/ThinkBlock.svelte";
export { default as NextStepButtons } from "./components/NextStepButtons.svelte";
export { default as EpistemicFooter } from "./components/EpistemicFooter.svelte";
// Ledger-derivation helpers (pure, unit-tested) exported for reuse + tests.
export {
  provKind,
  bandLabel,
  isUnverifiedRecall,
  routeLabel,
  verdictReceipt,
} from "./components/EpistemicFooter.svelte";
export type { ProvKind, VerdictReceipt } from "./components/EpistemicFooter.svelte";
