<script lang="ts">
  // Slim chat surface for the recipe-author workspace. Reuses the
  // daemon's `send_message_stream` + `message-chunk` /
  // `message-complete` events but skips the heavy ChatView shell
  // (corpus banner, atlas chips, insights, reading surface, etc.).
  //
  // v1 simplifications:
  // - One fresh conversation per project-select. Restarting the
  //   workspace gives you a new conversation; conversation history
  //   per project arrives in M3.
  // - No optimistic streaming buffer / FSM. The transcript is a
  //   plain array; chunks append to the in-flight assistant message.
  // - No reading-surface / insights handoff.

  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { createConversation, sendMessageStream } from "../../api";

  let { featureId, projectTitle }: { featureId: string; projectTitle: string } =
    $props();

  type Message = {
    id: string;
    role: "user" | "assistant";
    content: string;
    streaming: boolean;
    error?: string;
  };

  let conversationId: string | null = $state(null);
  let messages: Message[] = $state([]);
  let composerValue = $state("");
  let sending = $state(false);
  let transcriptRef: HTMLDivElement | null = $state(null);

  let unlistenChunk: UnlistenFn | null = null;
  let unlistenComplete: UnlistenFn | null = null;
  let unlistenError: UnlistenFn | null = null;

  // When the project changes, reset and provision a new conversation.
  // This is the v1 contract — one fresh conversation per session per
  // project. The runtime tags the conversation with `recipe-author`
  // on the first message because the workspace has activated that
  // skill (see store.activate()).
  $effect(() => {
    void resetForProject(featureId);
  });

  async function resetForProject(_id: string): Promise<void> {
    messages = [];
    composerValue = "";
    sending = false;
    try {
      const resp = await createConversation();
      conversationId = resp.id;
    } catch (e) {
      console.warn("recipe-author chat: createConversation failed:", e);
    }
  }

  function appendMessage(m: Message): void {
    messages = [...messages, m];
    scrollToBottom();
  }

  function patchMessage(id: string, patch: Partial<Message>): void {
    messages = messages.map((m) => (m.id === id ? { ...m, ...patch } : m));
    scrollToBottom();
  }

  function scrollToBottom(): void {
    queueMicrotask(() => {
      if (transcriptRef) {
        transcriptRef.scrollTop = transcriptRef.scrollHeight;
      }
    });
  }

  onMount(async () => {
    unlistenChunk = await listen<{
      conversation_id: string;
      message_id: string;
      chunk: string;
    }>("message-chunk", (event) => {
      const p = event.payload;
      if (p.conversation_id !== conversationId) return;
      messages = messages.map((m) => {
        if (m.id !== p.message_id) return m;
        return { ...m, content: m.content + p.chunk };
      });
      scrollToBottom();
    });
    unlistenComplete = await listen<{
      conversation_id: string;
      message_id: string;
      full_text: string;
    }>("message-complete", (event) => {
      const p = event.payload;
      if (p.conversation_id !== conversationId) return;
      patchMessage(p.message_id, {
        content: p.full_text,
        streaming: false,
      });
      sending = false;
    });
    unlistenError = await listen<{ message: string }>(
      "message-error",
      (event) => {
        // Mark the in-flight assistant message (if any) as errored.
        const inflight = messages.find((m) => m.streaming);
        if (inflight) {
          patchMessage(inflight.id, {
            streaming: false,
            error: event.payload.message,
          });
        }
        sending = false;
      },
    );
  });

  onDestroy(() => {
    unlistenChunk?.();
    unlistenComplete?.();
    unlistenError?.();
  });

  async function send(e?: Event): Promise<void> {
    e?.preventDefault();
    const text = composerValue.trim();
    if (!text || sending) return;
    if (!conversationId) {
      const resp = await createConversation();
      conversationId = resp.id;
    }
    const userId = `local-user-${Date.now()}`;
    appendMessage({
      id: userId,
      role: "user",
      content: text,
      streaming: false,
    });
    composerValue = "";
    sending = true;

    try {
      const resp = await sendMessageStream(text, conversationId);
      appendMessage({
        id: resp.message_id,
        role: "assistant",
        content: "",
        streaming: true,
      });
    } catch (e) {
      appendMessage({
        id: `err-${Date.now()}`,
        role: "assistant",
        content: "",
        streaming: false,
        error: String(e),
      });
      sending = false;
    }
  }

  function handleKeydown(e: KeyboardEvent): void {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void send();
    }
  }
