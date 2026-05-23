<script lang="ts">
  // Atlas Inspector — per-conv-corpus browse view.
  //
  // Spec: sovereign/docs/specs/CONV_TIERED_PORT.md §"A1 conv corpora
  // in Atlas index".
  //
  // Parallel to AtlasCorpusView.svelte but reads from the SQLite-
  // backed conv_skeletons table instead of atoms.json. Renders one
  // card per conversation with its title, state, chunk count, and
  // top entities. Clicking opens ConvDetail.svelte for the full
  // RAPTOR tree.

  import { onMount } from "svelte";
  import { atlasListConversations } from "../../api";
  import type { ConvListPage, ConvSummary } from "../../types";
  import EntityDrawer from "./EntityDrawer.svelte";

  interface Props {
    corpusId: string;
    onBack: () => void;
    onSelectConv: (convUuid: string) => void;
  }

  let { corpusId, onBack, onSelectConv }: Props = $props();

  /** Seed entity for the drawer; `null` = drawer closed. Click an
   *  `entity-chip` to open. */
  let drawerSeed: string | null = $state(null);

  function openDrawer(name: string) {
    drawerSeed = name;
  }

  function closeDrawer() {
    drawerSeed = null;
  }

  function handleDrawerSelectConv(convUuid: string) {
    closeDrawer();
    onSelectConv(convUuid);
  }

  let page: ConvListPage | null = $state(null);
  let loading = $state(true);
  let error: string | null = $state(null);
  let nameQuery = $state("");
  /** Debounce timer for the search input; mirrors the existing
   *  AtlasCorpusView's pattern. */
  let debounceTimer: ReturnType<typeof setTimeout> | null = null;

  async function loadPage(offset?: number) {
    loading = true;
    try {
      const filter = nameQuery.trim() === "" ? undefined : nameQuery.trim();
      const fresh = await atlasListConversations(corpusId, filter, offset);
      if (offset && offset > 0 && page) {
        page = {
          conversations: [...page.conversations, ...fresh.conversations],
          total_matching: fresh.total_matching,
          next_offset: fresh.next_offset,
        };
      } else {
        page = fresh;
      }
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void loadPage(0);
  });

  function onSearchInput(e: Event) {
    nameQuery = (e.target as HTMLInputElement).value;
    if (debounceTimer !== null) {
      clearTimeout(debounceTimer);
    }
    debounceTimer = setTimeout(() => {
      void loadPage(0);
    }, 220);
  }

  function loadMore() {
    if (page?.next_offset !== undefined && page.next_offset !== null) {
      void loadPage(page.next_offset);
    }
  }

  function formatTimestamp(unix: number): string {
    if (!unix) return "";
    return new Date(unix * 1000).toLocaleString();
  }

  // State-class names drive the per-state pill colour in CSS.
  function stateClass(state: string): string {
    return `state-pill state-${state.toLowerCase()}`;
  }

  /** Plain-language labels for the per-conv enrichment states. */
  const STATE_LABEL: Record<string, string> = {
    Ready: "Ready",
    MultiHopReady: "Partly ready",
    PartiallyReady: "Indexing…",
    Pending: "Waiting",
    Failed: "Failed",
  };

  function stateLabel(state: string): string {
    return STATE_LABEL[state] ?? state;
  }
</script>

