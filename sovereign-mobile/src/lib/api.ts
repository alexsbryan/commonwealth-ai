// SPDX-License-Identifier: AGPL-3.0-or-later
// Mobile invoke() bridge. Command names mirror the desktop `api.ts`
// contract; the Rust core implements them over the tailnet. Arg keys
// are camelCase — Tauri maps them to the snake_case Rust params.

import { invoke } from "@tauri-apps/api/core";
import type {
  ConversationSummary,
  ConversationView,
  CorpusRef,
  HostConnection,
} from "./types";

// ─── Host / connection ────────────────────────────────────────

export const addHostConnection = (
  displayName: string,
  tailnetAddress: string,
  tenantId: string,
  token: string,
  /** "tailnet" (default) or "iroh" — for iroh, tailnetAddress is the
   *  pairing string from the host's GET /status → iroh.dial
   *  (`<endpoint-id-hex>@<relay-url>`); no VPN needed. */
  endpointKind?: string,
): Promise<HostConnection> =>
  invoke("add_host_connection", {
    displayName,
    tailnetAddress,
    tenantId,
    token,
    endpointKind: endpointKind ?? null,
  });

export const listHostConnections = (): Promise<HostConnection[]> =>
  invoke("list_host_connections");

export const setDefaultHost = (id: string): Promise<void> =>
  invoke("set_default_host", { id });

/** Remove a host connection (token + rows). Removing the active host
 *  returns the app to the pairing screen — this is the "change host" path. */
export const removeHostConnection = (id: string): Promise<void> =>
  invoke("remove_host_connection", { id });

export const getConnectivity = (): Promise<string> => invoke("get_connectivity");

// ─── Conversations ─────────────────────────────────────────────

export const createConversation = (): Promise<string> => invoke("create_conversation");

export const listConversations = (): Promise<ConversationSummary[]> =>
  invoke("list_conversations");

export const getConversation = (conversationId: string): Promise<ConversationView | null> =>
  invoke("get_conversation", { conversationId });

export const deleteConversation = (conversationId: string): Promise<void> =>
  invoke("delete_conversation", { conversationId });

/** Kick off a streamed turn. Resolves once the stream task is launched;
 *  tokens arrive via the `message-chunk` / `message-complete` events
 *  (see `events.ts`). */
export const sendMessageStream = (
  conversationId: string,
  message: string,
): Promise<{ conversation_id: string }> =>
  invoke("send_message_stream", { conversationId, message });

// ─── Corpora / citations ──────────────────────────────────────

export const listCorpora = (): Promise<CorpusRef[]> => invoke("list_corpora");

export const resolveCitation = (
  corpusId: string,
  chunkId: string,
): Promise<string | null> => invoke("resolve_citation", { corpusId, chunkId });

/** One chunk in a reading window — the full passage text. */
export interface ReadChunk {
  chunk_id: number;
  content: string;
  title?: string | null;
  url?: string | null;
}

/** A cited passage + its surrounding context (the reader payload). */
export interface ReadingWindow {
  corpus_id: string;
  found: boolean;
  center?: ReadChunk | null;
  prev: ReadChunk[];
  next: ReadChunk[];
}

/** Open the reader for a citation: the full cited passage + context,
 *  fetched from the host (falls back to the cached snippet offline). */
export const readCitation = (
  corpusId: string,
  chunkId: string,
): Promise<ReadingWindow | null> =>
  invoke("read_citation", { corpusId, chunkId });
