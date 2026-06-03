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