<div class="conv-corpus-view">
  <header class="view-header">
    <button class="back-button" type="button" onclick={onBack}>
      ← Atlas
    </button>
    <div class="header-text">
      <h1>{corpusId}</h1>
      {#if page}
        <p class="subtitle">
          {page.total_matching.toLocaleString()} conversation{page.total_matching === 1 ? "" : "s"}
          {#if nameQuery.trim() !== ""}
            matching "{nameQuery.trim()}"
          {/if}
        </p>
      {/if}
    </div>
  </header>

  <div class="search-row">
    <input
      type="search"
      placeholder="Search conversations by title…"
      value={nameQuery}
      oninput={onSearchInput}
      aria-label="Filter conversations by title"
    />
  </div>

  {#if error}
    <div class="status error" role="alert">Failed to load: {error}</div>
  {/if}

  {#if loading && !page}
    <div class="status">Loading conversations…</div>
  {:else if page && page.conversations.length === 0}
    <div class="status empty">
      <p>No conversations match.</p>
      {#if nameQuery.trim() !== ""}
        <p class="hint">Try a shorter search term, or clear the filter.</p>
      {:else}
        <p class="hint">
          Tiered enrichment hasn't run yet for this corpus. Install /
          re-install the conv recipe with <code>[enrichment] type = "tiered"</code> to populate.
        </p>
      {/if}
    </div>
  {:else if page}
    <ul class="conv-list">
      {#each page.conversations as conv (conv.conv_uuid)}
        <li class="conv-row" data-testid="atlas-conv-row">
          <button
            class="conv-button"
            type="button"
            onclick={() => onSelectConv(conv.conv_uuid)}
            aria-label={`Open conversation: ${conv.title}`}
          >
            <div class="conv-header">
              <span class="conv-title">{conv.title}</span>
              <span
                class={stateClass(conv.state)}
                title={conv.state}
              >{stateLabel(conv.state)}</span>
            </div>
            <div class="conv-meta">
              <span class="chunks">{conv.chunk_count.toLocaleString()} chunks</span>
              <span class="updated">{formatTimestamp(conv.updated_at)}</span>
              {#if conv.is_tiny}
                <span class="tiny-pill" title="Short conversation — no detailed cluster summaries">
                  short
                </span>
              {/if}
            </div>
          </button>
          <!-- Chips render as siblings (not children) of the conv
               button so each can be its own <button> — HTML disallows
               nested interactive elements. -->
          {#if conv.top_entities.length > 0}
            <div class="entity-row">
              {#each conv.top_entities as ent (ent)}
                <button
                  type="button"
                  class="entity-chip"
                  onclick={() => openDrawer(ent)}
                  title={`See where "${ent}" appears across this corpus`}
                >{ent}</button>
              {/each}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
    {#if page.next_offset !== undefined && page.next_offset !== null}
      <div class="load-more">
        <button type="button" class="load-more-button" onclick={loadMore} disabled={loading}>
          {loading ? "Loading…" : "Load more"}
        </button>
      </div>
    {/if}
  {/if}
</div>

{#if drawerSeed !== null}
  <EntityDrawer
    {corpusId}
    seed={drawerSeed}
    onClose={closeDrawer}
    onSelectConv={handleDrawerSelectConv}
  />
{/if}

<style>
  .conv-corpus-view {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    padding: 1.5rem 2rem;
    max-width: 60rem;
    margin: 0 auto;
  }
  .view-header {
    display: flex;
    align-items: flex-start;
    gap: 1rem;
  }
  .back-button {
    background: transparent;
    border: 1px solid var(--border, #444);
    border-radius: 0.4rem;
    padding: 0.3rem 0.7rem;
    cursor: pointer;
    color: inherit;
    font-size: 0.85rem;
  }
  .back-button:hover {
    background: var(--surface-2, #2a2a2a);
  }
  .header-text h1 {
    margin: 0;
    font-size: 1.4rem;
  }
  .subtitle {
    margin: 0.2rem 0 0;
    color: var(--text-muted, #888);
    font-size: 0.85rem;
  }
  .search-row input {
    width: 100%;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--border, #444);
    border-radius: 0.4rem;
    background: var(--surface, #1a1a1a);
    color: inherit;
    font-size: 0.95rem;
  }
  .status {
    padding: 1rem;
    color: var(--text-muted, #888);
  }
  .status.error {
    color: var(--error, #d44);
  }
  .status.empty {
    text-align: center;
  }
  .status .hint {
    margin-top: 0.6rem;
    font-size: 0.85rem;
  }
  .conv-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  .conv-row {
    display: contents;
  }
  .conv-button {
    width: 100%;
    text-align: left;
    background: var(--surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 0.5rem;
    padding: 0.8rem 1rem;
    cursor: pointer;
    color: inherit;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .conv-button:hover {
    border-color: var(--border-strong, #555);
    background: var(--surface-2, #222);
  }
  .conv-header {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 0.8rem;
  }
  .conv-title {
    font-weight: 600;
    font-size: 0.98rem;
    line-height: 1.3;
    flex: 1;
  }
  .state-pill {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 0.15rem 0.5rem;
    border-radius: 0.7rem;
    background: var(--surface-2, #333);
    color: var(--text-muted, #aaa);
    white-space: nowrap;
  }
  .state-ready {
    background: rgba(46, 160, 67, 0.18);
    color: #4ec06b;
  }
  .state-multihopready {
    background: rgba(212, 167, 44, 0.18);
    color: #d4a72c;
  }
  .state-partiallyready {
    background: rgba(212, 167, 44, 0.12);
    color: #c39530;
  }
  .state-pending {
    background: rgba(150, 150, 150, 0.18);
    color: #999;
  }
  .state-failed {
    background: rgba(216, 76, 76, 0.18);
    color: #e25555;
  }
  .conv-meta {
    display: flex;
    gap: 0.8rem;
    font-size: 0.78rem;
    color: var(--text-muted, #888);
    align-items: center;
  }
  .tiny-pill {
    font-size: 0.65rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 0.1rem 0.4rem;
    border-radius: 0.6rem;
    background: rgba(150, 150, 150, 0.14);
    color: #999;
  }
  .entity-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
    margin-top: 0.2rem;
  }
  .entity-chip {
    background: rgba(96, 132, 232, 0.16);
    color: #92ade8;
    border-radius: 0.5rem;
    padding: 0.1rem 0.55rem;
    font-size: 0.75rem;
    border: 1px solid transparent;
    font-family: inherit;
    cursor: pointer;
  }
  .entity-chip:hover {
    background: rgba(96, 132, 232, 0.28);
    border-color: rgba(96, 132, 232, 0.55);
  }
  .entity-chip:focus-visible {
    outline: 2px solid rgba(96, 132, 232, 0.7);
    outline-offset: 1px;
  }
  .load-more {
    display: flex;
    justify-content: center;
    padding: 0.8rem;
  }
  .load-more-button {
    background: transparent;
    border: 1px solid var(--border, #444);
    border-radius: 0.4rem;
    padding: 0.4rem 1rem;
    cursor: pointer;
    color: inherit;
  }
</style>
