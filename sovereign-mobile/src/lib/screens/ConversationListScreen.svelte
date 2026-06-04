<script lang="ts">
  import { onMount } from "svelte";
  import { createConversation, listConversations } from "../api";
  import type { ConversationSummary } from "../types";

  let { onopen }: { onopen: (id: string) => void } = $props();

  let convos = $state<ConversationSummary[]>([]);

  async function refresh() {
    try {
      convos = await listConversations();
    } catch {
      /* offline + empty cache → leave list empty */
    }
  }

  async function startNew() {
    const id = await createConversation();
    onopen(id);
  }

  onMount(refresh);
</script>

<div class="list">
  <header>
    <h1>Conversations</h1>
    <button class="new-btn" onclick={startNew}><span class="plus">+</span> New</button>
  </header>
  <div class="rows">
    {#each convos as c (c.id)}
      <button class="row" onclick={() => onopen(c.id)}>
        <span class="title">{c.title ?? "Untitled"}</span>
      </button>
    {:else}
      <p class="empty">No conversations yet — start one.</p>
    {/each}
  </div>
</div>

<style>
  .list {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 1rem 1.1rem 0.7rem;
    position: sticky;
    top: 0;
    z-index: 2;
    background: linear-gradient(
      var(--bg-root) 55%,
      color-mix(in srgb, var(--bg-root) 0%, transparent)
    );
  }
  h1 {
    font-family: var(--font-sans);
    font-size: 1.5rem;
    font-weight: 600;
    letter-spacing: -0.02em;
    color: var(--text-primary);
  }
  .new-btn {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 0.85rem;
    font-weight: 500;
    color: var(--accent);
    background: var(--accent-dim);
    border: 1px solid color-mix(in srgb, var(--accent) 38%, transparent);
    border-radius: var(--radius);
    padding: 0.45rem 0.8rem;
    transition: background 0.15s;
  }
  .new-btn:active { background: color-mix(in srgb, var(--accent) 22%, transparent); }
  .plus { font-size: 1.05em; line-height: 1; }

  .rows {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.4rem 1.1rem calc(1rem + env(safe-area-inset-bottom));
  }
  .row {
    text-align: left;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 0.85rem 0.95rem;
    color: var(--text-primary);
    font-size: 0.92rem;
    font-weight: 420;
    line-height: 1.45;
    transition: border-color 0.15s, background 0.15s;
  }
  .row:active {
    background: var(--bg-elevated);
    border-color: var(--border-bright);
  }
  .title {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .empty {
    color: var(--text-muted);
    font-size: 0.9rem;
    padding: 1.5rem 1.1rem;
  }
</style>
