<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import {
    sendMessageStream,
    searchWeb,
    getConversation,
    createConversation,
    listCorpora,
    ingestDocument,
  } from "../api";
  import type { IngestDocumentResult } from "../api";
  import type {
    MessageEntry,
    TaskStep,
    ApprovalRequestPayload,
    UserInputRequestPayload,
    CorpusEntry,
    MessageChunkPayload,
    MessageCompletePayload,
    ErrorPayload,
  } from "../types";
  import { WordBufferedStream } from "../stream-buffer";
  import { insightStore } from "../stores/insights.svelte";
  import MessageBubble from "./MessageBubble.svelte";
  import TaskProgress from "./TaskProgress.svelte";
  import ApprovalCard from "./ApprovalCard.svelte";
  import CorpusProgressBanner from "./CorpusProgressBanner.svelte";
  import AttachmentBanner from "./AttachmentBanner.svelte";

  interface Props {
    conversationId: string | null;
    taskSteps: TaskStep[];
    pendingApproval: ApprovalRequestPayload | null;
    pendingInput: UserInputRequestPayload | null;
    onClearTask: () => void;
    onApprovalHandled: () => void;
    onInputHandled: () => void;
    onOpenSettings?: () => void;
    onToggleInsights?: () => void;
  }

  let {
    conversationId,
    taskSteps,
    pendingApproval,
    pendingInput,
    onClearTask,
    onApprovalHandled,
    onInputHandled,
    onOpenSettings,
    onToggleInsights,
  }: Props = $props();

  let messages: MessageEntry[] = $state([]);
  let inputText = $state("");
  let isLoading = $state(false);
  let messagesContainer: HTMLDivElement;

  // Document attachment state.
  let attachment = $state<{
    source: string;
    filePath: string;
    chunksCreated: number;
  } | null>(null);
  let isIngesting = $state(false);
  let activeConversationId: string | null = $state(null);
  let streamingMessageId: string | null = $state(null);
  let wordBuffer = new WordBufferedStream();
  let unlistenChunk: UnlistenFn | null = null;
  let unlistenComplete: UnlistenFn | null = null;
  let unlistenError: UnlistenFn | null = null;

  $effect(() => {
    if (conversationId !== activeConversationId) {
      activeConversationId = conversationId;
      loadConversation();
    }
  });

  onMount(async () => {
    unlistenChunk = await listen<MessageChunkPayload>(
      "message-chunk",
      (event) => {
        const p = event.payload;
        if (p.message_id !== streamingMessageId) return;
        const flushed = wordBuffer.push(p.chunk);
        if (flushed !== null) {
          const idx = messages.findIndex((m) => m.id === p.message_id);
          if (idx !== -1) {
            messages[idx].content += flushed;
          }
          scrollToBottom();
        }
      },
    );

    unlistenComplete = await listen<MessageCompletePayload>(
      "message-complete",
      (event) => {
        const p = event.payload;
        if (p.message_id !== streamingMessageId) return;
        const idx = messages.findIndex((m) => m.id === p.message_id);
        if (idx !== -1) {
          // Flush remaining buffered text.
          const remaining = wordBuffer.flush();
          if (remaining) {
            messages[idx].content += remaining;
          }
          // For non-streaming fallback, the placeholder may be empty.
          if (messages[idx].content.length === 0) {
            messages[idx].content = p.full_text;
          }
          // Apply metadata (provenance, retrieved_chunks) — now delivered
          // with the message-complete event after the backend saves the message.
          if (p.metadata) {
            messages[idx].metadata = p.metadata;
          }
        }
        streamingMessageId = null;
        isLoading = false;
        scrollToBottom();
      },
    );

    unlistenError = await listen<ErrorPayload>("message-error", (event) => {
      if (!streamingMessageId) return;
      const idx = messages.findIndex((m) => m.id === streamingMessageId);
      if (idx !== -1) {
        messages[idx].content =
          messages[idx].content + `\n\nError: ${event.payload.message}`;
      }
      streamingMessageId = null;
      isLoading = false;
    });
  });

  onDestroy(() => {
    unlistenChunk?.();
    unlistenComplete?.();
    unlistenError?.();
  });

  async function loadConversation() {
    messages = [];
    onClearTask();
    if (!activeConversationId) return;

    try {
      const detail = await getConversation(activeConversationId);
      messages = detail.messages;
      scrollToBottom();
    } catch {
      // New conversation — no history yet.
    }
  }

  async function handleAttach() {
    const selected = await open({
      multiple: false,
      filters: [
        {
          name: "Documents",
          extensions: ["txt", "md", "pdf"],
        },
      ],
    });
    if (!selected) return;

    const filePath = typeof selected === "string" ? selected : selected;
    isIngesting = true;

    try {
      const result = await ingestDocument(filePath);
      attachment = {
        source: result.source,
        filePath,
        chunksCreated: result.chunks_created,
      };
    } catch (e) {
      console.error("Failed to ingest document:", e);
    } finally {
      isIngesting = false;
    }
  }

  async function handleSend() {
    let text = inputText.trim();
    if (!text || isLoading) return;

    // If a document is attached, prepend context so the planner/tools know.
    if (attachment) {
      text = `[Document attached: ${attachment.source}]\n\n${text}`;
    }

    // Create a conversation if none selected.
    let convoId = activeConversationId;
    if (!convoId) {
      const created = await createConversation();
      convoId = created.id;
      activeConversationId = convoId;
    }

    // Add user message optimistically.
    const userMsg: MessageEntry = {
      id: crypto.randomUUID(),
      role: "user",
      content: text,
      created_at: Math.floor(Date.now() / 1000),
    };
    messages = [...messages, userMsg];
    inputText = "";
    attachment = null;
    isLoading = true;
    onClearTask();
    scrollToBottom();

    try {
      const started = await sendMessageStream(text, convoId);
      streamingMessageId = started.message_id;
      wordBuffer.reset();
      // Add empty placeholder; chunks will append to it.
      const placeholder: MessageEntry = {
        id: started.message_id,
        role: "assistant",
        content: "",
        created_at: Math.floor(Date.now() / 1000),
      };
      messages = [...messages, placeholder];
      scrollToBottom();
      // isLoading stays true until message-complete arrives.
    } catch (e) {
      const errorMsg: MessageEntry = {
        id: crypto.randomUUID(),
        role: "assistant",
        content: `Error: ${e}`,
        created_at: Math.floor(Date.now() / 1000),
      };
      messages = [...messages, errorMsg];
      isLoading = false;
      scrollToBottom();
    }
  }

  async function handleSearch() {
    const text = inputText.trim();
    if (!text || isLoading) return;

    let convoId = activeConversationId;
    if (!convoId) {
      const created = await createConversation();
      convoId = created.id;
      activeConversationId = convoId;
    }

    const userMsg: MessageEntry = {
      id: crypto.randomUUID(),
      role: "user",
      content: text,
      created_at: Math.floor(Date.now() / 1000),
    };
    messages = [...messages, userMsg];
    inputText = "";
    attachment = null;
    isLoading = true;
    onClearTask();
    scrollToBottom();

    try {
      const response = await searchWeb(text, convoId);
      const assistantMsg: MessageEntry = {
        id: response.message_id,
        role: "assistant",
        content: response.content,
        created_at: Math.floor(Date.now() / 1000),
      };
      messages = [...messages, assistantMsg];
    } catch (e) {
      const errorMsg: MessageEntry = {
        id: crypto.randomUUID(),
        role: "assistant",
        content: `Search error: ${e}`,
        created_at: Math.floor(Date.now() / 1000),
      };
      messages = [...messages, errorMsg];
    }

    isLoading = false;
    scrollToBottom();
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }

  function scrollToBottom() {
    requestAnimationFrame(() => {
      if (messagesContainer) {
        messagesContainer.scrollTop = messagesContainer.scrollHeight;
      }
    });
  }
