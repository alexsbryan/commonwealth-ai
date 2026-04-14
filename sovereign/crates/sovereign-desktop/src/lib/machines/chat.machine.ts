// chatMachine — owns the message list and the streaming lifecycle for
// ChatView. Replaces the ad-hoc `$state` + Tauri-listener choreography
// that spawned the provenance bug.
//
// Two parallel regions:
//
//   turn:
//     idle ──(SEND_START)──▶ streaming ──(MESSAGE_COMPLETE)──▶ idle
//                │                  │
//                │                  └──(MESSAGE_ERROR)──▶ idle (error tail
//                │                                         appended to bubble)
//                │
//                └──(ASSISTANT_MESSAGE_RECEIVED)──▶ idle (non-streaming
//                                                  paths: askDocument,
//                                                  searchWeb)
//
//     Both states also handle MESSAGE_REFINED, which rewrites a
//     previously-completed assistant bubble in place (post-stream
//     epistemic-humility refinement).
//
//   infoRequest:
//     idle ──(INFO_REQUEST_ARRIVED)──▶ pending ──(CLEAR_INFO)──▶ idle
//
// The component wrapper attaches Tauri `listen()` callbacks and
// forwards each event via `send()`. Non-streaming helpers
// (`askDocument`, `searchWeb`) likewise forward their results via
// ASSISTANT_MESSAGE_RECEIVED, so every write to the message list
// goes through the machine. Producing all context updates via
// `immer.produce()` guarantees a new top-level `messages` reference
// per write, eliminating the shallow-mutation bug class for good.
import { assign, setup } from "xstate";
import { produce } from "immer";
import type {
  MessageEntry,
  InformationRequestPayload,
} from "../types";

type Metadata = Record<string, unknown>;

export interface ChatContext {
  /** Conversation this machine is currently showing. Null before load. */
  conversationId: string | null;
  /** Ordered list of messages in the active conversation. */
  messages: MessageEntry[];
  /** `id` of the assistant message that's currently being streamed
   *  into, or null if no stream is in flight. */
  streamingMessageId: string | null;
  /** Pending epistemic-humility information request, if any. */
  pendingInfoRequest: InformationRequestPayload | null;
}

export type ChatEvent =
  // ─── Conversation lifecycle ─────────────────────────────────
  /** Called when switching conversations (or opening a new one). */
  | { type: "HYDRATE"; conversationId: string; messages: MessageEntry[] }
  /** Explicit reset — clears messages and any in-flight state.
   *  Used when the user navigates away from all conversations. */
  | { type: "RESET" }

  // ─── User-initiated turn ────────────────────────────────────
  /** Optimistically append the user's message and the assistant
   *  placeholder before `sendMessageStream` resolves. Moves the
   *  `turn` region into `streaming`. */
  | {
      type: "SEND_START";
      userMessage: MessageEntry;
      assistantMessageId: string;
    }
  /** `sendMessageStream` threw. Tags the placeholder (if any) with
   *  an error message and returns to `idle`. */
  | { type: "SEND_FAILED"; assistantMessageId: string; error: string }

  // ─── Streaming Tauri events ─────────────────────────────────
  | { type: "MESSAGE_CHUNK"; messageId: string; text: string }
  | {
      type: "MESSAGE_COMPLETE";
      messageId: string;
      fullText: string;
      pendingText: string;
      metadata?: Metadata;
    }
  | { type: "MESSAGE_ERROR"; error: string }
  | {
      type: "MESSAGE_REFINED";
      conversationId: string;
      messageId: string;
      newContent: string;
    }

  // ─── Non-streaming assistant responses ──────────────────────
  /** `askDocument` / `searchWeb` return fully-formed assistant
   *  messages rather than streaming. Same event shape regardless. */
  | { type: "ASSISTANT_MESSAGE_RECEIVED"; message: MessageEntry }

  // ─── Information request (parallel region) ──────────────────
  | { type: "INFO_REQUEST_ARRIVED"; payload: InformationRequestPayload }
  /** User submitted content or skipped. Either way the card goes
   *  away; the submission itself is a Tauri command run by the
   *  component, not the machine. */
  | { type: "CLEAR_INFO" };

// Helper: structurally-shared update of a single message by id. Used
// by every transition that rewrites assistant content. Returns the new
// messages array unchanged if the id is absent.
function updateMessageById(
  messages: MessageEntry[],
  id: string,
  updater: (draft: MessageEntry) => void,
): MessageEntry[] {
  const idx = messages.findIndex((m) => m.id === id);
  if (idx === -1) return messages;
  return produce(messages, (draft) => {
    updater(draft[idx]);
  });
}

