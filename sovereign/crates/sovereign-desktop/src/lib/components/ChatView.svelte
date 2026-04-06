<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import {
    sendMessageStream,
    searchWeb,
    getConversation,
    createConversation,
    listCorpora,
  } from "../api";
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
  import MessageBubble from "./MessageBubble.svelte";
  import TaskProgress from "./TaskProgress.svelte";
  import ApprovalCard from "./ApprovalCard.svelte";
  import CorpusProgressBanner from "./CorpusProgressBanner.svelte";

  interface Props {
    conversationId: string | null;
    taskSteps: TaskStep[];
    pendingApproval: ApprovalRequestPayload | null;
    pendingInput: UserInputRequestPayload | null;
    onClearTask: () => void;
    onApprovalHandled: () => void;
    onInputHandled: () => void;
    onOpenSettings?: () => void;
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
  }: Props = $props();

  let messages: MessageEntry[] = $state([]);
  let inputText = $state("");
  let isLoading = $state(false);
  let messagesContainer: HTMLDivElement;
  let activeConversationId: string | null = $state(null);
  let streamingMessageId: string | null = null;
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
        const idx = messages.findIndex((m) => m.id === p.message_id);
        if (idx === -1) return;
        messages[idx].content += p.chunk;
        scrollToBottom();
      },
    );

    unlistenComplete = await listen<MessageCompletePayload>(
      "message-complete",
      (event) => {
        const p = event.payload;
        if (p.message_id !== streamingMessageId) return;
        const idx = messages.findIndex((m) => m.id === p.message_id);
        if (idx !== -1) {
          // For non-streaming fallback, the placeholder may be empty.
          if (messages[idx].content.length === 0) {
            messages[idx].content = p.full_text;
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

  async function handleSend() {
    const text = inputText.trim();
    if (!text || isLoading) return;

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
    isLoading = true;
    onClearTask();
    scrollToBottom();

    try {
      const started = await sendMessageStream(text, convoId);
      streamingMessageId = started.message_id;
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
        <h2>Sovereign</h2>
        <p>Start a conversation. Everything runs on your machine.</p>
        {#await listCorpora() then corpora}
          {#if corpora.filter((c: CorpusEntry) => c.status === "installed").length > 0}
            <p class="kb-note">
              Knowledge bases:
              {corpora.filter((c: CorpusEntry) => c.status === "installed").map((c: CorpusEntry) => c.name).join(", ")}
            </p>
          {/if}
        {:catch}
          <!-- silently ignore if corpus listing fails -->
        {/await}
      </div>
    {:else}
      {#each messages as msg (msg.id)}
        <MessageBubble role={msg.role} content={msg.content} metadata={msg.metadata} />
      {/each}

      <TaskProgress steps={taskSteps} />

      <ApprovalCard
        approval={pendingApproval}
        inputRequest={pendingInput}
        {onApprovalHandled}
        {onInputHandled}
      />

      {#if isLoading}
        <div class="typing-indicator">
          <span></span><span></span><span></span>
        </div>
      {/if}
    {/if}
  </div>

  <div class="input-area">
    <textarea
      bind:value={inputText}
      placeholder="Type a message..."
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
    <button
      class="send-btn"
      onclick={handleSend}
      disabled={isLoading || !inputText.trim()}
    >
      Send
    </button>
  </div>
</div>

<style>
  .chat-view {
    display: flex;
    flex-direction: column;
    height: 100%;
  }

  .messages {
    flex: 1;
    overflow-y: auto;
    padding: 20px 24px;
    display: flex;
    flex-direction: column;
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    flex: 1;
    color: var(--text-muted);
    text-align: center;
  }

  .empty-state h2 {
    font-size: 1.8rem;
    font-weight: 300;
    margin-bottom: 0.5rem;
    color: var(--text-secondary);
  }

  .empty-state :global(.kb-note) {
    font-size: 0.8rem;
    color: var(--text-muted);
    margin-top: 0.5rem;
  }

  .input-area {
    display: flex;
    gap: 8px;
    padding: 12px 24px 16px;
    border-top: 1px solid var(--border);
    background: var(--bg-primary);
  }

  textarea {
    flex: 1;
    padding: 10px 14px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    resize: none;
    outline: none;
    min-height: 40px;
    max-height: 120px;
  }

  textarea:focus {
    border-color: var(--accent);
  }

  .search-btn {
    padding: 10px;
    background: var(--bg-surface);
    color: var(--text-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    transition:
      background 0.2s,
      color 0.2s,
      border-color 0.2s;
    align-self: flex-end;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .search-btn:hover:not(:disabled) {
    background: var(--accent);
    color: white;
    border-color: var(--accent);
  }

  .search-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .send-btn {
    padding: 10px 20px;
    background: var(--accent);
    color: white;
    border-radius: var(--radius);
    font-weight: 500;
    transition: background 0.2s;
    align-self: flex-end;
  }

  .send-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .send-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .typing-indicator {
    display: flex;
    gap: 4px;
    padding: 12px 16px;
    align-self: flex-start;
  }

  .typing-indicator span {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-muted);
    animation: bounce 1.4s ease-in-out infinite;
  }

  .typing-indicator span:nth-child(2) {
    animation-delay: 0.2s;
  }

  .typing-indicator span:nth-child(3) {
    animation-delay: 0.4s;
  }

  @keyframes bounce {
    0%,
    80%,
    100% {
      transform: scale(0.6);
      opacity: 0.4;
    }
    40% {
      transform: scale(1);
      opacity: 1;
    }
  }
</style>
