<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
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
  import {
    createConversation,
    sendMessageStream,
    recipeAuthorBuildPrelude,
  } from "../../api";

  let { featureId, projectTitle }: { featureId: string; projectTitle: string } =
    $props();

  type ErrorKind =
    | "context_overflow"   // prompt + history > n_ctx, recoverable by reset
    | "generic";           // anything else; show inline ⚠

  type Message = {
    id: string;
    role: "user" | "assistant";
    content: string;
    streaming: boolean;
    error?: string;
    errorKind?: ErrorKind;
  };

  let conversationId: string | null = $state(null);
  let messages: Message[] = $state([]);
  let composerValue = $state("");
  let sending = $state(false);
  let transcriptRef: HTMLDivElement | null = $state(null);

  /// Classify a raw inference error string into something the UI can
  /// act on. Today only context-overflow gets a recovery affordance;
  /// other failures (network, model load, tool errors) surface
  /// generically inline.
  function classifyError(msg: string): ErrorKind {
    const lower = msg.toLowerCase();
    if (
      lower.includes("prompt decode failed") ||
      lower.includes("decode error -3") ||
      lower.includes("prompt too long") ||
      lower.includes("context window")
    ) {
      return "context_overflow";
    }
    return "generic";
  }

  /// Reset to a fresh conversation. Wired to the "Start fresh"
  /// recovery action below context-overflow errors AND to a
  /// header-level "Reset" affordance so users can proactively
  /// clear context before hitting the wall.
  async function resetConversation(): Promise<void> {
    await resetForProject(featureId);
  }

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

  // Surface skill id — tags every conversation this surface creates
  // so routing applies the recipe-author intent_policy. Co-located
  // with the surface that owns it (2026-05-24 architecture redesign).
  const SURFACE_SKILL_ID = "recipe-author";

  async function resetForProject(_id: string): Promise<void> {
    messages = [];
    composerValue = "";
    sending = false;
    try {
      const resp = await createConversation(SURFACE_SKILL_ID);
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
        // Mark the in-flight assistant message (if any) as errored
        // AND classify so the renderer can pick the right affordance
        // — generic ⚠ for one-off failures, recovery card for the
        // context-overflow case that traps long sessions.
        const kind = classifyError(event.payload.message);
        const inflight = messages.find((m) => m.streaming);
        if (inflight) {
          patchMessage(inflight.id, {
            streaming: false,
            error: event.payload.message,
            errorKind: kind,
          });
        } else {
          appendMessage({
            id: `err-${Date.now()}`,
            role: "assistant",
            content: "",
            streaming: false,
            error: event.payload.message,
            errorKind: kind,
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
      // Fetch the per-turn project-state prelude. Without this the
      // agent has no signal about which project / recipe is active
      // and falls back to asking the user to paste everything. See
      // `recipe_author_build_prelude` for what the block contains.
      // Best-effort: a prelude failure (project moved on disk,
      // notes.db unavailable) shouldn't block the message — fall
      // through and send the raw text with a debug log.
      let prelude = "";
      try {
        prelude = await recipeAuthorBuildPrelude(featureId);
      } catch (err) {
        console.warn("recipe-author chat: build prelude failed:", err);
      }
      const augmented = prelude ? `${prelude}${text}` : text;
      const resp = await sendMessageStream(augmented, conversationId);
      appendMessage({
        id: resp.message_id,
        role: "assistant",
        content: "",
        streaming: true,
      });
    } catch (e) {
      const msg = String(e);
      appendMessage({
        id: `err-${Date.now()}`,
        role: "assistant",
        content: "",
        streaming: false,
        error: msg,
        errorKind: classifyError(msg),
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
    <div class="chat-head-right">
      <span class="conv" title={conversationId ?? ""}>
        {#if conversationId}conversation #{conversationId.slice(0, 8)}{/if}
      </span>
      <button
        type="button"
        class="head-action"
        onclick={resetConversation}
        title="Start a fresh conversation — context resets but the project's decisions, findings, and checkpoints are preserved on disk"
        disabled={messages.length === 0 || sending}
        data-testid="recipe-author-reset"
      >
        Reset
      </button>
    </div>
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
          {#if m.content || m.streaming}
            <div class="content">
              {m.content}
              {#if m.streaming}<span class="cursor">▋</span>{/if}
            </div>
          {/if}
          {#if m.error && m.errorKind === "context_overflow"}
            <!-- Context-overflow recovery card. Triggered when the
                 chat conversation has grown beyond the model's
                 n_ctx and the prompt decode fails. The project
                 itself (decisions, findings, checkpoints, the
                 recipe TOML) is preserved on disk by the
                 recipe-author tools; only the in-memory chat
                 transcript resets. -->
            <div class="overflow-card" role="alert">
              <div class="overflow-icon" aria-hidden="true">⊘</div>
              <div class="overflow-body">
                <p class="overflow-title">Conversation hit the model's context limit</p>
                <p class="overflow-desc">
                  The agent's working memory is full. Your project's
                  decisions, findings, and the recipe draft are saved
                  on disk — starting fresh keeps all of those and
                  just clears the chat transcript.
                </p>
                <div class="overflow-actions">
                  <button
                    type="button"
                    class="overflow-primary"
                    onclick={resetConversation}
                    disabled={sending}
                  >
                    Start fresh conversation
                  </button>
                  <details class="overflow-details">
                    <summary>Show error</summary>
                    <pre class="overflow-raw">{m.error}</pre>
                  </details>
                </div>
              </div>
            </div>
          {:else if m.error}
            <div class="msg-error" role="alert">
              <span class="msg-error-icon" aria-hidden="true">⚠</span>
              <span class="msg-error-text">{m.error}</span>
            </div>
          {/if}
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
    font-family: var(--font-sans);
    color: var(--text-primary);
  }
  .chat-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 10px 16px;
    border-bottom: 1px solid var(--border);
    font-size: 0.85rem;
  }
  .chat-head-right {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .project {
    font-weight: 600;
    color: var(--text-primary);
  }
  .conv {
    color: var(--text-muted);
    font-size: 0.74rem;
    font-family: var(--font-mono);
    letter-spacing: 0.02em;
  }
  .head-action {
    background: transparent;
    border: 1px solid var(--border-mid);
    color: var(--text-secondary);
    border-radius: var(--radius);
    padding: 3px 10px;
    font-size: 0.74rem;
    font-family: inherit;
    cursor: pointer;
    transition: border-color 120ms ease, color 120ms ease, background 120ms ease;
  }
  .head-action:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent-light);
    background: var(--accent-glow);
  }
  .head-action:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .transcript {
    flex: 1 1 auto;
    overflow-y: auto;
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .placeholder {
    color: var(--text-muted);
    font-size: 0.9rem;
    font-style: italic;
    text-align: center;
    margin: auto;
    max-width: 36ch;
    line-height: 1.5;
  }
  .msg {
    display: flex;
    flex-direction: column;
    gap: 4px;
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
    font-size: 0.66rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.1em;
    font-weight: 600;
  }
  .content {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 9px 12px;
    font-size: 0.9rem;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--text-primary);
  }
  .msg.user .content {
    background: var(--lavender-glow);
    border-color: color-mix(in srgb, var(--lavender) 30%, transparent);
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

  /* Generic inline error — used for one-off failures (tool errors,
     network blips). Reads as a sibling of `.content`, never replaces
     it, so partial-stream output stays visible above the warn line. */
  .msg-error {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    margin-top: 4px;
    padding: 7px 10px;
    background: var(--coral-dim);
    border: 1px solid color-mix(in srgb, var(--coral) 35%, transparent);
    border-radius: var(--radius);
    color: var(--coral);
    font-size: 0.82rem;
    line-height: 1.45;
  }
  .msg-error-icon {
    flex-shrink: 0;
  }
  .msg-error-text {
    word-break: break-word;
  }

  /* Context-overflow recovery card. Distinct from `.msg-error`
     because it carries an action — collapsing into the same coral
     wash would hide that affordance. The icon (⊘ "context full")
     and the explicit "Start fresh" button signal recoverable. */
  .overflow-card {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    margin-top: 4px;
    padding: 14px 16px;
    background: var(--accent-glow);
    border: 1px solid color-mix(in srgb, var(--accent) 40%, transparent);
    border-radius: var(--radius-lg);
    color: var(--text-primary);
  }
  .overflow-icon {
    flex-shrink: 0;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    background: var(--accent-dim);
    border: 1px solid color-mix(in srgb, var(--accent) 50%, transparent);
    color: var(--accent-light);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.95rem;
    font-weight: 600;
  }
  .overflow-body {
    flex: 1 1 auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .overflow-title {
    margin: 0;
    font-size: 0.92rem;
    font-weight: 600;
    color: var(--accent-light);
    letter-spacing: -0.005em;
  }
  .overflow-desc {
    margin: 0;
    font-size: 0.84rem;
    color: var(--text-secondary);
    line-height: 1.5;
  }
  .overflow-actions {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    margin-top: 4px;
  }
  .overflow-primary {
    background: var(--accent);
    color: var(--text-on-accent);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    padding: 6px 14px;
    font-size: 0.84rem;
    font-weight: 500;
    font-family: inherit;
    cursor: pointer;
    transition: background 120ms ease;
  }
  .overflow-primary:hover:not(:disabled) {
    background: var(--accent-hover);
  }
  .overflow-primary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .overflow-details {
    font-size: 0.76rem;
    color: var(--text-muted);
  }
  .overflow-details summary {
    cursor: pointer;
    user-select: none;
    padding: 3px 0;
    color: var(--text-muted);
  }
  .overflow-details summary:hover {
    color: var(--text-secondary);
  }
  .overflow-raw {
    margin: 6px 0 0;
    padding: 8px 10px;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-family: var(--font-mono);
    font-size: 0.74rem;
    line-height: 1.5;
    color: var(--text-secondary);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 180px;
    overflow-y: auto;
  }

  .composer {
    display: flex;
    gap: 8px;
    padding: 12px 16px;
    border-top: 1px solid var(--border);
    background: transparent;
  }
  textarea {
    flex: 1 1 auto;
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    color: var(--text-primary);
    padding: 8px 10px;
    border-radius: var(--radius);
    resize: vertical;
    font-family: var(--font-sans);
    font-size: 0.9rem;
    line-height: 1.5;
    transition: border-color 120ms ease, background 120ms ease;
  }
  textarea::placeholder {
    color: var(--text-muted);
  }
  textarea:focus {
    outline: none;
    border-color: var(--accent);
    background: var(--bg-surface);
  }
  .send {
    background: var(--lavender-dim);
    border: 1px solid color-mix(in srgb, var(--lavender) 50%, transparent);
    color: var(--lavender-light);
    padding: 0 18px;
    border-radius: var(--radius);
    cursor: pointer;
    font-family: inherit;
    font-size: 0.88rem;
    font-weight: 500;
    transition: background 120ms ease, color 120ms ease, border-color 120ms ease;
  }
  .send:hover:not(:disabled) {
    background: color-mix(in srgb, var(--lavender) 25%, transparent);
    border-color: var(--lavender);
    color: var(--text-primary);
  }
  .send:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
</style>
