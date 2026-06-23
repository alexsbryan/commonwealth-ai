// SPDX-License-Identifier: AGPL-3.0-or-later
// The journey manifest — the QA apparatus's prioritization spine, as
// DATA (ARCH_PRINCIPLES §6 / SICP data-vs-program). Each entry declares
// a canonical end-to-end user journey and its user-impact tier.
//
// The tier is load-bearing across the whole apparatus, not just this
// increment:
//   • Increment 1 (this file): the journey report groups acceptance
//     results by tier so "what matters most" is read first.
//   • Increment 2 (breaker personas): allocates its time budget
//     tier-first — exhaust Tier-1 surfaces adversarially before
//     spending a tick on Tier-4.
//   • Increment 3 (triage): ranks findings by severity × tier.
//
// Tiers (from the user-facing surface map):
//   1  every session; breakage = churn (chat, cancel, setup, citations)
//   2  session-persistent (ingest, enrichment, settings)
//   3  multi-session collaboration (mesh, shared inference)
//   4  power users (recipe authoring, atlas, inner work)
//   5  rare / admin (mobile pairing, diagnostics)
//
// Adding a journey: add its const here AND the spec under journeys/.
// The report keys off the result records (which embed id/tier/title),
// so a journey with no spec simply doesn't appear — honest by default.

export type Tier = 1 | 2 | 3 | 4 | 5;

export interface Journey {
  /** Stable id; appears in the test title, the result record, and the
   *  report. kebab-case, matches the spec filename stem. */
  id: string;
  /** One line a colleague could read to know what path is under test. */
  title: string;
  /** User-impact tier. The prioritization spine — see file header. */
  tier: Tier;
  /** Surfaces exercised, for coverage/triage rollups. */
  surfaces: string[];
}

// ── Tier 1: the core loop, hit every session ──

export const J_CHAT_CITATION: Journey = {
  id: "chat-citation",
  title: "Ask a grounded question, stream a reply, read a citation",
  tier: 1,
  surfaces: ["chat-stream", "citations", "reading-surface"],
};

export const J_CANCELLATION: Journey = {
  id: "cancellation",
  title: "Cancel a streaming reply and keep the session usable",
  tier: 1,
  surfaces: ["chat-stream", "cancellation"],
};

export const J_CORPUS_FILTER: Journey = {
  id: "corpus-filter",
  title: "Scope retrieval to a selected corpus (allow-list honored both ways)",
  tier: 1,
  surfaces: ["corpus-filter", "retrieval-scope"],
};

export const J_CONVERSATION_LIFECYCLE: Journey = {
  id: "conversation-lifecycle",
  title: "Create, rename, switch (history persists), and delete conversations",
  tier: 1,
  surfaces: ["conversations"],
};

export const J_FIRST_LAUNCH_SETUP: Journey = {
  id: "first-launch-setup",
  title: "First launch through setup and consent into a working chat",
  tier: 1,
  surfaces: ["setup", "consent", "chat-stream"],
};

/** All implemented journeys, in priority order. Tier-2 journeys
 *  (local-ingest, settings-persistence) land next — see the plan's
 *  fast-follow section. */
export const JOURNEYS: Journey[] = [
  J_CHAT_CITATION,
  J_CANCELLATION,
  J_CORPUS_FILTER,
  J_CONVERSATION_LIFECYCLE,
  J_FIRST_LAUNCH_SETUP,
];
