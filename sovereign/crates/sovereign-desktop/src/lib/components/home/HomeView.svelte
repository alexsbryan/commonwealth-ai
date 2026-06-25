<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Home — the hub / landing (Phase 2 UX refactor, D5).

  A launcher + status, not a deep surface: an ask box (→ global Ask), a
  recent-notebooks strip (→ Library / a notebook), and "pick up where you
  left off" (→ a prior conversation). Pure composition over data that
  already exists — `notebook_list`, `list_conversations`, the chat seed.
  Depth lives in Ask and Library; Home stays a summary so it never
  becomes a second Library.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { notebookList, listConversations } from "../../api";
  import type { NotebookSummary, ConversationEntry } from "../../types";
  import NotebookKindIcon from "../library/NotebookKindIcon.svelte";
  import { kindLabel } from "../library/notebookKind";

  let {
    onAsk,
    onOpenLibrary,
    onOpenNotebook,
    onOpenConversation,
    onAdd,
  }: {
    onAsk: (text: string) => void;
    onOpenLibrary: () => void;
    onOpenNotebook: (nb: NotebookSummary) => void;
    onOpenConversation: (id: string) => void;
    onAdd: () => void;
  } = $props();

  let notebooks = $state<NotebookSummary[]>([]);
  let conversations = $state<ConversationEntry[]>([]);
  let loading = $state(true);
  let askText = $state("");

  // The strip is a launcher: a recent few, not the whole shelf.
  let recentNotebooks = $derived(notebooks.slice(0, 6));
  let recentThreads = $derived(
    [...conversations].sort((a, b) => b.updated_at - a.updated_at).slice(0, 5),
  );

  onMount(async () => {
    try {
      const [nb, convs] = await Promise.all([
        notebookList(),
        listConversations(12, 0).catch(() => []),
      ]);
      notebooks = nb;
      conversations = convs;
    } catch {
      notebooks = [];
    } finally {
      loading = false;
    }
  });

  function submitAsk() {
    const t = askText.trim();
    if (!t) return;
    askText = "";
    onAsk(t);
  }

  function onAskKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submitAsk();
    }
  }

  function freshness(unix: number | null): string {
    if (!unix) return "";
    const days = Math.floor((Date.now() / 1000 - unix) / 86400);
    if (days <= 0) return "today";
    if (days === 1) return "1d ago";
    if (days < 30) return `${days}d ago`;
    if (days < 365) return `${Math.floor(days / 30)}mo ago`;
    return `${Math.floor(days / 365)}y ago`;
  }

  function threadTitle(c: ConversationEntry): string {
    return c.title?.trim() || "Untitled conversation";
  }
</script>

