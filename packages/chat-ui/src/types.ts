// SPDX-License-Identifier: AGPL-3.0-or-later
// Shared chat-render types consumed by the chat-ui components + FSM.
// Self-contained: this module imports nothing from any app, so the
// package carries no back-dependency on desktop or mobile. Each app's
// local `types.ts` re-exports these so existing `from "../types"`
// imports keep resolving (one source of truth, structural identity).

/** A rendered conversation message. `metadata` is the host's opaque
 *  provenance/citation blob (the desktop/mobile bridges project it for
 *  RoutingMeta / SourceAttribution). */
export interface MessageEntry {
  id: string;
  role: string;
  content: string;
  created_at: number;
  metadata?: Record<string, unknown>;
  /** True between the moment the user submits the search-now / paste
   *  affordance and the moment the refined message arrives. Drives the
   *  "Refining…" indicator. */
  refining?: boolean;
  /** Set on a refined bubble sourced from the search-now affordance;
   *  drives the "Augmented via web search" footer. */
  searchAugmentation?: SearchAugmentation;
}

/** Web-search augmentation footer payload. */
export interface SearchAugmentation {
  query: string;
  backend_id: string;
  sources: Array<{ title: string; url: string }>;
}

/** Producer discriminator for an information-request card. */
export type InformationRequestKind = "refinement" | "step_block";

/** Sent when the agent suspends a task to ask the user for a specific
 *  external piece of evidence. Rendered as a dedicated card. */
export interface InformationRequestPayload {
  task_id: string;
  step_id: number;
  key: string;
  current_understanding: string;
  gap: string;
  relevance: string;
  satisfying_source: string;
  search_hints: string[];
  kind: InformationRequestKind;
  /** Populated only for `step_block` cards; empty for `refinement`. */
  task_title: string;
}

/** A grounded follow-up offer rendered as a clickable chip under an
 *  assistant message. */
export interface NextStepOffer {
  label: string;
  description?: string | null;
  follow_up_query: string;
  session_ref?: string | null;
  intent_hint?: string | null;
}

/** Field-model position style for a clipped/attributed paragraph. */
export type PositionStyle =
  | "Compatibilism"
  | "HardIncompatibilism"
  | "Libertarianism"
  | { Custom: { bg: string; text: string; border: string } };

// ─── Epistemic ledger (EPISTEMIC_STATE.md) ────────────────────────
//
// TS mirror of the `sovereign-contracts` `epistemic` wire types
// (`sovereign/crates/sovereign-contracts/src/types/epistemic.rs`),
// serialized onto `MessageEntry.metadata.epistemic_state`. serde
// `rename_all = "snake_case"` with NO container tag → externally-tagged
// enums: unit variants are bare strings, data variants are single-key
// objects. Additive only; the ledger carries a `version` for opt-in
// reader changes. The desktop `EpistemicFooter` renders these.

/** The kind of demand facet. Deterministic v1 facets (I4 adds
 *  `stance`/`section`; kept forward-open with a string fallback). */
export type DemandFacet = "query" | "sub_question" | "entity" | (string & {});

/** How far the evidence pool got toward covering a demand. */
export type CoverageLevel = "supported" | "retrieved" | "absent";

/** One facet of what the question needs — the coverage contract. */
export interface Demand {
  facet: DemandFacet;
  text: string;
  covered: CoverageLevel;
}

/** Epistemic band of a recalled memory (derived from stored confidence). */
export type MemoryBand = "told_directly" | "inferred" | "tentative";

/** What verification a holding survived. */
export type Verification = "verified" | "failed_once" | "fail_open" | "unverified";

/** The basis of a holding — a closed set. Externally-tagged: data
 *  variants are single-key objects, `general_knowledge` is a bare
 *  string. Rendering paths MUST match on this so a memory recall can
 *  never render as document evidence (invariant I3). */
export type Provenance =
  | { corpus: { corpus_id: string | null; chunk_id: number | null } }
  | { memory: { band: MemoryBand; entry_id: string } }
  | "general_knowledge"
  | { tool_derived: { tool: string } };

/** One claim the answer asserts, with its basis and verification. */
export interface Holding {
  claim: string;
  provenance: Provenance;
  verification: Verification;
}

/** The cross-corpus coverage verdict behind a gap. */
export type GapCoverage = "topic_uncovered" | "claim_uncovered";

/** One acquisition conjecture — a concrete place to fetch the missing
 *  knowledge. Externally-tagged serde enum (unit variants are strings).
 *  Structurally identical to the Rust `AcquisitionRoute`. */
export type AcquisitionRoute =
  | "connect_folder"
  | "connect_vault"
  | "import_conversations"
  | { install_recipe: { recipe_id: string; name: string } }
  | { web_search: { queries: string[] } }
  | { provide_document: { kind: string } };

/** A demand the evidence never covered, with acquisition conjecture. */
export interface Gap {
  demand_idx: number;
  statement: string;
  coverage: GapCoverage;
  routes: AcquisitionRoute[];
}

/** The turn-level verdict, derived purely from holdings + gaps. */
export type TurnVerdict =
  | "grounded"
  | "mixed"
  | "memory_recall"
  | "general_knowledge"
  | "unverified"
  | "cannot_know_from_here";

/** The typed epistemic account of one answer turn. Lives on
 *  `metadata.epistemic_state`. */
export interface EpistemicState {
  version: number;
  demands: Demand[];
  holdings: Holding[];
  gaps: Gap[];
  verdict: TurnVerdict;
}