</script>

<div class="chat-view">
  <CorpusProgressBanner {onOpenSettings} />
  <div class="messages" bind:this={messagesContainer}>
    {#if messages.length === 0 && !isLoading}
      <div class="empty-state">
        <div class="empty-glow"></div>
        <div class="empty-mark">◈</div>
        <h2>SOVEREIGN</h2>
        <p class="empty-sub">Your AI. Your data. Your mesh.</p>
        {#await listCorpora() then corpora}
          {#if corpora.filter((c: CorpusEntry) => c.status === "installed").length > 0}
            <div class="kb-tags">
              {#each corpora.filter((c: CorpusEntry) => c.status === "installed") as corpus}
                <span class="kb-tag">{corpus.name}</span>
              {/each}
            </div>
          {/if}
        {:catch}
          <!-- silently ignore if corpus listing fails -->
        {/await}
      </div>
    {:else}
      {#each messages as msg (msg.id)}
        <MessageBubble
          role={msg.role}
          content={msg.content}
          metadata={msg.metadata}
          messageId={msg.id}
          conversationId={activeConversationId ?? ""}
          isStreaming={msg.id === streamingMessageId}
        />
      {/each}

      <TaskProgress steps={taskSteps} />

      <ApprovalCard
        approval={pendingApproval}
        inputRequest={pendingInput}
        {onApprovalHandled}
        {onInputHandled}
      />

      {#if isLoading}
        <div class="typing-indicator" aria-label="Sovereign is responding">
          <span></span><span></span><span></span>
        </div>
      {/if}
    {/if}
  </div>

  <div class="input-area">
    {#if attachment}
      <AttachmentBanner
        filename={attachment.source}
        chunksCreated={attachment.chunksCreated}
        onremove={() => (attachment = null)}
      />
    {/if}
    <div class="input-row">
    <button
      class="attach-btn"
      onclick={handleAttach}
      disabled={isLoading || isIngesting}
      title={isIngesting ? "Ingesting document..." : "Attach a document"}
    >
      {#if isIngesting}
        <span class="attach-spinner"></span>
      {:else}
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
          <path d="M14 8.5l-5.6 5.6a3.5 3.5 0 01-5-5L9 3.5a2.5 2.5 0 013.5 3.5L7 12.5a1.5 1.5 0 01-2.1-2.1L10.3 5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
        </svg>
      {/if}
    </button>
    <textarea
      bind:value={inputText}
      placeholder={attachment ? `Ask about ${attachment.source}...` : "Type a message..."}
      onkeydown={handleKeydown}
      rows="1"
      disabled={isLoading}
    ></textarea>
    <button
      class="search-btn"
      onclick={handleSearch}
      disabled={isLoading || !inputText.trim()}
      title="Search the web"
    >
      <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
        <circle cx="7" cy="7" r="5.5" stroke="currentColor" stroke-width="1.5"/>
        <line x1="11" y1="11" x2="14.5" y2="14.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
      </svg>
    </button>
    {#if onToggleInsights}
      <button
        class="insights-toggle-btn"
        onclick={onToggleInsights}
        title="Toggle insights panel"
      >
        &#x25C8;
        {#if insightStore.count > 0}
          <span class="insights-badge">{insightStore.count}</span>
        {/if}
      </button>
    {/if}
    <button
      class="send-btn"
      onclick={handleSend}
      disabled={isLoading || !inputText.trim()}
    >
      Send
    </button>
    </div>
  </div>
</div>

<style>
  .chat-view {
    display: flex;
    flex-direction: column;
    height: 100%;
  }

  /* ── Messages ── */
  .messages {
    flex: 1;
    overflow-y: auto;
    padding: 24px 32px 16px;
    display: flex;
    flex-direction: column;
  }

  /* ── Empty state ── */
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    flex: 1;
    text-align: center;
    position: relative;
    gap: 0;
  }

  .empty-glow {
    position: absolute;
    width: 380px;
    height: 300px;
    border-radius: 50%;
    background: radial-gradient(
      ellipse at 50% 50%,
      rgba(155, 135, 196, 0.09) 0%,
      rgba(201, 168, 76,  0.04) 45%,
      transparent 70%
    );
    pointer-events: none;
  }

  .empty-mark {
    font-size: 2.8rem;
    color: var(--accent);
    line-height: 1;
    filter: drop-shadow(0 0 14px rgba(201, 168, 76, 0.45));
    margin-bottom: 16px;
    animation: empty-breathe 3.5s ease-in-out infinite;
    position: relative;
  }

  .empty-state h2 {
    font-size: 1.1rem;
    font-weight: 700;
    letter-spacing: 0.22em;
    color: var(--text-secondary);
    margin-bottom: 10px;
    position: relative;
  }

  .empty-sub {
    font-size: 0.8rem;
    color: var(--text-muted);
    letter-spacing: 0.05em;
    margin-bottom: 20px;
    position: relative;
  }

  .kb-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    justify-content: center;
    position: relative;
  }

  .kb-tag {
    font-size: 0.67rem;
    padding: 3px 10px;
    border: 1px solid var(--border-mid);
    border-radius: 100px;
    color: var(--text-muted);
    font-family: 'Syne Mono', monospace;
    letter-spacing: 0.04em;
    background: var(--bg-surface);
  }

  @keyframes empty-breathe {
    0%, 100% {
      filter: drop-shadow(0 0 10px rgba(201, 168, 76, 0.38));
    }
    50% {
      filter: drop-shadow(0 0 24px rgba(201, 168, 76, 0.65));
    }
  }

  /* ── Input area ── */
  .input-area {
    display: flex;
    flex-direction: column;
    padding: 12px 20px 16px;
    border-top: 1px solid var(--border-mid);
    background: var(--bg-secondary);
  }

  .input-row {
    display: flex;
    gap: 8px;
    align-items: flex-end;
  }

  .attach-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    background: transparent;
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    color: var(--text-muted);
    cursor: pointer;
    flex-shrink: 0;
    transition: border-color 0.15s, color 0.15s;
  }
  .attach-btn:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent);
  }
  .attach-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .attach-spinner {
    width: 12px;
    height: 12px;
    border: 2px solid var(--text-muted);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.6s linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  textarea {
    flex: 1;
    padding: 10px 14px;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg);
    resize: none;
    outline: none;
    min-height: 42px;
    max-height: 120px;
    line-height: 1.5;
    color: var(--text-primary);
    transition: border-color 0.2s, box-shadow 0.2s;
  }

  textarea::placeholder {
    color: var(--text-muted);
  }

  textarea:focus {
    border-color: color-mix(in srgb, var(--accent) 50%, transparent);
    box-shadow: 0 0 0 2px var(--accent-glow);
  }

  .search-btn {
    padding: 10px;
    background: var(--bg-surface);
    color: var(--text-muted);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    align-self: flex-end;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: all 0.2s;
  }

  .search-btn:hover:not(:disabled) {
    background: var(--sky-dim);
    border-color: var(--sky);
    color: var(--sky);
  }

  .search-btn:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }

  .insights-toggle-btn {
    padding: 10px;
    background: var(--bg-surface);
    color: var(--amber);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    align-self: flex-end;
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 14px;
    cursor: pointer;
    transition: all 0.2s;
    position: relative;
  }

  .insights-toggle-btn:hover {
    border-color: var(--amber);
    background: rgba(186, 117, 23, 0.06);
  }

  .insights-badge {
    font-size: 9px;
    font-family: var(--font-mono);
    background: var(--accent-glow);
    border: 0.5px solid color-mix(in srgb, var(--amber) 40%, transparent);
    border-radius: 999px;
    padding: 0 4px;
    color: var(--amber);
  }

  .send-btn {
    padding: 10px 20px;
    background: var(--accent);
    color: var(--bg-root);
    border-radius: var(--radius);
    font-weight: 700;
    font-size: 0.82rem;
    letter-spacing: 0.05em;
    align-self: flex-end;
    transition: background 0.2s, box-shadow 0.2s, transform 0.15s;
  }

  .send-btn:hover:not(:disabled) {
    background: var(--accent-light);
    box-shadow: 0 0 18px var(--accent-dim);
    transform: translateY(-1px);
  }

  .send-btn:active:not(:disabled) {
    transform: translateY(0);
  }

  .send-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  /* ── Typing indicator ── */
  .typing-indicator {
    display: flex;
    gap: 5px;
    padding: 4px 0 4px 16px;
    align-self: flex-start;
    border-left: 2px solid color-mix(in srgb, var(--lavender) 30%, transparent);
    margin-bottom: 12px;
  }

  .typing-indicator span {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--lavender);
    animation: typing-pulse 1.3s ease-in-out infinite;
  }

  .typing-indicator span:nth-child(2) {
    animation-delay: 0.2s;
  }

  .typing-indicator span:nth-child(3) {
    animation-delay: 0.4s;
  }

  @keyframes typing-pulse {
    0%, 80%, 100% {
      transform: scale(0.55);
      opacity: 0.35;
    }
    40% {
      transform: scale(1);
      opacity: 1;
    }
  }
</style>
