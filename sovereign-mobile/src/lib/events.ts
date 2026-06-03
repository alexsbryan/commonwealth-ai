// Wires the Rust core's Tauri events into the shared chat FSM. These
// are the SAME event names the desktop backend emits, so the shared
// chat.machine consumes mobile streams unchanged.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";

type Send = (event: Record<string, unknown>) => void;

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

/** Subscribe to connectivity transitions (off-tailnet / host-down /
 *  host-busy / reachable). */
export async function attachConnectivityListener(
  cb: (state: string, retryAfterSecs?: number | null) => void,
): Promise<UnlistenFn> {
  return listen<ConnectivityPayload>("connectivity-changed", (e) =>
    cb(e.payload.state, e.payload.retry_after_secs),
  );
}
