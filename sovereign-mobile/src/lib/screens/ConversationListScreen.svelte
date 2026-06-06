<script lang="ts">
  import { onMount } from "svelte";
  import { createConversation, listConversations, removeHostConnection } from "../api";
  import type { ConversationSummary, HostConnection } from "../types";

  // `host` is the active connection (for the manage-host chip); `ondisconnect`
  // lets App.svelte re-derive `paired` after a removal so it can route back to
  // the pairing screen.
  let {
    onopen,
    host,
    ondisconnect,
  }: {
    onopen: (id: string) => void;
    host: HostConnection | null;
    ondisconnect: () => void;
  } = $props();

  let convos = $state<ConversationSummary[]>([]);
  let menuOpen = $state(false);

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

  // Remove the active host, then hand control back to App.svelte. Removing the
  // only host drops `paired`, so the pairing screen returns — this is also the
  // "change host" path (remove, then pair again with a new address/token).
  async function disconnect() {
    if (host) {
      try {
        await removeHostConnection(host.id);
      } catch {
        /* best-effort: even on error, fall through to re-pairing */
      }
    }
    menuOpen = false;
    ondisconnect();
  }

  onMount(refresh);
</script>

<div class="list">
  <header>
    <div class="head-left">
      <h1>Conversations</h1>
      {#if host}
        <button
          class="host-chip"
          onclick={() => (menuOpen = !menuOpen)}
          aria-label={`Host: ${host.display_name}. Tap to manage.`}
          aria-expanded={menuOpen}
        >
          <span class="dot" aria-hidden="true">◈</span>
          {host.display_name}
        </button>
      {/if}
    </div>
    <button class="new-btn" onclick={startNew} aria-label="New conversation">
      <span class="plus" aria-hidden="true">+</span> New
    </button>
  </header>
  {#if menuOpen && host}
    <div class="host-menu" role="dialog" aria-label="Host connection">
      <div class="host-addr">{host.tailnet_address}</div>
      <div class="host-actions">
        <button class="disconnect" onclick={disconnect}>
          Disconnect / change host
        </button>
        <button class="cancel" onclick={() => (menuOpen = false)}>Cancel</button>
      </div>
    </div>
  {/if}
  <div class="rows" role="group" aria-label="Conversations">
    {#each convos as c (c.id)}
      <button
        class="row"
        onclick={() => onopen(c.id)}
        aria-label={`Open conversation: ${c.title ?? "Untitled"}`}
      >
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
    /* Cap the column on tablets/landscape; full-width with gutters on a
       phone. Centered so it never stretches edge-to-edge. */
    width: 100%;
    max-width: var(--measure);
    margin-inline: auto;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 1rem var(--pad-r) 0.7rem var(--pad-l);
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

  .head-left {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.2rem;
    min-width: 0;
  }
  /* The connected-host chip — the entry point to change/remove the host. */
  .host-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.28rem;
    max-width: 100%;
    font-family: var(--font-sans);
    font-size: 0.72rem;
    font-weight: 500;
    color: var(--lavender-light);
    background: var(--lavender-dim);
    border: 1px solid color-mix(in srgb, var(--lavender) 24%, transparent);
    border-radius: 999px;
    padding: 0.16rem 0.5rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .host-chip .dot { font-size: 0.7em; opacity: 0.7; }
  .host-chip:active { background: color-mix(in srgb, var(--lavender) 22%, transparent); }

  .host-menu {
    margin: 0.1rem var(--pad-r) 0.4rem var(--pad-l);
    padding: 0.7rem 0.8rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border-bright);
    border-radius: var(--radius-lg);
  }
  .host-addr {
    font-family: var(--font-mono);
    font-size: 0.74rem;
    color: var(--text-muted);
    margin-bottom: 0.55rem;
    word-break: break-all;
  }
  .host-actions { display: flex; gap: 0.5rem; }
  .host-actions button {
    flex: 1;
    font-size: 0.8rem;
    font-weight: 500;
    border-radius: var(--radius);
    padding: 0.5rem 0.6rem;
  }
  .disconnect {
    color: var(--danger, #e5837a);
    background: color-mix(in srgb, var(--danger, #e5837a) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger, #e5837a) 34%, transparent);
  }
  .disconnect:active { background: color-mix(in srgb, var(--danger, #e5837a) 22%, transparent); }
  .cancel {
    color: var(--text-secondary, var(--text-muted));
    background: var(--bg-surface);
    border: 1px solid var(--border);
  }

  .rows {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.4rem var(--pad-r) calc(1rem + env(safe-area-inset-bottom)) var(--pad-l);
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
    padding: 1.5rem var(--pad-l);
  }
</style>
