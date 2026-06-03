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
    <button onclick={startNew}>New</button>
  </header>
  {#each convos as c (c.id)}
    <button class="row" onclick={() => onopen(c.id)}>
      {c.title ?? "Untitled"}
    </button>
  {:else}
    <p class="muted">No conversations yet — start one.</p>
  {/each}
</div>

<style>
  .list {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    padding: 1rem;
    overflow-y: auto;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .row {
    background: var(--surface);
    color: var(--text);
    text-align: left;
    font-weight: 400;
  }
  .muted {
    color: var(--muted);
  }
</style>
