// chatMachine — owns the message list and the streaming lifecycle for
// ChatView. Replaces the ad-hoc `$state` + Tauri-listener choreography
// that spawned the provenance bug.
//
// Two parallel regions:
//
//   turn:
//     idle ──(SEND_INITIATED)──▶ preparing ──(SEND_START)──▶ streaming
//                │                    │                          │
//                │                    └──(SEND_FAILED)──▶ idle   │
//                │                    │                          │
//                │                    └──(MESSAGE_ERROR)─▶ idle  │
//                │                                               │
//                │              ┌──(MESSAGE_COMPLETE)──▶ idle ◀──┘
//                │              └──(MESSAGE_ERROR)──▶ idle (error tail
//                │                                          appended)
//                │
//                └──(ASSISTANT_MESSAGE_RECEIVED)──▶ idle (non-streaming
//                                                  paths: askDocument,
//                                                  searchWeb)
//
//     The `preparing` substate exists so the user message + a loading
//     indicator can render the moment the user clicks Send, not after
//     `create_conversation` + `send_message_stream` have round-tripped.
//     On a cold daemon those awaits can take seconds, and without a
//     pre-stream visible state the chat looks frozen until the first
//     chunk lands.
//
//     All three streaming-flavoured substates handle MESSAGE_REFINED,
//     which rewrites a previously-completed assistant bubble in place
//     (post-stream epistemic-humility refinement).
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
  SearchAugmentation,
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
  /** Bind a freshly-created conversation id to the current turn
   *  WITHOUT wiping the message list. Used by `ensureConversation`
   *  when the user has already optimistically pushed their message
   *  (via SEND_INITIATED) and we just need to associate it with the
   *  new conversation row. HYDRATE would clobber the in-flight user
   *  bubble; this event preserves it. */
  | { type: "CONVERSATION_BOUND"; conversationId: string }
  /** Explicit reset — clears messages and any in-flight state.
   *  Used when the user navigates away from all conversations. */
  | { type: "RESET" }

  // ─── User-initiated turn ────────────────────────────────────
  /** Fired the moment the user clicks Send — BEFORE awaiting
   *  `create_conversation` / `send_message_stream`. Appends the user's
   *  message and moves into `preparing` so the surface can render the
   *  bubble + a loading indicator while the bridge calls round-trip. */
  | { type: "SEND_INITIATED"; userMessage: MessageEntry }
  /** Fired after `send_message_stream` resolves with a real
   *  assistant message id. Appends the assistant placeholder and moves
   *  into `streaming`. The user's message bubble was already appended
   *  by `SEND_INITIATED`, so this event no longer carries it. */
  | {
      type: "SEND_START";
      assistantMessageId: string;
    }
  /** `sendMessageStream` (or `create_conversation`) threw before any
   *  stream began. Appends a stand-alone "Error: ..." assistant
   *  message and returns to `idle`. */
  | { type: "SEND_FAILED"; error: string }

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
  /** Antifragile routing — emitted by ChatView after a
   *  `REDIRECT_SUBMIT` resolves. Marks the in-flight bubble as
   *  redirected-away and installs a new placeholder for the
   *  replacement stream. */
  | { type: "REDIRECT_STARTED"; newAssistantMessageId: string }

  // ─── Non-streaming assistant responses ──────────────────────
  /** `askDocument` / `searchWeb` return fully-formed assistant
   *  messages rather than streaming. Same event shape regardless. */
  | { type: "ASSISTANT_MESSAGE_RECEIVED"; message: MessageEntry }

  // ─── Information request (parallel region) ──────────────────
  | { type: "INFO_REQUEST_ARRIVED"; payload: InformationRequestPayload }
  /** User submitted content or skipped. Either way the card goes
   *  away; the submission itself is a Tauri command run by the
   *  component, not the machine. */
  | { type: "CLEAR_INFO" }
  /** Fired the moment the user submits a paste or kicks off a
   *  search from the InformationRequestCard. The bubble identified
   *  by `messageId` gets a `refining: true` flag so AssistantMessage
   *  can render a "Refining…" overlay until the corresponding
   *  MESSAGE_REFINED arrives. `messageId` is the most-recent
   *  assistant message at the moment the card was dismissed —
   *  resolved by ChatView from `messages[messages.length-1]`. */
  | { type: "MESSAGE_REFINING"; messageId: string }
  /** Fired after `submit_information_search` returns with the search
   *  metadata. Stashes the augmentation on the targeted message so
   *  the post-refine bubble can render the "Augmented via web
   *  search" footer with clickable source URLs. */
  | {
      type: "SEARCH_AUGMENTED";
      messageId: string;
      augmentation: SearchAugmentation;
    };

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
    // HYDRATE + RESET must also re-target the compound regions to
    // their idle substates — root-level actions only touch context,
    // so if we hydrate while `turn` is in `streaming` (e.g. the user
    // switches conversations mid-stream), every subsequent
    // conversation inherits the loading spinner. The bare actions
    // below remain as a safety net for any stray emit outside a
    // known substate; the per-substate overrides below handle the
    // common cases with an explicit `target`.
    HYDRATE: {
      actions: assign({
        conversationId: ({ event }) => event.conversationId,
        messages: ({ event }) => event.messages,
        streamingMessageId: () => null,
        pendingInfoRequest: () => null,
      }),
    },
    CONVERSATION_BOUND: {
      actions: assign({
        conversationId: ({ event }) => event.conversationId,
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
      // both conversation and message ids. Two guard clauses:
      //   1. Drop if conversation has moved on (racy switch).
      //   2. Drop if the message is STILL streaming. Refinement is a
      //      post-stream concept; if the backend ever fires it before
      //      MESSAGE_COMPLETE (chaos / protocol violation), accepting
      //      it here would clobber partial content and subsequent
      //      chunks would append to the refined text. Pinned by
      //      tests/e2e/specs/chat-chaos.spec.ts.
      guard: ({ context, event }) =>
        event.conversationId === context.conversationId &&
        event.messageId !== context.streamingMessageId,
      actions: assign(({ context, event }) => ({
        messages: updateMessageById(context.messages, event.messageId, (m) => {
          m.content = event.newContent;
          // Clear the refining-in-progress flag set by
          // MESSAGE_REFINING. The bubble's CSS transition fires off
          // the `refining` → falsy edge so the new content fades in
          // rather than slamming over the old one. `searchAugmentation`
          // (if any) was stashed by SEARCH_AUGMENTED earlier and is
          // intentionally NOT cleared here — the post-refine bubble
          // keeps the augmentation footer permanently.
          m.refining = false;
        }),
      })),
    },
    MESSAGE_REFINING: {
      // Set by ChatView the moment the user dismisses the
      // InformationRequestCard via paste-submit or search. Drives the
      // "Refining…" overlay so the user sees the in-place rewrite
      // coming. Tolerant of unknown messageId (no-op) so a stray
      // event from a stale card doesn't crash the machine.
      actions: assign(({ context, event }) => ({
        messages: updateMessageById(context.messages, event.messageId, (m) => {
          m.refining = true;
        }),
      })),
    },
    SEARCH_AUGMENTED: {
      // Stash search provenance on the targeted message so the
      // post-refine bubble can render the "Augmented via web search"
      // footer. Order vs MESSAGE_REFINED is intentionally undefined
      // — both transitions are idempotent under either order: this
      // event sets `searchAugmentation` without touching `content`
      // or `refining`, and MESSAGE_REFINED rewrites `content`
      // without touching `searchAugmentation`.
      actions: assign(({ context, event }) => ({
        messages: updateMessageById(context.messages, event.messageId, (m) => {
          m.searchAugmentation = event.augmentation;
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
            SEND_INITIATED: {
              target: "preparing",
              actions: assign(({ context, event }) => ({
                messages: produce(context.messages, (draft) => {
                  draft.push(event.userMessage);
                }),
              })),
            },
          },
        },
        preparing: {
          // Window between user-click and `send_message_stream`
          // resolving. The user bubble is already on screen; we're
          // waiting on the bridge to hand back an assistant message
          // id before installing the placeholder.
          on: {
            SEND_START: {
              target: "streaming",
              actions: assign(({ context, event }) => ({
                messages: produce(context.messages, (draft) => {
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
            SEND_FAILED: {
              // create_conversation / send_message_stream threw before
              // any stream began. Append a stand-alone error bubble
              // (the user message is already there) and bail to idle.
              target: "idle",
              actions: assign(({ context, event }) => ({
                messages: produce(context.messages, (draft) => {
                  draft.push({
                    id: crypto.randomUUID(),
                    role: "assistant",
                    content: `Error: ${event.error}`,
                    created_at: Math.floor(Date.now() / 1000),
                  });
                }),
              })),
            },
            MESSAGE_ERROR: {
              // Backend errored before SEND_START fired (rare but
              // possible — daemon crash mid-handshake). Same recovery
              // shape as SEND_FAILED.
              target: "idle",
              actions: assign(({ context, event }) => ({
                messages: produce(context.messages, (draft) => {
                  draft.push({
                    id: crypto.randomUUID(),
                    role: "assistant",
                    content: `Error: ${event.error}`,
                    created_at: Math.floor(Date.now() / 1000),
                  });
                }),
              })),
            },
            // Conversation switch / reset while in `preparing` re-targets
            // idle, mirroring the `streaming` substate. Without this the
            // loading indicator would leak across conversations.
            HYDRATE: {
              target: "idle",
              actions: assign({
                conversationId: ({ event }) => event.conversationId,
                messages: ({ event }) => event.messages,
                streamingMessageId: () => null,
                pendingInfoRequest: () => null,
              }),
            },
            RESET: {
              target: "idle",
              actions: assign({
                conversationId: () => null,
                messages: () => [],
                streamingMessageId: () => null,
                pendingInfoRequest: () => null,
              }),
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
            REDIRECT_STARTED: {
              // Stay in `streaming` — the new stream continues the
              // turn; we just pivot to a different assistant bubble.
              actions: assign(({ context, event }) => {
                const oldId = context.streamingMessageId;
                const nextMessages = oldId
                  ? updateMessageById(context.messages, oldId, (m) => {
                      // Tag the cancelled bubble so the renderer can
                      // de-emphasise it. Preserve any metadata the
                      // server already wrote.
                      m.metadata = {
                        ...(m.metadata ?? {}),
                        redirected_away: true,
                      };
                    })
                  : context.messages;
                // Push the replacement placeholder immediately so
                // the first MESSAGE_CHUNK with `newAssistantMessageId`
                // passes the guard.
                const withPlaceholder = produce(nextMessages, (draft) => {
                  draft.push({
                    id: event.newAssistantMessageId,
                    role: "assistant",
                    content: "",
                    created_at: Math.floor(Date.now() / 1000),
                  });
                });
                return {
                  messages: withPlaceholder,
                  streamingMessageId: event.newAssistantMessageId,
                };
              }),
            },
            // PR6 — switching conversations or resetting app state
            // while a stream is mid-flight must re-target `idle`.
            // The root-level HYDRATE/RESET handlers only touch
            // context; a compound-state transition needs to live in
            // the child. Without this, the old conversation's
            // spinner leaks into every subsequent conversation and
            // the user has no way to clear it short of a restart.
            HYDRATE: {
              target: "idle",
              actions: assign({
                conversationId: ({ event }) => event.conversationId,
                messages: ({ event }) => event.messages,
                streamingMessageId: () => null,
                pendingInfoRequest: () => null,
              }),
            },
            RESET: {
              target: "idle",
              actions: assign({
                conversationId: () => null,
                messages: () => [],
                streamingMessageId: () => null,
                pendingInfoRequest: () => null,
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
            // PR6 — same cross-conversation leak guard as `turn`.
            HYDRATE: {
              target: "idle",
              actions: assign({
                conversationId: ({ event }) => event.conversationId,
                messages: ({ event }) => event.messages,
                streamingMessageId: () => null,
                pendingInfoRequest: () => null,
              }),
            },
            RESET: {
              target: "idle",
              actions: assign({
                conversationId: () => null,
                messages: () => [],
                streamingMessageId: () => null,
                pendingInfoRequest: () => null,
              }),
            },
          },
        },
      },
    },
  },
});
