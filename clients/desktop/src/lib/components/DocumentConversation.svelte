<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { askDocument, createConversation } from "../api";
  import type {
    DocumentAsset,
    DocumentAssetOperation,
    DocumentOperationPayload,
  } from "../types";
  import OperationBadge from "./OperationBadge.svelte";
  import IngestBanner from "./IngestBanner.svelte";

  interface Props {
    asset: DocumentAsset;
    onBack: () => void;
  }

  let { asset, onBack }: Props = $props();

  interface DocMessage {
    role: "user" | "assistant";
    content: string;
    operation?: DocumentAssetOperation;
    sources?: string[];
    error?: boolean;
  }

  let messages: DocMessage[] = $state([]);
  let inputText = $state("");
  let isLoading = $state(false);
  let currentOp: string | null = $state(null);
  let unlisten: (() => void) | undefined;
  let conversationId: string | null = $state(null);

  onMount(async () => {
    unlisten = await listen<DocumentOperationPayload>(
      "document:operation",
      ({ payload }) => {
        currentOp = payload.operation ?? payload.type;
      },
    );
  });

  async function ensureConversation(): Promise<string> {
    if (conversationId) return conversationId;
    const created = await createConversation();
    conversationId = created.id;
    return conversationId;
  }

  onDestroy(() => unlisten?.());

  function isQueryable(): boolean {
    const s = asset.state;
    return (
      s === "PartiallyReady" ||
      s === "MultiHopReady" ||
      s === "Ready" ||
      (typeof s === "object" && "BuildingSkeleton" in s)
    );
  }

  async function send() {
    const text = inputText.trim();
    if (!text || isLoading) return;

    inputText = "";
    isLoading = true;
    currentOp = null;

    messages = [...messages, { role: "user", content: text }];

    try {
      const convoId = await ensureConversation();
      const result = await askDocument(asset.id, text, convoId);
      messages = [
        ...messages,
        {
          role: "assistant",
          content: result.response,
          operation: result.operation,
          sources: result.sources,
        },
      ];
    } catch (e) {
      messages = [
        ...messages,
        {
          role: "assistant",
          content: "Something went wrong. Please try again.",
          error: true,
        },
      ];
    } finally {
      isLoading = false;
      currentOp = null;
    }
  }

  function formatWords(n: number): string {
    if (n >= 1000) return `${(n / 1000).toFixed(0)}K`;
    return String(n);
  }

  function starterPrompts(): { label: string; question: string }[] {
    if (!asset.skeleton) return [];
    const prompts: { label: string; question: string }[] = [];

    if (asset.document_type === "Narrative") {
      prompts.push({
        label: "Character arcs",
        question: "Who are the main characters and how do they develop?",
      });
      prompts.push({
        label: "Plot arc",
        question: "What is the plot arc?",
      });
    } else if (asset.document_type === "Argument") {
      prompts.push({
        label: "Central argument",
        question: "What is the central thesis and how is it argued?",
      });
      prompts.push({
        label: "Objections",
        question: "What are the main objections addressed?",
      });
    } else {
      prompts.push({
        label: "Overview",
        question: "What is this document about?",
      });
    }

    // Entity-specific prompts from skeleton.
    for (const entity of asset.skeleton.main_entities.slice(0, 3)) {
      prompts.push({
        label: entity.name,
        question: `Tell me about ${entity.name}`,
      });
    }

    return prompts;
  }
</script>

