// SPDX-License-Identifier: AGPL-3.0-or-later
// Mobile types. Re-exports the shared chat-render types from
// @sovereign/chat-ui (so the copied `chat.machine.ts`'s `../types`
// imports resolve to one source of truth) and adds the client-owned +
// projection types the Rust commands return.

export type {
  MessageEntry,
  SearchAugmentation,
  InformationRequestKind,
  InformationRequestPayload,
  NextStepOffer,
  PositionStyle,
} from "@sovereign/chat-ui";

/** Client-owned (source of truth on device). The token is NOT here —
 *  it lives in the keychain. */
export interface HostConnection {
  id: string;
  display_name: string;
  /** Opaque transport address, interpreted per endpoint_kind.
   *  For "tailnet": MagicDNS name or overlay IP + port. */
  tailnet_address: string;
  /** How tailnet_address is interpreted; "tailnet" today, open set
   *  so future transports (dial-by-key) add kinds without a schema
   *  change. */
  endpoint_kind: string;
  is_default: boolean;
  last_status: "reachable" | "host_down" | "off_tailnet";
  created_at: number;
}

/** Cached projection — the spec's CORPUS_REF. */
export interface CorpusRef {
  corpus_id: string;
  display_name: string;
  category?: string | null;
  icon?: string | null;
  chunk_count: number;
  /** Privacy posture: "local" (private to this host) vs "mesh". */
  scope?: "local" | "mesh" | string | null;
  /** false = never sharded/gossiped to peers. */
  mesh_shared?: boolean;
}

/** Cached projection — the spec's RESPONSE_PROVENANCE. */
export interface Provenance {
  inference_backend: string;
  routing_tier?: string | null;
  ttft_ms?: number | null;
  total_ms?: number | null;
  sources?: { origin: string; count: number; from_peer?: string | null }[];
}

/** Cached projection — the spec's CITATION, carrying (corpus_id, chunk_id). */
export interface Citation {
  corpus_id: string;
  chunk_id: string;
  title?: string | null;
  snippet: string;
  score: number;
  rank: number;
}

export interface ConversationSummary {
  id: string;
  title?: string | null;
  created_at: number;
  updated_at: number;
  /** true once the host has indexed this conversation into the
   *  per-identity conversation corpus. */
  indexed_in_corpus?: boolean;
}

export interface MessageView {
  id: string;
  role: string;
  content: string;
  status?: string | null;
  created_at: number;
  provenance?: Provenance | null;
  citations: Citation[];
  /** `{ provenance, retrieved_chunks }` blob built host-client-side on
   *  hydrate so reopened messages render citations + resolve reader
   *  clicks like freshly-streamed ones (see Rust `attach_metadata`). */
  metadata?: Record<string, unknown> | null;
}

export interface ConversationView extends ConversationSummary {
  messages: MessageView[];
}

/** Mirror of the Rust connectivity monitor's `ConnState`. */
export type ConnState = "off_tailnet" | "host_down" | "host_busy" | "reachable";
