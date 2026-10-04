// SPDX-License-Identifier: AGPL-3.0-or-later
// Live-turns registry — a runed singleton that keeps per-conversation
// streaming state ALIVE across conversation switches.
//
// Why this exists
// ---------------
// `chat.machine` owns exactly ONE conversation's `messages` +
// `streamingMessageId`, and wipes `streamingMessageId` to null on every
// HYDRATE (i.e. every conversation switch). The global Tauri stream
// events (`message-chunk` / `message-complete` / `message-error`) each
// carry a `conversation_id`, but the machine filtered only on
// `messageId` — so any event whose turn was NOT the visible one was
// silently dropped. A slow turn (e.g. synthesis offloaded to a mesh
// peer) that the user navigated away from was orphaned: on return there
// was no assistant row (the backend persists it only after the stream
// ends), no loading affordance (streamingMessageId was null), and the
// dropped completion never rendered.
//
// This registry is fed by those `conversation_id`-tagged events
// REGARDLESS of which conversation is on screen. ChatView reads it when
// a conversation loads and re-attaches the turn — restoring the loading
// affordance, the partial text streamed so far, and the final answer
// even when it landed while the user was elsewhere.
//
// Scope boundary
// --------------
// This lives as long as ChatView is mounted. It survives conversation
// switches (which do NOT unmount ChatView). It is intentionally NOT
// durable across an app restart or a navigation that unmounts ChatView
// (Settings / Atlas) — that durability belongs to the store (a
// persisted assistant row), a deliberately separate concern.
//
// Invariant: a conversation has at most one in-flight turn at a time,
// and `begin()` overwrites, so the map is naturally bounded to the
// number of conversations touched this session (one small entry each).

export type LiveTurnStatus = "streaming" | "done" | "error";

export interface LiveTurn {
  conversationId: string;
  messageId: string;
  /** Raw accumulated assistant text (pre word-buffer smoothing) so a
   *  re-attach can restore everything streamed so far in one shot. */
  text: string;
  status: LiveTurnStatus;
  /** Provenance/metadata from `message-complete` (retrieved chunks,
   *  finish_reason, intent, …). Present once the turn is `done`. */
  metadata?: Record<string, unknown>;
  /** Error tail when `status === "error"`. */
  error?: string;
}

let _turns: Record<string, LiveTurn> = $state({});

function put(conversationId: string, turn: LiveTurn): void {
  _turns = { ..._turns, [conversationId]: turn };
}

export const liveTurns = {
  /** Snapshot of all tracked turns. Reactive. */
  get all(): Record<string, LiveTurn> {
    return _turns;
  },

  /** Register a fresh turn (called when SEND_START resolves the assistant
   *  message id). No-ops if we're already tracking this exact turn — an
   *  early-arrival chunk may have created the entry before SEND_START
   *  fired, and we must not wipe its accumulated text. A DIFFERENT
   *  messageId means a new turn superseded the old one, so we reset. */
  begin(conversationId: string, messageId: string): void {
    const prev = _turns[conversationId];
    if (prev && prev.messageId === messageId) return;
    put(conversationId, {
      conversationId,
      messageId,
      text: "",
      status: "streaming",
    });
  },

  /** Append a streamed chunk. Upserts, so a chunk that races ahead of
   *  `begin` still lands. Keys content on `messageId` so a stale chunk
   *  from a superseded turn starts a fresh (correct) entry rather than
   *  concatenating onto the wrong turn. */
  chunk(conversationId: string, messageId: string, chunk: string): void {
    const prev = _turns[conversationId];
    const base: LiveTurn =
      prev && prev.messageId === messageId
        ? prev
        : { conversationId, messageId, text: "", status: "streaming" };
    put(conversationId, {
      ...base,
      text: base.text + chunk,
      status: "streaming",
    });
  },

  /** Mark a turn complete. Prefers the accumulated text (what the user
   *  actually saw stream); falls back to `fullText` when we never
   *  observed the chunks (a re-attach that missed them still finalizes
   *  correctly). */
  complete(
    conversationId: string,
    messageId: string,
    fullText: string,
    metadata?: Record<string, unknown>,
  ): void {
    const prev = _turns[conversationId];
    const text =
      prev && prev.messageId === messageId && prev.text.length > 0
        ? prev.text
        : fullText;
    put(conversationId, {
      conversationId,
      messageId,
      text,
      status: "done",
      metadata,
    });
  },

  /** Mark a turn errored (e.g. a mesh peer died mid-stream). Keeps
   *  whatever partial text streamed so the re-attach can show it above
   *  the error tail. */
  error(conversationId: string, messageId: string, message: string): void {
    const prev = _turns[conversationId];
    const text = prev && prev.messageId === messageId ? prev.text : "";
    put(conversationId, {
      conversationId,
      messageId,
      text,
      status: "error",
      error: message,
    });
  },

  /** The tracked turn for a conversation, if any. */
  get(conversationId: string | null | undefined): LiveTurn | undefined {
    if (!conversationId) return undefined;
    return _turns[conversationId];
  },

  /** Drop a conversation's entry. Idempotent. Callers use this once a
   *  terminal turn has been fully absorbed into the machine + store and
   *  no longer needs re-attaching. */
  clear(conversationId: string): void {
    if (!(conversationId in _turns)) return;
    const next = { ..._turns };
    delete next[conversationId];
    _turns = next;
  },

  /** Test-only: wipe all tracked turns. */
  reset(): void {
    _turns = {};
  },
};
