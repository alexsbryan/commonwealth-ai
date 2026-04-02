<script lang="ts">
  import { onMount } from "svelte";
  import {
    listConversations,
    createConversation,
    deleteConversation,
  } from "../api";
  import type { ConversationEntry } from "../types";

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

  async function loadConversations() {
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
  <div class="list-header">
    <button class="new-btn" onclick={handleNew}>+ New Chat</button>
    <button class="settings-btn" onclick={onToggleSettings} title="Settings">
      &#9881;
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
        <span class="convo-title">
          {convo.title || "New conversation"}
        </span>
        <span class="convo-meta">
          {formatTime(convo.updated_at)}
        </span>
        <button
          class="delete-btn"
          onclick={(e) => handleDelete(convo.id, e)}
          title="Delete"
        >
          &times;
        </button>
      </div>
    {/each}

    {#if conversations.length === 0}
      <p class="empty">No conversations yet</p>
    {/if}
  </div>
</div>

<style>
  .conversation-list {
    display: flex;
    flex-direction: column;
    height: 100%;
  }

  .list-header {
    display: flex;
    gap: 8px;
    padding: 12px;
    border-bottom: 1px solid var(--border);
  }

  .new-btn {
    flex: 1;
    padding: 8px 12px;
    background: var(--accent);
    color: white;
    border-radius: var(--radius);
    font-weight: 500;
    transition: background 0.2s;
  }

  .new-btn:hover {
    background: var(--accent-hover);
  }

  .settings-btn {
    padding: 8px 12px;
    background: var(--bg-surface);
    border-radius: var(--radius);
    font-size: 1.1rem;
    transition: background 0.2s;
  }

  .settings-btn:hover {
    background: var(--border);
  }

  .list-items {
    flex: 1;
    overflow-y: auto;
    padding: 4px 0;
  }

  .convo-item {
    display: flex;
    align-items: center;
    width: 100%;
    padding: 10px 12px;
    text-align: left;
    transition: background 0.15s;
    position: relative;
    cursor: pointer;
  }

  .convo-item:hover {
    background: var(--bg-surface);
  }

  .convo-item.selected {
    background: var(--bg-surface);
    border-left: 3px solid var(--accent);
  }

  .convo-title {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.9rem;
  }

  .convo-meta {
    font-size: 0.75rem;
    color: var(--text-muted);
    margin-left: 8px;
    white-space: nowrap;
  }

  .delete-btn {
    opacity: 0;
    padding: 2px 6px;
    font-size: 1rem;
    color: var(--text-muted);
    margin-left: 4px;
    border-radius: 4px;
    transition:
      opacity 0.15s,
      color 0.15s;
  }

  .convo-item:hover .delete-btn {
    opacity: 1;
  }

  .delete-btn:hover {
    color: var(--error);
    background: rgba(244, 67, 54, 0.1);
  }

  .empty {
    text-align: center;
    color: var(--text-muted);
    padding: 2rem;
    font-size: 0.9rem;
  }
</style>