<div class="doc-conversation">
  <!-- Header -->
  <div class="doc-header">
    <button class="back-btn" onclick={onBack} title="Back to library">
      <svg
        width="12"
        height="12"
        viewBox="0 0 12 12"
        fill="none"
        aria-hidden="true"
      >
        <path
          d="M8 1L3 6l5 5"
          stroke="currentColor"
          stroke-width="1.6"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
    </button>
    <div class="doc-header-text">
      <div class="doc-header-title">{asset.title}</div>
      <div class="doc-header-meta">
        {formatWords(asset.word_count)} words
        &middot; {asset.document_type}
        {#if asset.skeleton}
          &middot; {asset.skeleton.main_entities.length} main entities
        {/if}
      </div>
    </div>
  </div>

  <!-- Ingest banner -->
  {#if asset.state !== "Ready"}
    <IngestBanner {asset} />
  {/if}

  <!-- Thread -->
  <div class="doc-thread">
    {#if messages.length === 0}
      <div class="starter-prompts">
        <p class="starter-intro">Ask anything about this document</p>

        {#if asset.skeleton}
          <div class="prompt-suggestions">
            {#each starterPrompts() as prompt}
              <button
                class="prompt-chip"
                onclick={() => {
                  inputText = prompt.question;
                  send();
                }}
              >
                {prompt.label}
              </button>
            {/each}
          </div>
        {/if}
      </div>
    {/if}

    {#each messages as message, i (i)}
      {#if message.role === "user"}
        <div class="user-message">
          <p>{message.content}</p>
        </div>
      {:else}
        <div class="assistant-message">
          {#if message.operation}
            <OperationBadge operation={message.operation} />
          {/if}
          <div class="sv-response-text">
            {message.content}
          </div>
          {#if message.sources && message.sources.length > 0}
            <details class="source-panel">
              <summary class="source-toggle">
                {message.sources.length} source passage{message.sources
                  .length === 1
                  ? ""
                  : "s"}
              </summary>
              <div class="source-list">
                {#each message.sources as source, j}
                  <div class="source-item">
                    <span class="source-num">[{j + 1}]</span>
                    <span class="source-text"
                      >{source.slice(0, 200)}{source.length > 200
                        ? "\u2026"
                        : ""}</span
                    >
                  </div>
                {/each}
              </div>
            </details>
          {/if}
        </div>
      {/if}
    {/each}

    {#if isLoading}
      <div class="loading-state">
        {#if currentOp}
          <span class="loading-op">{currentOp}</span>
        {:else}
          <span class="loading-op">Thinking&hellip;</span>
        {/if}
      </div>
    {/if}
  </div>

  <!-- Input -->
  <div class="doc-input-area">
    <textarea
      class="doc-input"
      bind:value={inputText}
      placeholder="Ask anything about this document..."
      onkeydown={(e) => {
        if (e.key === "Enter" && !e.shiftKey) {
          e.preventDefault();
          send();
        }
      }}
      disabled={!isQueryable() || isLoading}
      rows={1}
    ></textarea>
    <button
      class="send-btn"
      onclick={send}
      disabled={!inputText.trim() || isLoading || !isQueryable()}
    >
      &rarr;
    </button>
  </div>
</div>

<style>
  .doc-conversation {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .doc-header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 16px 24px 12px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .back-btn {
    color: var(--text-muted);
    background: none;
    border: none;
    cursor: pointer;
    padding: 4px;
    border-radius: 4px;
    flex-shrink: 0;
  }
  .back-btn:hover {
    color: var(--text-primary);
    background: var(--bg-elevated);
  }
  .doc-header-text {
    min-width: 0;
  }
  .doc-header-title {
    font-size: 16px;
    font-weight: 600;
    font-family: var(--font-serif);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .doc-header-meta {
    font-size: 11px;
    color: var(--text-muted);
    margin-top: 3px;
  }
  .doc-thread {
    flex: 1;
    overflow-y: auto;
    padding: 24px;
  }
  .starter-prompts {
    text-align: center;
    padding: 48px 24px 24px;
  }
  .starter-intro {
    font-size: 14px;
    color: var(--text-muted);
    margin-bottom: 16px;
  }
  .prompt-suggestions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    justify-content: center;
  }
  .prompt-chip {
    padding: 6px 14px;
    border: 1px solid var(--border);
    border-radius: 20px;
    font-size: 12px;
    background: var(--bg-surface);
    color: var(--text-secondary);
    cursor: pointer;
  }
  .prompt-chip:hover {
    background: var(--bg-elevated);
    color: var(--text-primary);
  }
  .user-message {
    display: flex;
    justify-content: flex-end;
    margin-bottom: 20px;
  }
  .user-message p {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 12px 12px 2px 12px;
    padding: 10px 14px;
    font-size: 14px;
    max-width: 70%;
    color: var(--text-primary);
  }
  .assistant-message {
    margin-bottom: 28px;
  }
  .sv-response-text {
    font-size: 14px;
    line-height: 1.7;
    color: var(--text-primary);
    font-family: var(--font-serif);
  }
  .source-panel {
    margin-top: 10px;
  }
  .source-toggle {
    font-size: 11px;
    color: var(--text-muted);
    cursor: pointer;
  }
  .source-toggle:hover {
    color: var(--text-secondary);
  }
  .source-list {
    margin-top: 8px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .source-item {
    display: flex;
    gap: 6px;
    font-size: 11px;
    color: var(--text-muted);
    line-height: 1.5;
  }
  .source-num {
    flex-shrink: 0;
    font-weight: 600;
    color: var(--text-secondary);
  }
  .source-text {
    font-style: italic;
  }
  .loading-state {
    font-size: 12px;
    color: var(--text-muted);
    padding: 8px 0;
    font-style: italic;
  }
  .doc-input-area {
    padding: 16px 24px;
    border-top: 1px solid var(--border);
    display: flex;
    gap: 8px;
    align-items: flex-end;
    flex-shrink: 0;
  }
  .doc-input {
    flex: 1;
    padding: 10px 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 14px;
    resize: none;
    font-family: var(--font-sans, sans-serif);
    background: var(--bg-input);
    color: var(--text-primary);
  }
  .doc-input:disabled {
    opacity: 0.5;
  }
  .doc-input:focus {
    outline: none;
    border-color: var(--accent);
  }
  .send-btn {
    padding: 10px 16px;
    background: var(--text-primary);
    color: var(--bg-primary);
    border: none;
    border-radius: var(--radius);
    cursor: pointer;
    font-size: 16px;
  }
  .send-btn:disabled {
    opacity: 0.3;
    cursor: default;
  }
</style>
