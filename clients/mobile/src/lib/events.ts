// SPDX-License-Identifier: AGPL-3.0-or-later
// Wires the Rust core's Tauri events into the shared chat FSM. These
// are the SAME event names the desktop backend emits, so the shared
// chat.machine consumes mobile streams unchanged.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ChatEvent } from "./machines/chat.machine";

// The shared chat FSM's send only accepts its own event union, so typing
// this as the looser `Record<string, unknown>` made the machine's `send`
// un-assignable here (function-param contravariance). Mirror the machine's
// event type — this also type-checks the events we dispatch below.
type Send = (event: ChatEvent) => void;

interface ChunkPayload {
  conversation_id: string;
  message_id: string;
  chunk: string;
}
interface CompletePayload {
  conversation_id: string;
  message_id: string;
  full_text: string;
  metadata?: Record<string, unknown>;
}
interface ErrorPayload {
  message: string;
  retry_after_secs?: number | null;
}
interface ConnectivityPayload {
  host_connection_id: string;
  state: string;
  retry_after_secs?: number | null;
}

interface StartPayload {
  conversation_id: string;
  message_id: string;
}

/** Attach the streaming listeners; returns an unlisten fn. */
export async function attachStreamListeners(send: Send): Promise<UnlistenFn> {
  const offs: UnlistenFn[] = [];
  // First token → the host has assigned the assistant message id. Create
  // the streaming placeholder (SEND_START) before chunks arrive; the
  // FSM's MESSAGE_CHUNK guard requires `streamingMessageId` to be set.
  offs.push(
    await listen<StartPayload>("message-start", (e) =>
      send({ type: "SEND_START", assistantMessageId: e.payload.message_id }),
    ),
  );
  offs.push(
    await listen<ChunkPayload>("message-chunk", (e) =>
      send({ type: "MESSAGE_CHUNK", messageId: e.payload.message_id, text: e.payload.chunk }),
    ),
  );
  offs.push(
    await listen<CompletePayload>("message-complete", (e) =>
      send({
        type: "MESSAGE_COMPLETE",
        messageId: e.payload.message_id,
        fullText: e.payload.full_text,
        pendingText: "",
        metadata: e.payload.metadata,
      }),
    ),
  );
  offs.push(
    await listen<ErrorPayload>("message-error", (e) =>
      send({ type: "MESSAGE_ERROR", error: e.payload.message }),
    ),
  );
  return () => offs.forEach((off) => off());
}

/** One glassbox progress signal for the in-flight turn, forwarded from
 *  the host's runtime narration channel (`message-narration`). */
export interface NarrationEntry {
  conversation_id: string;
  message_id: string;
  /** `NarrationPhase`: a snake_case string (`"retrieval_start"`) or a
   *  single-key object (`{ retrieval_complete: { chunks_in, corpora } }`). */
  phase: string | Record<string, unknown>;
  text: string;
  elapsed_ms: number;
}

/** Attach the live-narration listener; returns an unlisten fn. Kept
 *  separate from the chat FSM (mirrors desktop's routingStore): narration
 *  is transient turn-progress, not message state. */
export async function attachNarrationListener(
  cb: (entry: NarrationEntry) => void,
): Promise<UnlistenFn> {
  return listen<NarrationEntry>("message-narration", (e) => cb(e.payload));
}

/** Subscribe to connectivity transitions (off-tailnet / host-down /
 *  host-busy / reachable). */
export async function attachConnectivityListener(
  cb: (state: string, retryAfterSecs?: number | null) => void,
): Promise<UnlistenFn> {
  return listen<ConnectivityPayload>("connectivity-changed", (e) =>
    cb(e.payload.state, e.payload.retry_after_secs),
  );
}
