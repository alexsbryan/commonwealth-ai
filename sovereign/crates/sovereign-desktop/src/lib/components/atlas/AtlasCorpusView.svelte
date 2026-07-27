<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
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

  import { onMount, untrack, tick } from "svelte";
  import { atlasListAtoms, atlasSubgraph } from "../../api";
  import AtlasGraph from "./AtlasGraph.svelte";
  import type {
    AtomFilter,
    AtomListPage,
    AtomSummary,
    AtomType,
    AtlasSubgraph,
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
    /** Whether the "Atlas" back control leads anywhere. False when this
     *  view IS the surface root (a notebook's scoped Explore tab, where
     *  there is no corpus index to return to) — the host hides the button
     *  rather than render a dead no-op. Defaults to true so the standalone
     *  Atlas Inspector keeps its back-to-index affordance. */
    showBack?: boolean;
    /** Where the back control leads, in the user's words. Defaults to
     *  the atlas index; a collection notebook passes "Articles",
     *  because back goes to that notebook's article picker, not out to
     *  the global index. */
    backLabel?: string;
    /** Drill into a single atom's detail view. */
    onSelectAtom?: (atomId: string) => void;
  }

  let {
    corpusId,
    totalAtomsHint,
    atomCountsHint,
    onBack,
    showBack = true,
    backLabel = "Atlas",
    onSelectAtom,
  }: Props = $props();

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

  // ─── Map view (epistemic landscape) ───────────────────────
  let viewMode = $state<"list" | "map">("list");
  let sg = $state<AtlasSubgraph | null>(null);
  let sgLoading = $state(false);
  let sgError = $state<string | null>(null);

  // Fetch the curated subgraph when the user enters Map mode (or switches
  // corpus while in it). The backend caches atoms.json, so re-entering is
  // cheap; reads only corpusId + viewMode so list keystrokes don't refetch.
  $effect(() => {
    const cid = corpusId;
    if (viewMode !== "map") return;
    sgLoading = true;
    sgError = null;
    atlasSubgraph(cid)
      .then((g) => {
        sg = g;
      })
      .catch((e) => {
        sgError = e instanceof Error ? e.message : String(e);
      })
      .finally(() => {
        sgLoading = false;
      });
  });
  let loadingMore = $state(false);
  let error: string | null = $state(null);

  /** Is anything narrowing the list right now? Gates the empty-state
   *  copy: with no filter applied, "no atoms" is a fact about the
   *  corpus, not about the user's query. */
  const filterActive = $derived(
    activeType !== "all" || debouncedQuery.trim().length > 0,
  );

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
      void measureRow();
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

  /** Short relative-time label for atoms whose source document was
   *  re-indexed after the bulk install (a newsworthy fetch, a
   *  watched-folder edit). The backend has already sorted these to the
   *  top; this renders the "fresh" marker beside the name. `null` /
   *  `undefined` updated_at → "" (baseline install-time content, no
   *  badge). */
  function freshLabel(updatedAt: number | null | undefined): string {
    if (updatedAt == null) return "";
    const secs = Math.max(0, Date.now() / 1000 - updatedAt);
    if (secs < 90) return "just now";
    const mins = Math.round(secs / 60);
    if (mins < 60) return `${mins}m ago`;
    const hrs = Math.round(mins / 60);
    if (hrs < 24) return `${hrs}h ago`;
    return `${Math.round(hrs / 24)}d ago`;
  }

  // ─── Windowing (virtual list) ─────────────────────────────
  // SEP and other large corpora list thousands of atoms; rendering
  // every <li> made the page so tall the scroll container couldn't
  // reach the bottom (and choked on the node count). We render only
  // the rows in (and just around) the viewport, inside a sizer that
  // reserves the full scroll height so the scrollbar stays honest.
  const OVERSCAN = 8; // rows kept rendered above/below the viewport
  const ROW_GAP = 8; // matches `.atom-row` margin-bottom (px)
  const EST_ROW = 64; // row-height estimate before the first measure

  let viewport: HTMLElement | undefined = $state();
  let scrollTop = $state(0);
  let viewportH = $state(0);
  // Center-to-center row height (row box + gap). Measured from the
  // first rendered row; rows are uniform (single-line ellipsized name
  // + ≤3 non-wrapping meta chips), so one measurement holds.
  let rowStride = $state(EST_ROW + ROW_GAP);

  let total = $derived(items.length);
  let startIndex = $derived(
    Math.max(0, Math.floor(scrollTop / rowStride) - OVERSCAN),
  );
  let windowCount = $derived(
    Math.ceil(viewportH / rowStride) + OVERSCAN * 2,
  );
  let endIndex = $derived(Math.min(total, startIndex + windowCount));
  let visible = $derived(items.slice(startIndex, endIndex));
  let topPad = $derived(startIndex * rowStride);
  let totalHeight = $derived(total * rowStride);

  function onScroll() {
    if (viewport) scrollTop = viewport.scrollTop;
    maybeLoadMore();
  }

  // Infinite scroll: prefetch the next page as the window nears the
  // end of what's loaded, so the list grows without a manual click.
  function maybeLoadMore() {
    if (nextOffset === undefined || loadingMore || !viewport) return;
    const remaining = totalHeight - (scrollTop + viewportH);
    if (remaining < rowStride * OVERSCAN) void loadMore();
  }

  // Measure the real row height once a fresh list has painted, and
  // reset the scroll to the top (a filter/search change starts over).
  async function measureRow() {
    scrollTop = 0;
    await tick();
    if (viewport) viewport.scrollTop = 0;
    const el = viewport?.querySelector<HTMLElement>(".atom-row");
    if (el) {
      const h = el.offsetHeight + ROW_GAP;
      if (h > ROW_GAP) rowStride = h;
    }
    // The first viewport may be taller than the first page — top up.
    maybeLoadMore();
  }
