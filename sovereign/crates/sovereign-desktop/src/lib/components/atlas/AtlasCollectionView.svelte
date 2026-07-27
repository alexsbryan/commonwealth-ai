<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Atlas Inspector — collection (article-picker) view.
  //
  // Some corpora are ingested as ONE index but enriched per article.
  // SEP is the case that forced this surface: 182k paragraphs live in
  // the `sep` index, but its map is 1,769 sibling `sep-<slug>`
  // atlases, one per encyclopedia entry — so `sep/atlas/atoms.json` is
  // a 44-byte `{"atoms":[]}` and the ordinary atom browser had nothing
  // to show ("No atoms match the current filter", with nothing to
  // match). See `sovereign-recipes/sep/recipe.toml` `[enrichment]`.
  //
  // Rather than union 1,769 atlases into one view — which would erase
  // the per-article boundary the philosophy_atlas pipeline is built
  // around — this view lists the articles and hands the chosen one to
  // the ordinary AtlasCorpusView. The accepted cost is that this
  // notebook's Explore interaction differs from every other corpus's:
  // one extra click, choosing WHICH map to open.
  //
  // The whole list is metadata (id + title + count), so filtering is
  // client-side: one backend call on mount, then keystrokes are free.

  import { onMount } from "svelte";
  import { atlasListMembers } from "../../api";
  import type { AtlasMemberSummary } from "../../types";

  interface Props {
    /** The collection corpus (`sep`), not the member. */
    corpusId: string;
    /** Open one member's atlas. Receives the MEMBER's corpus id. */
    onSelectMember: (memberCorpusId: string) => void;
    onBack: () => void;
    /** False when this view IS the surface root (a notebook's scoped
     *  Explore tab) — the host hides a back button that leads nowhere. */
    showBack?: boolean;
  }

  let { corpusId, onSelectMember, onBack, showBack = true }: Props = $props();

  let members: AtlasMemberSummary[] = $state([]);
  let loading = $state(true);
  let error: string | null = $state(null);
  let query = $state("");

  onMount(async () => {
    try {
      members = await atlasListMembers(corpusId);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  });

  /** Substring match over the human title AND the slug, because the
   *  title is slug-derived and imperfect — someone who knows the SEP
   *  slug (`logic-modal`) should find it even though the row reads
   *  "Logic Modal". */
  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return members;
    return members.filter(
      (m) =>
        m.title.toLowerCase().includes(q) ||
        m.corpus_id.toLowerCase().includes(q),
    );
  });

  const totalAtoms = $derived(
    members.reduce((sum, m) => sum + m.total_atoms, 0),
  );
</script>

