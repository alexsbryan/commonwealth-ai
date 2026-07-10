<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  LibraryView — the knowledge home (Phase 1 UX refactor, the biggest win).

  One shelf of every notebook you have, from every source, off the single
  `notebook_list` command. It owns three local routes:
    - the shelf (cards) — default,
    - the Add sheet (`+ Add`),
    - a notebook's detail (click a card / its Ask·Explore actions).

  Everything it composes already exists; LibraryView is the host that
  gives them one coherent home, replacing the Atlas rail and the
  scattered Settings → Knowledge / Imports surfaces.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { notebookList } from "../../api";
  import type { NotebookSummary, StarterQuestion } from "../../types";
  import NotebookKindIcon from "./NotebookKindIcon.svelte";
  import { kindLabel, kindTitle } from "./notebookKind";
  import AddSheet from "./AddSheet.svelte";
  import NotebookDetail from "./NotebookDetail.svelte";
  import { fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { cardSend, cardReceive, motionDur } from "../../motion";

  let {
    onOpenChatWithSeed,
    onDropToChat,
    onOpenWorkshop,
  }: {
    onOpenChatWithSeed?: (question: StarterQuestion) => void;
    onDropToChat?: () => void;
    onOpenWorkshop?: () => void;
  } = $props();

  let notebooks = $state<NotebookSummary[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Local routing within the Library surface.
  type DetailTab = "ask" | "explore" | "conflicts" | "sources" | "settings";
  let selected = $state<NotebookSummary | null>(null);
  let selectedTab = $state<DetailTab>("ask");
  let showAdd = $state(false);

  async function reload() {
    try {
      error = null;
      notebooks = await notebookList();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      notebooks = [];
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void reload();
  });

  function open(nb: NotebookSummary, tab: DetailTab) {
    selectedTab = tab;
    selected = nb;
  }

  function closeDetail() {
    selected = null;
    void reload();
  }

  function closeAdd() {
    showAdd = false;
    void reload();
  }

  function freshness(unix: number | null): string {
    if (!unix) return "";
    const days = Math.floor((Date.now() / 1000 - unix) / 86400);
    if (days <= 0) return "added today";
    if (days === 1) return "added yesterday";
    if (days < 30) return `added ${days}d ago`;
    if (days < 365) return `added ${Math.floor(days / 30)}mo ago`;
    return `added ${Math.floor(days / 365)}y ago`;
  }
</script>

{#if showAdd}
  <div class="sheet-host" transition:fly={{ y: 16, duration: motionDur(240), easing: cubicOut }}>
    <AddSheet onClose={closeAdd} {onOpenChatWithSeed} {onDropToChat} />
  </div>
{:else if selected}
  {#key selected.id}
    <NotebookDetail
      notebook={selected}
      initialTab={selectedTab}
      onBack={closeDetail}
      onChanged={reload}
      {onOpenWorkshop}
    />
  {/key}
{:else}
  <div class="library" data-testid="library-view">
    <header class="lib-header">
      <div class="lib-title">
        <h1>Library</h1>
        <p>Everything you can ask and explore.</p>
      </div>
      <button class="add-btn" onclick={() => (showAdd = true)} data-testid="library-add">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M5 12h14" /><path d="M12 5v14" />
        </svg>
        Add
      </button>
    </header>

    <div class="lib-body">
      {#if loading}
        <p class="muted">Loading your notebooks…</p>
      {:else if error}
        <p class="error">Couldn't load your notebooks: {error}</p>
      {:else if notebooks.length === 0}
        <div class="empty" data-testid="library-empty">
          <div class="empty-glyph" aria-hidden="true">
            <NotebookKindIcon kind="folder" size={34} />
          </div>
          <h2>No notebooks yet</h2>
          <p>
            A notebook is a body of knowledge you can ask questions of and
            explore — a folder of documents, an Obsidian vault, a chat export,
            or a ready-made library from the catalog.
          </p>
          <button class="primary" onclick={() => (showAdd = true)} data-testid="library-empty-add">
            Add your first notebook
          </button>
        </div>
      {:else}
        <div class="shelf" role="list">
          {#each notebooks as nb (nb.id)}
            <div
              class="card"
              role="listitem"
              data-testid="notebook-card"
              data-notebook-id={nb.id}
              in:cardReceive={{ key: nb.id }}
              out:cardSend={{ key: nb.id }}
            >
              <button class="card-open" onclick={() => open(nb, "ask")} title={`Ask ${nb.name}`}>
                <div class="card-top">
                  <span class="card-icon" title={kindTitle(nb.source_kind)}>
                    <NotebookKindIcon kind={nb.source_kind} size={18} />
                  </span>
                  {#if nb.explorable}
                    <span class="card-star" title="Has an explorable map">✦</span>
                  {/if}
                </div>
                <div class="card-name">{nb.name}</div>
                <div class="card-meta">
                  <span class="chip">{kindLabel(nb.source_kind)}</span>
                  <span class="dot">·</span>
                  <span>{nb.doc_count.toLocaleString()} passages</span>
                  {#if nb.open_conflicts != null && nb.open_conflicts > 0}
                    <span
                      class="chip chip-conflict"
                      title="Open conflicts to settle"
                    >{nb.open_conflicts} {nb.open_conflicts === 1 ? "conflict" : "conflicts"}</span>
                  {/if}
                </div>
                <div class="card-fresh">{freshness(nb.updated_unix)}</div>
              </button>
              <div class="card-actions">
                <button data-testid="notebook-ask" onclick={() => open(nb, "ask")}>Ask</button>
                <button data-testid="notebook-explore" onclick={() => open(nb, "explore")}>Explore</button>
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .sheet-host {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  .library {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
    background: var(--bg-primary);
  }
  .lib-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    padding: 22px 28px 16px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .lib-title h1 {
    font-size: 1.3rem;
    font-weight: 680;
    color: var(--text-primary);
    margin: 0;
  }
  .lib-title p {
    color: var(--text-muted);
    font-size: 0.85rem;
    margin: 3px 0 0;
  }
  .add-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font: inherit;
    font-weight: 600;
    font-size: 0.85rem;
    padding: 8px 15px;
    border-radius: var(--radius);
    border: 1px solid color-mix(in oklch, var(--accent) 40%, var(--border));
    background: color-mix(in oklch, var(--accent) 12%, var(--bg-elevated));
    color: var(--text-primary);
    cursor: pointer;
    flex-shrink: 0;
  }
  .add-btn:hover { background: color-mix(in oklch, var(--accent) 20%, var(--bg-elevated)); }

  .lib-body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 22px 28px 40px;
  }
  .muted { color: var(--text-muted); font-size: 0.88rem; }
  .error { color: var(--error); font-size: 0.88rem; }

  .shelf {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 14px;
  }
  .card {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: 12px;
    background: var(--bg-secondary);
    overflow: hidden;
    transition: border-color 160ms ease, transform 160ms ease;
  }
  .card:hover {
    border-color: color-mix(in oklch, var(--accent) 35%, var(--border));
    transform: translateY(-1px);
  }
  .card-open {
    text-align: left;
    font: inherit;
    background: none;
    border: none;
    cursor: pointer;
    padding: 14px 15px 12px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    color: inherit;
  }
  .card-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 4px;
  }
  .card-icon { color: var(--text-secondary); display: inline-flex; }
  .card-star { color: var(--accent); font-size: 0.9rem; }
  .card-name {
    font-weight: 620;
    font-size: 0.95rem;
    color: var(--text-primary);
    line-height: 1.3;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .card-meta {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 0.76rem;
    color: var(--text-muted);
    margin-top: 4px;
  }
  .chip {
    color: var(--text-secondary);
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 1px 6px;
    font-weight: 500;
  }
  .chip-conflict {
    color: color-mix(in oklch, var(--error) 80%, var(--text-primary));
    background: color-mix(in oklch, var(--error) 12%, transparent);
    border-color: color-mix(in oklch, var(--error) 35%, var(--border));
    font-weight: 600;
  }
  .dot { opacity: 0.6; }
  .card-fresh { font-size: 0.72rem; color: var(--text-muted); margin-top: 2px; }

  .card-actions {
    display: flex;
    border-top: 1px solid var(--border);
    margin-top: auto;
  }
  .card-actions button {
    flex: 1;
    font: inherit;
    font-size: 0.8rem;
    font-weight: 550;
    padding: 8px;
    background: none;
    border: none;
    color: var(--text-secondary);
    cursor: pointer;
  }
  .card-actions button:first-child { border-right: 1px solid var(--border); }
  .card-actions button:hover { background: var(--bg-elevated); color: var(--text-primary); }

  .empty {
    max-width: 460px;
    margin: 6vh auto 0;
    text-align: center;
    display: flex;
    flex-direction: column;
    align-items: center;
  }
  .empty-glyph { color: color-mix(in oklch, var(--accent) 60%, var(--text-muted)); margin-bottom: 12px; }
  .empty h2 { font-size: 1.1rem; font-weight: 640; color: var(--text-primary); margin: 0 0 8px; }
  .empty p { color: var(--text-secondary); font-size: 0.9rem; line-height: 1.6; margin: 0 0 20px; }

  button.primary {
    font: inherit;
    font-weight: 600;
    font-size: 0.88rem;
    padding: 10px 20px;
    border-radius: var(--radius);
    border: none;
    background: var(--accent);
    color: var(--accent-contrast, #fff);
    cursor: pointer;
  }
</style>
