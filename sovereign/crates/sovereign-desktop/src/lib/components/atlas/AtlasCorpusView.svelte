<script lang="ts">
  // Atlas Inspector — per-corpus browse view.
  //
  // Type-filtered, name-searchable, paginated atom list for one
  // corpus. The first call deserializes atoms.json once on the
  // backend; every subsequent filter/search keystroke is served from
  // the in-process cache (see sovereign-tools::atlas_view::atom_browse).
  //
  // Step 4 will wire row clicks to an atom-detail surface. For now
  // rows render as buttons that no-op (with a placeholder hint), so
  // the interaction shape is already in place.

  import { onMount, untrack } from "svelte";
  import { atlasListAtoms } from "../../api";
  import type {
    AtomFilter,
    AtomListPage,
    AtomSummary,
    AtomType,
  } from "../../types";

  interface Props {
    corpusId: string;
    /** Total atom count for the corpus, passed in by the parent so
     *  we can render the "X of Y" label without an extra round-trip
     *  while atoms load. Optional — falls back to the live response. */
    totalAtomsHint?: number;
    /** Per-type counts from the picker. Drives the tab badges. Same
     *  shape as `AtlasCorpusSummary.atom_counts`. */
    atomCountsHint?: Partial<Record<AtomType, number>>;
    onBack: () => void;
    /** Drill into a single atom's detail view. */
    onSelectAtom?: (atomId: string) => void;
  }

  let { corpusId, totalAtomsHint, atomCountsHint, onBack, onSelectAtom }: Props =
    $props();

  const ATOM_TYPE_ORDER: readonly AtomType[] = [
    "Entity",
    "Event",
    "State",
    "Relation",
    "Claim",
    "Question",
    "Configuration",
    "ArgumentReconstruction",
  ] as const;

  const ATOM_TYPE_LABEL: Record<AtomType, string> = {
    Entity: "Entity",
    Event: "Event",
    State: "State",
    Relation: "Relation",
    Claim: "Claim",
    Question: "Question",
    Configuration: "Config",
    ArgumentReconstruction: "Argument",
  };

  const PAGE_LIMIT = 200;

  // ─── State ────────────────────────────────────────────────
  let activeType: AtomType | "all" = $state("all");
  let nameQuery = $state("");
  /** Debounced version of nameQuery actually sent to the backend.
   *  Each keystroke resets the timer; after 200ms of inactivity the
   *  debounced value catches up and the $effect re-fetches. */
  let debouncedQuery = $state("");
  let debounceTimer: ReturnType<typeof setTimeout> | null = null;

  let items: AtomSummary[] = $state([]);
  let totalMatching = $state(0);
  let nextOffset: number | undefined = $state(undefined);
  let loading = $state(true);
  let loadingMore = $state(false);
  let error: string | null = $state(null);

  // ─── Effects ──────────────────────────────────────────────
  $effect(() => {
    // Re-fire the initial fetch whenever the active filter changes.
    // Reads `activeType` + `debouncedQuery`; we untrack the actual
    // fetch + state writes so the effect graph stays linear.
    const _type = activeType;
    const _query = debouncedQuery;
    untrack(() => {
      void initialFetch();
    });
  });

  $effect(() => {
    // Debounce nameQuery → debouncedQuery. Reset the timer on every
    // keystroke; after 200ms of quiet, the debounced value updates
    // and the fetch effect above re-fires.
    const q = nameQuery;
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      debouncedQuery = q;
    }, 200);
    return () => {
      if (debounceTimer) clearTimeout(debounceTimer);
    };
  });

  async function initialFetch() {
    loading = true;
    error = null;
    try {
      const page = await atlasListAtoms(
        corpusId,
        buildFilter(),
        { offset: 0, limit: PAGE_LIMIT },
      );
      applyPage(page, /* replace */ true);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  async function loadMore() {
    if (nextOffset === undefined || loadingMore) return;
    loadingMore = true;
    try {
      const page = await atlasListAtoms(
        corpusId,
        buildFilter(),
        { offset: nextOffset, limit: PAGE_LIMIT },
      );
      applyPage(page, /* replace */ false);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loadingMore = false;
    }
  }

  function buildFilter(): AtomFilter {
    const f: AtomFilter = {};
    if (activeType !== "all") f.atom_type = activeType;
    if (debouncedQuery.trim()) f.name_query = debouncedQuery.trim();
    return f;
  }

  function applyPage(page: AtomListPage, replace: boolean) {
    if (replace) {
      items = page.items;
    } else {
      items = [...items, ...page.items];
    }
    totalMatching = page.total_matching;
    nextOffset = page.next_offset;
  }

  function countForType(t: AtomType | "all"): number | undefined {
    if (!atomCountsHint) return undefined;
    if (t === "all") return totalAtomsHint;
    return atomCountsHint[t];
  }

  function formatSalience(s: number | undefined): string {
    if (s === undefined) return "";
    return s.toFixed(2);
  }
</script>

<div class="atlas-corpus-view">
  <header class="corpus-header">
    <button class="back-btn" type="button" onclick={onBack} aria-label="Back to atlas index">
      <!-- Lucide: arrow-left -->
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m12 19-7-7 7-7"/>
        <path d="M19 12H5"/>
      </svg>
      <span>Atlas</span>
    </button>
    <h1 class="corpus-title">{corpusId}</h1>
    {#if totalAtomsHint !== undefined}
      <span class="total-hint">{totalAtomsHint.toLocaleString()} atoms</span>
    {/if}
  </header>

  <nav class="type-tabs" aria-label="Filter by atom type">
    <button
      class="tab"
      class:active={activeType === "all"}
      type="button"
      onclick={() => (activeType = "all")}
    >
      All
      {#if countForType("all") !== undefined}
        <span class="badge">{countForType("all")?.toLocaleString()}</span>
      {/if}
    </button>
    {#each ATOM_TYPE_ORDER as t (t)}
      {@const count = countForType(t)}
      <button
        class="tab"
        class:active={activeType === t}
        type="button"
        disabled={count === 0}
        onclick={() => (activeType = t)}
      >
        {ATOM_TYPE_LABEL[t]}
        {#if count !== undefined}
          <span class="badge">{count.toLocaleString()}</span>
        {/if}
      </button>
    {/each}
  </nav>

  <div class="search-row">
    <input
      type="search"
      class="search-input"
      placeholder="Search by name…"
      bind:value={nameQuery}
      aria-label="Filter atoms by name"
    />
    <div class="result-count" aria-live="polite">
      {#if loading}
        Loading…
      {:else}
        Showing {items.length.toLocaleString()} of {totalMatching.toLocaleString()}
      {/if}
    </div>
  </div>

  {#if error}
    <div class="status error" role="alert">
      Failed to load atoms: {error}
    </div>
  {:else if loading && items.length === 0}
    <div class="status">Loading atoms…</div>
  {:else if items.length === 0}
    <div class="status empty">
      No atoms match the current filter.
    </div>
  {:else}
    <ul class="atom-list">
      {#each items as a (a.atom_id)}
        <li class="atom-row" data-testid="atlas-atom-row">
          <button
            class="atom-button"
            type="button"
            disabled={!onSelectAtom}
            onclick={() => onSelectAtom?.(a.atom_id)}
            aria-label={`Inspect ${a.display_name}`}
          >
            <div class="atom-header">
              <span class="type-pill" data-type={a.atom_type}>
                {ATOM_TYPE_LABEL[a.atom_type]}
              </span>
              <span class="display-name">{a.display_name}</span>
            </div>
            <div class="atom-meta">
              {#if a.salience !== undefined}
                <span class="meta-chip" title="Salience">
                  ◆ {formatSalience(a.salience)}
                </span>
              {/if}
              {#if a.evidence_chunk_count > 0}
                <span class="meta-chip" title="Evidence chunks">
                  ▤ {a.evidence_chunk_count}
                </span>
              {/if}
              <span class="meta-chip depth" title="Enrichment depth">
                {a.enrichment_depth}
              </span>
            </div>
          </button>
        </li>
      {/each}
    </ul>

    {#if nextOffset !== undefined}
      <div class="load-more-row">
        <button
          class="load-more"
          type="button"
          disabled={loadingMore}
          onclick={loadMore}
        >
          {loadingMore ? "Loading…" : "Load more"}
        </button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .atlas-corpus-view {
    max-width: 920px;
    margin: 0 auto;
    padding: 32px 32px 80px;
    color: var(--text-primary);
    font-family: var(--font-sans);
  }

  .corpus-header {
    display: flex;
    align-items: center;
    gap: 16px;
    margin-bottom: 20px;
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
    transition: background 150ms ease, border-color 150ms ease;
  }

  .back-btn:hover {
    background: var(--bg-secondary);
    border-color: var(--border-mid);
    color: var(--text-primary);
  }

  .corpus-title {
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

  .type-tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-bottom: 16px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
  }

  .tab {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 11px;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius);
    color: var(--text-secondary);
    font: inherit;
    font-size: 0.82rem;
    cursor: pointer;
    transition: background 150ms ease, color 150ms ease;
  }

  .tab:hover:not(:disabled) {
    background: var(--bg-secondary);
    color: var(--text-primary);
  }

  .tab.active {
    background: var(--bg-elevated, var(--bg-secondary));
    border-color: var(--border-mid);
    color: var(--text-primary);
  }

  .tab:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .badge {
    padding: 1px 6px;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 8px;
    font-size: 0.7rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  .tab.active .badge {
    background: var(--bg-primary);
    color: var(--text-secondary);
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

  .atom-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .atom-row {
    list-style: none;
  }

  .atom-button {
    width: 100%;
    padding: 12px 14px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    transition: border-color 150ms ease, background 150ms ease;
    display: block;
  }

  .atom-button:hover {
    border-color: var(--border-mid);
    background: var(--bg-elevated, var(--bg-secondary));
  }

  .atom-button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .atom-header {
    display: flex;
    align-items: baseline;
    gap: 10px;
    margin-bottom: 6px;
  }

  .type-pill {
    flex-shrink: 0;
    padding: 2px 8px;
    background: var(--bg-primary);
    border: 1px solid var(--border-mid, var(--border));
    border-radius: 10px;
    font-size: 0.7rem;
    color: var(--text-muted);
    letter-spacing: 0.02em;
  }

  .display-name {
    font-size: 0.92rem;
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .atom-meta {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }

  .meta-chip {
    font-size: 0.72rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  .meta-chip.depth {
    text-transform: lowercase;
    font-style: italic;
  }

  .load-more-row {
    display: flex;
    justify-content: center;
    margin-top: 16px;
  }

  .load-more {
    padding: 8px 18px;
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-secondary);
    font: inherit;
    font-size: 0.85rem;
    cursor: pointer;
  }

  .load-more:hover:not(:disabled) {
    background: var(--bg-secondary);
    color: var(--text-primary);
  }

  .load-more:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