<div class="home" data-testid="home-view">
  <div class="home-inner">
    {#if loading}
      <p class="muted">Loading…</p>
    {:else if notebooks.length === 0}
      <!-- First run — nothing to ask yet; lead with adding knowledge. -->
      <div class="empty" data-testid="home-empty">
        <h1>Nothing here yet — that's the fun part.</h1>
        <p>
          A folder of notes, PDFs, transcripts — anything. Sovereign reads it on
          your machine and turns it into something you can ask.
        </p>
        <div class="empty-actions">
          <button class="primary" onclick={onAdd} data-testid="home-empty-add">
            + Add a folder
          </button>
          <button class="secondary" onclick={onAdd} data-testid="home-empty-sample">
            Try a sample
          </button>
        </div>
      </div>
    {:else}
      <!-- ── Ask anything ── -->
      <section class="ask">
        <h2>Ask anything</h2>
        <div class="ask-row">
          <input
            type="text"
            bind:value={askText}
            onkeydown={onAskKeydown}
            placeholder="ask across everything you know…"
            data-testid="home-ask-input"
            aria-label="Ask across everything you know"
          />
          <button
            class="ask-go"
            onclick={submitAsk}
            disabled={!askText.trim()}
            data-testid="home-ask-submit"
            aria-label="Ask"
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M5 12h14" /><path d="m12 5 7 7-7 7" />
            </svg>
          </button>
        </div>
      </section>

      <!-- ── Your notebooks ── -->
      <section class="strip">
        <div class="strip-head">
          <h2>Your notebooks</h2>
          <button class="link" onclick={onOpenLibrary} data-testid="home-all-notebooks">All →</button>
        </div>
        <div class="tiles">
          {#each recentNotebooks as nb (nb.id)}
            <button
              class="tile"
              data-testid="home-notebook-tile"
              data-notebook-id={nb.id}
              onclick={() => onOpenNotebook(nb)}
            >
              <div class="tile-top">
                <span class="tile-icon"><NotebookKindIcon kind={nb.source_kind} size={17} /></span>
                {#if nb.explorable}<span class="tile-star" title="Has an explorable map">✦</span>{/if}
              </div>
              <div class="tile-name">{nb.name}</div>
              <div class="tile-meta">
                {kindLabel(nb.source_kind)} · {nb.doc_count.toLocaleString()}
                {#if freshness(nb.updated_unix)}<span class="tile-fresh"> · {freshness(nb.updated_unix)}</span>{/if}
              </div>
            </button>
          {/each}
          <button class="tile tile-add" onclick={onAdd} data-testid="home-add">
            <span class="add-plus" aria-hidden="true">+</span>
            <span class="add-label">Add</span>
          </button>
        </div>
      </section>

      <!-- ── Pick up where you left off ── -->
      {#if recentThreads.length > 0}
        <section class="threads">
          <h2>Pick up where you left off</h2>
          <ul>
            {#each recentThreads as c (c.id)}
              <li>
                <button
                  class="thread"
                  data-testid="home-thread"
                  data-conversation-id={c.id}
                  onclick={() => onOpenConversation(c.id)}
                >
                  <span class="thread-title">{threadTitle(c)}</span>
                  <span class="thread-when">{freshness(c.updated_at)}</span>
                </button>
              </li>
            {/each}
          </ul>
        </section>
      {/if}
    {/if}
  </div>
</div>

<style>
  .home {
    height: 100%;
    overflow-y: auto;
    background: var(--bg-primary);
  }
  .home-inner {
    max-width: 760px;
    margin: 0 auto;
    padding: 6vh 32px 40px;
  }
  .muted { color: var(--text-muted); font-size: 0.9rem; }

  h2 {
    font-size: 0.78rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-muted);
    margin: 0 0 12px;
  }

  /* ── Ask box ── */
  .ask { margin-bottom: 38px; }
  .ask-row {
    display: flex;
    gap: 8px;
    align-items: stretch;
  }
  .ask-row input {
    flex: 1;
    font: inherit;
    font-size: 1rem;
    padding: 14px 18px;
    border-radius: 12px;
    border: 1px solid var(--border-mid);
    background: var(--bg-secondary);
    color: var(--text-primary);
    outline: none;
    transition: border-color 160ms ease;
  }
  .ask-row input:focus {
    border-color: color-mix(in oklch, var(--accent) 55%, var(--border-mid));
  }
  .ask-go {
    flex-shrink: 0;
    width: 52px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 12px;
    border: none;
    background: var(--accent);
    color: var(--accent-contrast, #fff);
    cursor: pointer;
  }
  .ask-go:disabled { opacity: 0.45; cursor: default; }

  /* ── Notebooks strip ── */
  .strip { margin-bottom: 34px; }
  .strip-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }
  .link {
    font: inherit;
    font-size: 0.8rem;
    font-weight: 500;
    color: var(--text-secondary);
    background: none;
    border: none;
    cursor: pointer;
  }
  .link:hover { color: var(--accent); }
  .tiles {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
    gap: 12px;
  }
  .tile {
    text-align: left;
    font: inherit;
    cursor: pointer;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 13px 14px;
    border-radius: 11px;
    border: 1px solid var(--border);
    background: var(--bg-secondary);
    color: inherit;
    transition: border-color 150ms ease, transform 150ms ease;
  }
  .tile:hover {
    border-color: color-mix(in oklch, var(--accent) 35%, var(--border));
    transform: translateY(-1px);
  }
  .tile-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .tile-icon { color: var(--text-secondary); display: inline-flex; }
  .tile-star { color: var(--accent); font-size: 0.85rem; }
  .tile-name {
    font-weight: 600;
    font-size: 0.92rem;
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tile-meta { font-size: 0.74rem; color: var(--text-muted); }
  .tile-fresh { color: var(--text-muted); }

  .tile-add {
    align-items: center;
    justify-content: center;
    color: var(--text-muted);
    border-style: dashed;
    gap: 2px;
  }
  .tile-add:hover { color: var(--text-secondary); }
  .add-plus { font-size: 1.3rem; line-height: 1; }
  .add-label { font-size: 0.8rem; font-weight: 500; }

  /* ── Recent threads ── */
  .threads ul { list-style: none; margin: 0; padding: 0; }
  .threads li { border-bottom: 1px solid var(--border); }
  .thread {
    width: 100%;
    text-align: left;
    font: inherit;
    cursor: pointer;
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 14px;
    padding: 11px 4px;
    background: none;
    border: none;
    color: inherit;
  }
  .thread:hover .thread-title { color: var(--accent); }
  .thread-title {
    font-size: 0.9rem;
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .thread-when { font-size: 0.74rem; color: var(--text-muted); flex-shrink: 0; }

  /* ── Empty (first run) ── */
  .empty {
    text-align: center;
    max-width: 480px;
    margin: 8vh auto 0;
  }
  .empty h1 {
    font-size: 1.3rem;
    font-weight: 640;
    color: var(--text-primary);
    margin: 0 0 14px;
  }
  .empty p {
    color: var(--text-secondary);
    font-size: 0.92rem;
    line-height: 1.6;
    margin: 0 0 24px;
  }
  .empty-actions {
    display: flex;
    gap: 12px;
    justify-content: center;
  }
  button.primary {
    font: inherit;
    font-weight: 600;
    font-size: 0.9rem;
    padding: 11px 20px;
    border-radius: var(--radius);
    border: none;
    background: var(--accent);
    color: var(--accent-contrast, #fff);
    cursor: pointer;
  }
  button.secondary {
    font: inherit;
    font-weight: 500;
    font-size: 0.9rem;
    padding: 11px 20px;
    border-radius: var(--radius);
    border: 1px solid var(--border-mid);
    background: var(--bg-elevated);
    color: var(--text-primary);
    cursor: pointer;
  }
</style>