</script>

<section class="chat" data-testid="recipe-author-chat">
  <header class="chat-head">
    <span class="project">{projectTitle}</span>
    <span class="conv" title={conversationId ?? ""}>
      {#if conversationId}conversation #{conversationId.slice(0, 8)}{/if}
    </span>
  </header>

  <div class="transcript" bind:this={transcriptRef}>
    {#if messages.length === 0}
      <p class="placeholder">
        Describe the corpus you want to build. The agent will probe
        URLs, draft TOML, and surface decisions on the right.
      </p>
    {:else}
      {#each messages as m (m.id)}
        <div class="msg" class:user={m.role === "user"} class:assistant={m.role === "assistant"}>
          <div class="role">{m.role}</div>
          <div class="content">
            {m.content}
            {#if m.streaming}<span class="cursor">▋</span>{/if}
            {#if m.error}<span class="msg-error">⚠ {m.error}</span>{/if}
          </div>
        </div>
      {/each}
    {/if}
  </div>

  <form class="composer" onsubmit={send}>
    <textarea
      bind:value={composerValue}
      onkeydown={handleKeydown}
      placeholder="Talk to the recipe agent. Press Enter to send."
      rows="3"
      disabled={sending}
      data-testid="recipe-author-composer"
    ></textarea>
    <button
      type="submit"
      class="send"
      disabled={sending || !composerValue.trim()}
      data-testid="recipe-author-send"
    >
      {sending ? "…" : "Send"}
    </button>
  </form>
</section>

<style>
  .chat {
    display: flex;
    flex-direction: column;
    flex: 1 1 auto;
    min-height: 0;
  }
  .chat-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    padding: 0.5rem 1rem;
    border-bottom: 1px solid var(--border, #2a2c33);
    font-size: 0.85rem;
  }
  .project {
    font-weight: 600;
  }
  .conv {
    color: var(--muted, #8a8c93);
    font-size: 0.75rem;
    font-family: ui-monospace, monospace;
  }
  .transcript {
    flex: 1 1 auto;
    overflow-y: auto;
    padding: 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.8rem;
  }
  .placeholder {
    color: var(--muted, #8a8c93);
    font-size: 0.9rem;
    font-style: italic;
    text-align: center;
    margin: auto;
  }
  .msg {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    max-width: 85%;
  }
  .msg.user {
    align-self: flex-end;
    align-items: flex-end;
  }
  .msg.assistant {
    align-self: flex-start;
  }
  .role {
    font-size: 0.7rem;
    color: var(--muted, #8a8c93);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .content {
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid var(--border, #2a2c33);
    border-radius: 6px;
    padding: 0.5rem 0.7rem;
    font-size: 0.9rem;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .msg.user .content {
    background: rgba(120, 200, 240, 0.1);
    border-color: rgba(120, 200, 240, 0.25);
  }
  .cursor {
    opacity: 0.6;
    margin-left: 2px;
    animation: blink 1s steps(2) infinite;
  }
  @keyframes blink {
    50% {
      opacity: 0.1;
    }
  }
  .msg-error {
    display: block;
    margin-top: 0.4rem;
    color: #f4b3b3;
    font-size: 0.8rem;
  }
  .composer {
    display: flex;
    gap: 0.5rem;
    padding: 0.6rem 1rem;
    border-top: 1px solid var(--border, #2a2c33);
    background: rgba(255, 255, 255, 0.015);
  }
  textarea {
    flex: 1 1 auto;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid var(--border, #2a2c33);
    color: inherit;
    padding: 0.5rem 0.6rem;
    border-radius: 4px;
    resize: vertical;
    font: inherit;
    font-size: 0.9rem;
  }
  .send {
    background: rgba(120, 200, 240, 0.18);
    border: 1px solid rgba(120, 200, 240, 0.4);
    color: inherit;
    padding: 0 1rem;
    border-radius: 4px;
    cursor: pointer;
    font-size: 0.9rem;
  }
  .send:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
</style>