<div class="atlas-collection-view">
  <header class="collection-header">
    {#if showBack}
      <button
        class="back-btn"
        type="button"
        onclick={onBack}
        aria-label="Back to atlas index"
      >
        <!-- Lucide: arrow-left -->
        <svg
          width="16"
          height="16"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.75"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="m12 19-7-7 7-7" />
          <path d="M19 12H5" />
        </svg>
        <span>Atlas</span>
      </button>
    {/if}
    <h1 class="collection-title">{corpusId}</h1>
    {#if !loading && members.length > 0}
      <span class="total-hint">
        {members.length.toLocaleString()} articles · {totalAtoms.toLocaleString()}
        atoms
      </span>
    {/if}
  </header>

  <p class="lede">
    This notebook's map is per article. Pick one to explore its atoms.
  </p>

  <div class="search-row">
    <input
      type="search"
      class="search-input"
      placeholder="Search articles…"
      bind:value={query}
      aria-label="Filter articles by title"
      data-testid="atlas-collection-search"
    />
    <div class="result-count" aria-live="polite">
      {#if loading}
        Loading…
      {:else}
        Showing {filtered.length.toLocaleString()} of {members.length.toLocaleString()}
      {/if}
    </div>
  </div>

  <div class="member-scroll">
    {#if error}
      <div class="status error" role="alert">
        Failed to load articles: {error}
      </div>
    {:else if loading}
      <div class="status">Loading articles…</div>
    {:else if members.length === 0}
      <!-- Distinguish "nothing here" from "your filter hid it" — the
           two need different actions from the user. -->
      <div class="status empty">
        No article maps have been built for this notebook yet.
      </div>
    {:else if filtered.length === 0}
      <div class="status empty">
        No article matches “{query}”.
      </div>
    {:else}
      <ul class="member-list">
        {#each filtered as m (m.corpus_id)}
          <li class="member-row" data-testid="atlas-member-row">
            <button
              class="member-button"
              type="button"
              onclick={() => onSelectMember(m.corpus_id)}
              aria-label={`Explore ${m.title}`}
            >
              <span class="member-title">{m.title}</span>
              <span class="member-count">
                {m.total_atoms.toLocaleString()} atoms
              </span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</div>

<style>
  .atlas-collection-view {
    max-width: var(--measure);
    width: 100%;
    margin: 0 auto;
    padding: var(--gutter-top) var(--gutter) var(--gutter-bottom);
    color: var(--text-primary);
    font-family: var(--font-sans);
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    flex: 1 1 auto;
    min-height: 0;
  }

  .collection-header {
    display: flex;
    align-items: center;
    gap: 16px;
    margin-bottom: 8px;
  }

  .back-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px 6px 8px;
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-secondary);
    font: inherit;
    font-size: 0.85rem;
    cursor: pointer;
    transition:
      background 150ms ease,
      border-color 150ms ease;
  }

  .back-btn:hover {
    background: var(--bg-secondary);
    border-color: var(--border-mid);
    color: var(--text-primary);
  }

  .collection-title {
    margin: 0;
    font-size: 1.4rem;
    font-weight: 600;
    letter-spacing: -0.01em;
    flex: 1;
  }

  .total-hint {
    color: var(--text-muted);
    font-size: 0.85rem;
    font-variant-numeric: tabular-nums;
  }

  .lede {
    margin: 0 0 16px;
    color: var(--text-secondary);
    font-size: 0.88rem;
  }

  .search-row {
    display: flex;
    gap: 12px;
    align-items: center;
    margin-bottom: 16px;
  }

  .search-input {
    flex: 1;
    padding: 8px 12px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font: inherit;
    font-size: 0.88rem;
  }

  .search-input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -1px;
    border-color: transparent;
  }

  .result-count {
    color: var(--text-muted);
    font-size: 0.78rem;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .status {
    padding: 32px;
    text-align: center;
    color: var(--text-muted);
    font-size: 0.9rem;
  }

  .status.error {
    color: var(--danger, #c33);
  }

  /* Floor, not 0 — same collapse hazard as AtlasCorpusView's
     `.atom-scroll`, and worse here: `.member-row` uses
     `content-visibility: auto`, so a collapsed viewport skips layout
     for essentially the whole member list. */
  .member-scroll {
    flex: 1 1 auto;
    min-height: 240px;
    overflow-y: auto;
    scrollbar-gutter: stable both-edges;
  }

  .member-list {
    list-style: none;
    padding: 0;
    margin: 0;
  }

  .member-row {
    /* Let the browser skip layout/paint for offscreen rows — a
       collection can be ~1,800 articles long. `contain-intrinsic-size`
       keeps the scrollbar honest while rows are skipped. */
    content-visibility: auto;
    contain-intrinsic-size: auto 44px;
  }

  .member-button {
    display: flex;
    align-items: baseline;
    gap: 12px;
    width: 100%;
    padding: 10px 12px;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius);
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    transition:
      background 120ms ease,
      border-color 120ms ease;
  }

  .member-button:hover {
    background: var(--bg-secondary);
    border-color: var(--border-mid);
  }

  .member-button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -1px;
  }

  .member-title {
    flex: 1;
    font-size: 0.92rem;
  }

  .member-count {
    color: var(--text-muted);
    font-size: 0.78rem;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
</style>