</script>

<div class="atlas-corpus-view">
  <header class="corpus-header">
    {#if showBack}
      <button class="back-btn" type="button" onclick={onBack} aria-label={`Back to ${backLabel.toLowerCase()}`}>
        <!-- Lucide: arrow-left -->
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="m12 19-7-7 7-7"/>
          <path d="M19 12H5"/>
        </svg>
        <span>{backLabel}</span>
      </button>
    {/if}
    <h1 class="corpus-title">{corpusId}</h1>
    {#if totalAtomsHint !== undefined}
      <span class="total-hint">{totalAtomsHint.toLocaleString()} atoms</span>
    {/if}
    <div class="view-toggle" role="group" aria-label="View mode">
      <button
        class="vt-btn"
        class:active={viewMode === "list"}
        type="button"
        onclick={() => (viewMode = "list")}>List</button
      >
      <button
        class="vt-btn"
        class:active={viewMode === "map"}
        type="button"
        onclick={() => (viewMode = "map")}>Map</button
      >
    </div>
  </header>

  {#if viewMode === "list"}
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

  <div
    class="atom-scroll"
    bind:this={viewport}
    bind:clientHeight={viewportH}
    onscroll={onScroll}
  >
    {#if error}
      <div class="status error" role="alert">
        Failed to load atoms: {error}
      </div>
    {:else if loading && items.length === 0}
      <div class="status">Loading atoms…</div>
    {:else if items.length === 0}
      <!-- "Nothing here" and "your filter hid it" call for different
           actions, so they must not share one line of copy. An empty
           result with no filter applied means this corpus's map was
           never built — that read as a filter problem for as long as
           the two shared a message. -->
      <div class="status empty" data-testid="atlas-atoms-empty">
        {#if filterActive}
          No atoms match the current filter.
        {:else}
          This notebook has no atoms yet — its map hasn't been built.
        {/if}
      </div>
    {:else}
      <!-- The sizer reserves the full scroll height for all `total`
           rows; the list is absolutely positioned and translated to the
           current window, so only the visible slice is in the DOM. -->
      <div class="atom-sizer" style="height: {totalHeight}px;">
        <ul class="atom-list" style="transform: translateY({topPad}px);">
          {#each visible as a (a.atom_id)}
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
                  {#if a.updated_at != null}
                    <span
                      class="fresh-badge"
                      data-testid="atlas-atom-fresh"
                      title={`Source refreshed ${freshLabel(a.updated_at)}`}
                    >
                      ● {freshLabel(a.updated_at)}
                    </span>
                  {/if}
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
      </div>

      {#if loadingMore}
        <div class="load-more-row" aria-live="polite">
          <span class="loading-hint">Loading more…</span>
        </div>
      {/if}
    {/if}
  </div>
  {:else}
    {#if sgError}
      <div class="status error" role="alert">Failed to build the map: {sgError}</div>
    {:else if sgLoading || !sg}
      <div class="status">Building the landscape…</div>
    {:else}
      <div class="map-census">
        {sg.census.atom_total.toLocaleString()} atoms · {sg.census.tensions} tensions
        · {sg.census.questions} questions · {sg.census.arguments} arguments{#if sg.census.shown < sg.census.atom_total}
          · showing top {sg.census.shown}{/if}
      </div>
      <AtlasGraph
        nodes={sg.nodes}
        edges={sg.edges}
        onNodeClick={(id) => onSelectAtom?.(id)}
      />
    {/if}
  {/if}
</div>

<style>
  .view-toggle {
    display: inline-flex;
    gap: 2px;
    margin-left: 8px;
    border: 1px solid var(--border-mid);
    border-radius: 100px;
    padding: 2px;
  }
  .vt-btn {
    padding: 2px 12px;
    border-radius: 100px;
    background: transparent;
    color: var(--text-muted);
    font-size: 0.72rem;
    cursor: pointer;
    border: none;
  }
  .vt-btn.active {
    background: var(--accent-dim);
    color: var(--accent);
  }
  .map-census {
    font-family: var(--font-mono);
    font-size: 0.68rem;
    color: var(--text-muted);
    padding: 8px 14px;
    letter-spacing: 0.02em;
    line-height: 1.5;
  }
  .atlas-corpus-view {
    max-width: 920px;
    width: 100%;
    margin: 0 auto;
    padding: 32px 32px 16px;
    color: var(--text-primary);
    font-family: var(--font-sans);
    box-sizing: border-box;
    /* Fill the .atlas-surface flex/scroll column and host an internal
       windowed scroller (`.atom-scroll`), so the atom list always
       reaches its bottom no matter how many rows it holds. */
    display: flex;
    flex-direction: column;
    flex: 1 1 auto;
    min-height: 0;
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

  .atom-scroll {
    /* The internal windowed scroll region — fills the height left by
       the header/tabs/search above it. */
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    scrollbar-gutter: stable;
  }

  .atom-sizer {
    /* Reserves the full height of all `total` rows so the scrollbar
       reflects the whole list; the `.atom-list` inside is absolutely
       positioned and translated to the current window. */
    position: relative;
    width: 100%;
  }

  .atom-list {
    list-style: none;
    padding: 0;
    margin: 0;
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    will-change: transform;
  }

  .atom-row {
    list-style: none;
    /* Inter-row spacing baked into the row box (was `.atom-list` gap)
       so each row's stride is self-contained for the windowing math. */
    margin-bottom: 8px;
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

  /* Atoms whose source doc was re-indexed after install — newsworthy
     fetches, watched-folder edits. The backend sorts these to the top;
     this badge says why a row leads the list. */
  .fresh-badge {
    flex-shrink: 0;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1px 8px;
    border-radius: 10px;
    font-size: 0.68rem;
    font-weight: 600;
    color: var(--accent-light, var(--accent));
    background: var(--accent-dim);
    letter-spacing: 0.01em;
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

  .loading-hint {
    color: var(--text-muted);
    font-size: 0.82rem;
  }
</style>
