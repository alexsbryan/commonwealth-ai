<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
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
    /** Whether the "← Atlas" control leads anywhere. False when this
     *  view IS the surface root (a notebook's scoped Explore tab, where
     *  there is no corpus index to return to) — the host then hides the
     *  button rather than render a dead no-op. Defaults to true so the
     *  standalone Atlas Inspector keeps its back-to-index affordance. */
    showBack?: boolean;
    onSelectConv: (convUuid: string) => void;
  }

  let { corpusId, onBack, showBack = true, onSelectConv }: Props = $props();

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
    {#if showBack}
      <button class="back-button" type="button" onclick={onBack}>
        ← Atlas
      </button>
    {/if}
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
  /* Atlas conv-corpus view — Lavender Court palette.
   * All colour, border, and radius tokens come from app.css; no
   * hardcoded fallbacks (the prior version drifted with stale
   * --surface/--surface-2/--border-strong references that didn't
   * map onto any real token). State pills use the app's semantic
   * roles (growth = Ready, accent = in-flight, coral = pending,
   * error = failed). Entity chips wear the lavender wash that
   * also marks the Conversations tile in AtlasIndex. */
  .conv-corpus-view {
    display: flex;
    flex-direction: column;
    gap: 18px;
    padding: 28px 32px 44px;
    max-width: 64rem;
    margin: 0 auto;
    font-family: var(--font-sans);
    color: var(--text-primary);
  }
  .view-header {
    display: flex;
    align-items: flex-start;
    gap: 16px;
  }
  .back-button {
    background: transparent;
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    padding: 6px 12px;
    cursor: pointer;
    color: var(--text-secondary);
    font-size: 0.82rem;
    font-family: inherit;
    letter-spacing: 0.01em;
    transition: border-color 120ms ease, color 120ms ease, background 120ms ease;
  }
  .back-button:hover {
    background: var(--bg-elevated);
    border-color: var(--border-bright);
    color: var(--text-primary);
  }
  .header-text h1 {
    margin: 0;
    font-size: 1.5rem;
    font-weight: 600;
    line-height: 1.2;
    letter-spacing: -0.01em;
    color: var(--text-primary);
  }
  .subtitle {
    margin: 4px 0 0;
    color: var(--text-muted);
    font-size: 0.82rem;
  }
  .search-row input {
    width: 100%;
    padding: 9px 14px;
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    background: var(--bg-input);
    color: var(--text-primary);
    font-size: 0.92rem;
    font-family: inherit;
    transition: border-color 120ms ease, background 120ms ease;
  }
  .search-row input::placeholder {
    color: var(--text-muted);
  }
  .search-row input:focus {
    outline: none;
    border-color: var(--accent);
    background: var(--bg-surface);
  }
  .status {
    padding: 18px 4px;
    color: var(--text-secondary);
    font-size: 0.92rem;
  }
  .status.error {
    color: var(--error);
  }
  .status.empty {
    text-align: center;
    padding: 32px 4px;
  }
  .status .hint {
    margin-top: 8px;
    font-size: 0.82rem;
    color: var(--text-muted);
  }
  .status .hint code {
    font-family: var(--font-mono);
    font-size: 0.78rem;
    padding: 1px 5px;
    background: var(--bg-elevated);
    border-radius: 3px;
    color: var(--text-secondary);
  }
  .conv-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .conv-row {
    display: contents;
  }
  .conv-button {
    width: 100%;
    text-align: left;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 12px 16px;
    cursor: pointer;
    color: var(--text-primary);
    font-family: inherit;
    display: flex;
    flex-direction: column;
    gap: 6px;
    transition: border-color 140ms ease, background 140ms ease;
  }
  .conv-button:hover {
    border-color: var(--border-bright);
    background: var(--bg-elevated);
  }
  .conv-button:focus-visible {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent-dim);
  }
  .conv-header {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 12px;
  }
  .conv-title {
    font-weight: 600;
    font-size: 0.95rem;
    line-height: 1.35;
    flex: 1;
    color: var(--text-primary);
  }
  /* State pills — each carries its own semantic role token.
   * Tinted background + matching foreground; no hardcoded RGBA. */
  .state-pill {
    font-size: 0.66rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    padding: 3px 8px;
    border-radius: 999px;
    background: var(--bg-elevated);
    color: var(--text-muted);
    white-space: nowrap;
    font-weight: 500;
  }
  .state-ready {
    background: var(--growth-dim);
    color: var(--growth);
  }
  .state-multihopready {
    background: var(--accent-dim);
    color: var(--accent-light);
  }
  .state-partiallyready {
    background: var(--lavender-dim);
    color: var(--lavender-light);
  }
  .state-pending {
    background: var(--bg-elevated);
    color: var(--text-muted);
  }
  .state-failed {
    background: var(--coral-dim);
    color: var(--coral);
  }
  .conv-meta {
    display: flex;
    gap: 14px;
    font-size: 0.76rem;
    color: var(--text-muted);
    align-items: center;
  }
  .tiny-pill {
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    padding: 2px 7px;
    border-radius: 999px;
    background: var(--bg-elevated);
    color: var(--text-muted);
    font-weight: 500;
  }
  .entity-row {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    margin-top: 4px;
  }
  /* Entity chip — lavender wash. Same colour family as the
   * Conversations tile on AtlasIndex, so the chips read as part
   * of the same world rather than imported blue from another app. */
  .entity-chip {
    background: var(--lavender-dim);
    color: var(--lavender-light);
    border-radius: var(--radius);
    padding: 2px 9px;
    font-size: 0.74rem;
    border: 1px solid transparent;
    font-family: inherit;
    cursor: pointer;
    transition: background 120ms ease, border-color 120ms ease;
  }
  .entity-chip:hover {
    background: var(--lavender-glow);
    border-color: var(--lavender);
    color: var(--text-primary);
  }
  .entity-chip:focus-visible {
    outline: 2px solid var(--lavender);
    outline-offset: 1px;
  }
  .load-more {
    display: flex;
    justify-content: center;
    padding: 12px;
  }
  .load-more-button {
    background: transparent;
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    padding: 7px 18px;
    cursor: pointer;
    color: var(--text-secondary);
    font-family: inherit;
    font-size: 0.85rem;
    transition: border-color 120ms ease, color 120ms ease, background 120ms ease;
  }
  .load-more-button:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent-light);
    background: var(--accent-glow);
  }
  .load-more-button:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
</style>