export const chatMachine = setup({
  types: {
    context: {} as ChatContext,
    events: {} as ChatEvent,
  },
}).createMachine({
  id: "chat",
  type: "parallel",
  context: {
    conversationId: null,
    messages: [],
    streamingMessageId: null,
    pendingInfoRequest: null,
  },
  // Global event handlers (applicable in every state of every region).
  // Kept at the root so HYDRATE / RESET / MESSAGE_REFINED /
  // ASSISTANT_MESSAGE_RECEIVED work regardless of which substate
  // either region is in. Without this, we'd have to duplicate the
  // handler in `idle` and `streaming`.
  on: {
    HYDRATE: {
      actions: assign({
        conversationId: ({ event }) => event.conversationId,
        messages: ({ event }) => event.messages,
        streamingMessageId: () => null,
        pendingInfoRequest: () => null,
      }),
    },
    RESET: {
      actions: assign({
        conversationId: () => null,
        messages: () => [],
        streamingMessageId: () => null,
        pendingInfoRequest: () => null,
      }),
    },
    MESSAGE_REFINED: {
      // Post-stream epistemic-humility refinement — see
      // `runtime.rs::run_post_stream_refinement`. The payload carries
      // both conversation and message ids; ignore it if we've since
      // navigated away (rare but possible under racy conversation
      // switches).
      guard: ({ context, event }) =>
        event.conversationId === context.conversationId,
      actions: assign(({ context, event }) => ({
        messages: updateMessageById(context.messages, event.messageId, (m) => {
          m.content = event.newContent;
        }),
      })),
    },
    ASSISTANT_MESSAGE_RECEIVED: {
      // Non-streaming responses (document ask, web search). Just
      // append. The component has already awaited the API call.
      actions: assign(({ context, event }) => ({
        messages: produce(context.messages, (draft) => {
          draft.push(event.message);
        }),
      })),
    },
  },
  states: {
    turn: {
      initial: "idle",
      states: {
        idle: {
          on: {
            SEND_START: {
              target: "streaming",
              actions: assign(({ context, event }) => ({
                messages: produce(context.messages, (draft) => {
                  draft.push(event.userMessage);
                  // Placeholder assistant bubble — chunks will stream
                  // into its `content`. Same id the backend uses so
                  // MESSAGE_CHUNK / MESSAGE_COMPLETE can find it.
                  draft.push({
                    id: event.assistantMessageId,
                    role: "assistant",
                    content: "",
                    created_at: Math.floor(Date.now() / 1000),
                  });
                }),
                streamingMessageId: event.assistantMessageId,
              })),
            },
          },
        },
        streaming: {
          on: {
            MESSAGE_CHUNK: {
              guard: ({ context, event }) =>
                event.messageId === context.streamingMessageId,
              actions: assign(({ context, event }) => ({
                messages: updateMessageById(
                  context.messages,
                  event.messageId,
                  (m) => {
                    m.content += event.text;
                  },
                ),
              })),
            },
            MESSAGE_COMPLETE: {
              guard: ({ context, event }) =>
                event.messageId === context.streamingMessageId,
              target: "idle",
              actions: assign(({ context, event }) => ({
                messages: updateMessageById(
                  context.messages,
                  event.messageId,
                  (m) => {
                    // Append the word-buffer's residue first, then fall
                    // back to `fullText` for the non-streaming case
                    // where the placeholder was never populated.
                    if (event.pendingText) m.content += event.pendingText;
                    if (m.content.length === 0) m.content = event.fullText;
                    if (event.metadata) m.metadata = event.metadata;
                  },
                ),
                streamingMessageId: null,
              })),
            },
            MESSAGE_ERROR: {
              target: "idle",
              actions: assign(({ context, event }) => {
                const id = context.streamingMessageId;
                if (!id) return {};
                return {
                  messages: updateMessageById(context.messages, id, (m) => {
                    m.content = `${m.content}\n\nError: ${event.error}`;
                  }),
                  streamingMessageId: null,
                };
              }),
            },
            SEND_FAILED: {
              target: "idle",
              actions: assign({
                streamingMessageId: () => null,
              }),
            },
          },
        },
      },
    },
    infoRequest: {
      initial: "idle",
      states: {
        idle: {
          on: {
            INFO_REQUEST_ARRIVED: {
              target: "pending",
              actions: assign({
                pendingInfoRequest: ({ event }) => event.payload,
              }),
            },
          },
        },
        pending: {
          on: {
            CLEAR_INFO: {
              target: "idle",
              actions: assign({
                pendingInfoRequest: () => null,
              }),
            },
            // If a second info-request arrives while one is already
            // pending, overwrite — last write wins. In practice this
            // shouldn't happen (the backend blocks until the first
            // resolves), but the guardrail is cheap.
            INFO_REQUEST_ARRIVED: {
              actions: assign({
                pendingInfoRequest: ({ event }) => event.payload,
              }),
            },
          },
        },
      },
    },
  },
});
