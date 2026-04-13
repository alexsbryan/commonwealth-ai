<script lang="ts">
  import { onMount } from "svelte";
  import {
    listConversations,
    createConversation,
    deleteConversation,
  } from "../api";
  import type { ConversationEntry } from "../types";
  import MeshStatusIndicator from "./MeshStatusIndicator.svelte";

  interface Props {
    selectedConversationId: string | null;
    onSelect: (id: string | null) => void;
    onToggleSettings: () => void;
  }

  let { selectedConversationId, onSelect, onToggleSettings }: Props = $props();

  let conversations: ConversationEntry[] = $state([]);

  onMount(() => {
    loadConversations();
  });

  export async function loadConversations() {
    try {
      conversations = await listConversations(50, 0);
    } catch (e) {
      console.error("Failed to load conversations:", e);
    }
  }

  async function handleNew() {
    try {
      const created = await createConversation();
      conversations = [
        {
          id: created.id,
          title: null,
          created_at: created.created_at,
          updated_at: created.created_at,
        },
        ...conversations,
      ];
      onSelect(created.id);
    } catch (e) {
      console.error("Failed to create conversation:", e);
    }
  }

  async function handleDelete(id: string, event: Event) {
    event.stopPropagation();
    try {
      await deleteConversation(id);
      conversations = conversations.filter((c) => c.id !== id);
      if (selectedConversationId === id) {
        onSelect(null);
      }
    } catch (e) {
      console.error("Failed to delete conversation:", e);
    }
  }

  function formatTime(epoch: number): string {
    const date = new Date(epoch * 1000);
    const now = new Date();
    const diff = now.getTime() - date.getTime();
    const days = Math.floor(diff / (1000 * 60 * 60 * 24));

    if (days === 0) return "Today";
    if (days === 1) return "Yesterday";
    if (days < 7) return `${days}d ago`;
    return date.toLocaleDateString();
  }
</script>

<div class="conversation-list">
  <div class="sidebar-brand">
    <span class="brand-mark">◈</span>
    <span class="brand-name">SOVEREIGN</span>
    <button class="settings-btn" onclick={onToggleSettings} title="Settings">
      <svg width="15" height="15" viewBox="0 0 15 15" fill="none" aria-hidden="true">
        <circle cx="7.5" cy="7.5" r="2.2" stroke="currentColor" stroke-width="1.4"/>
        <path d="M7.5 1.5v1.8M7.5 11.7v1.8M1.5 7.5h1.8M11.7 7.5h1.8M3.2 3.2l1.3 1.3M10.5 10.5l1.3 1.3M3.2 11.8l1.3-1.3M10.5 4.5l1.3-1.3" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
      </svg>
    </button>
  </div>

  <div class="list-header">
    <button class="new-btn" onclick={handleNew}>
      <svg width="11" height="11" viewBox="0 0 11 11" fill="none" aria-hidden="true">
        <path d="M5.5 1v9M1 5.5h9" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>
      </svg>
      New conversation
    </button>
  </div>

  <div class="list-items">
    {#each conversations as convo (convo.id)}
      <div
        class="convo-item"
        class:selected={selectedConversationId === convo.id}
        role="button"
        tabindex="0"
        onclick={() => onSelect(convo.id)}
        onkeydown={(e) => e.key === "Enter" && onSelect(convo.id)}
      >
        <div class="convo-body">
          <span class="convo-title">
            {convo.title || "New conversation"}
          </span>
          <span class="convo-meta">
            {formatTime(convo.updated_at)}
          </span>
        </div>
        <button
          class="delete-btn"
          onclick={(e) => handleDelete(convo.id, e)}
          title="Delete"
        >
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden="true">
            <path d="M1 1l8 8M9 1L1 9" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
          </svg>
        </button>
      </div>
    {/each}

    {#if conversations.length === 0}
      <p class="empty">No conversations yet</p>
    {/if}
  </div>

  <div class="sidebar-footer">
    <MeshStatusIndicator onOpen={onToggleSettings} />
  </div>
</div>

<style>
  .conversation-list {
    display: flex;
    flex-direction: column;
    height: 100%;
  }

  /* ── Brand ── */
  .sidebar-brand {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 16px 14px 12px;
  }

  .brand-mark {
    color: var(--accent);
    font-size: 1.1rem;
    line-height: 1;
    filter: drop-shadow(0 0 6px rgba(201, 168, 76, 0.50));
    flex-shrink: 0;
  }

  .brand-name {
    flex: 1;
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.2em;
    color: var(--text-secondary);
    text-transform: uppercase;
  }

  .settings-btn {
    color: var(--text-muted);
    padding: 5px;
    border-radius: var(--radius);
    display: flex;
    align-items: center;
    justify-content: center;
    transition: color 0.2s, background 0.2s;
    flex-shrink: 0;
  }

  .settings-btn:hover {
    color: var(--text-secondary);
    background: var(--bg-surface);
  }

  /* ── New button ── */
  .list-header {
    padding: 0 10px 10px;
  }

  .new-btn {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    padding: 8px 12px;
    background: transparent;
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    color: var(--text-muted);
    font-size: 0.8rem;
    font-weight: 600;
    letter-spacing: 0.03em;
    transition: all 0.2s;
  }

  .new-btn:hover {
    background: var(--accent-dim);
    border-color: var(--accent);
    color: var(--accent);
  }

  /* ── Conversation list ── */
  .list-items {
    flex: 1;
    overflow-y: auto;
    padding: 2px 0;
  }

  .convo-item {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 9px 14px;
    border-left: 2px solid transparent;
    cursor: pointer;
    transition: background 0.15s, border-color 0.15s;
  }

  .convo-item:hover {
    background: var(--bg-surface);
    border-left-color: var(--border-bright);
  }

  .convo-item.selected {
    background: var(--bg-surface);
    border-left-color: var(--accent);
  }

  .convo-body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .convo-title {
    display: block;
    font-size: 0.84rem;
    font-weight: 500;
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    transition: color 0.15s;
  }

  .convo-item.selected .convo-title {
    color: var(--accent);
  }

  .convo-meta {
    display: block;
    font-size: 0.67rem;
    color: var(--text-muted);
    font-family: 'Syne Mono', monospace;
    letter-spacing: 0.02em;
  }

  .delete-btn {
    opacity: 0;
    flex-shrink: 0;
    color: var(--text-muted);
    padding: 3px;
    border-radius: 3px;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: opacity 0.15s, color 0.15s, background 0.15s;
  }

  .convo-item:hover .delete-btn {
    opacity: 1;
  }

  .delete-btn:hover {
    color: var(--error);
    background: rgba(212, 72, 72, 0.1);
  }

  .empty {
    text-align: center;
    color: var(--text-muted);
    padding: 2.5rem 1rem;
    font-size: 0.82rem;
    letter-spacing: 0.03em;
  }

  /* ── Footer ── */
  .sidebar-footer {
    padding: 10px 10px 12px;
    border-top: 1px solid var(--border);
  }
</style>
