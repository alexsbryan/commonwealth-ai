<script lang="ts">
  // Atlas Inspector — index route.
  //
  // Lists corpora that have an atlas on disk, with per-atom-type
  // counts. Clicking a row delegates to the parent's `onSelect`
  // callback (AtlasSurface), which switches the inner view to
  // `AtlasCorpusView` for that corpus.

  import { onMount, onDestroy } from "svelte";
  import {
    atlasGetChunkEntityProgress,
    atlasListConvCorpora,
    atlasListCorpora,
  } from "../../api";
  import type {
    AtlasCorpusSummary,
    AtomType,
    ChunkEntityProgressRow,
    ConvCorpusSummary,
  } from "../../types";

  // Distinguish the two row shapes the index renders. Atom corpora
  // (atoms.json-backed) show per-atom-type counts; conv corpora
  // (SQLite-backed tiered enrichment) show per-state counts. Both
  // live under the same `display_category` heading when the recipe
  // tags them the same way (e.g. "conversation").
  type IndexRow =
    | { kind: "atom"; data: AtlasCorpusSummary }
    | { kind: "conv"; data: ConvCorpusSummary };

  interface Props {
    /** Optional click handler. When set, rows become buttons that
     *  call this with the corpus id + kind. Kind = "atom" routes to
     *  the legacy AtlasCorpusView; "conv" routes to
     *  AtlasConvCorpusView (conv_skeletons / conv_raptor_nodes
     *  backed). When omitted, rows render read-only. */
    onSelect?: (corpusId: string, kind: "atom" | "conv") => void;
  }

  let { onSelect }: Props = $props();

  let summaries: AtlasCorpusSummary[] = $state([]);
  let convSummaries: ConvCorpusSummary[] = $state([]);
  /** Per-corpus extraction progress. Keyed by `corpus_id`. */
  let extractionProgress: Record<string, ChunkEntityProgressRow | null> =
    $state({});
  let loading = $state(true);
  let error: string | null = $state(null);
  /** Refresh interval id when any conv is still being enriched
   *  (not all Ready). Cleared when all conversations settle. */
  let convPollIntervalId: ReturnType<typeof setInterval> | null = null;
  /** Separate poll for chunk_entity_progress — runs while any
   *  corpus has `state = 'running'`. Clears when all extraction
   *  jobs settle. */
  let extractionPollIntervalId: ReturnType<typeof setInterval> | null = null;

  const ATOM_TYPE_ORDER: AtomType[] = [
    "Entity",
    "Event",
    "State",
    "Relation",
    "Claim",
    "Question",
    "Configuration",
    "ArgumentReconstruction",
  ];

  // Compact glyph per atom type — keeps the per-corpus row scannable
  // without burning horizontal space. Same labels used in Step 3 tabs.
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

  onMount(async () => {
    try {
      const [atoms, convs] = await Promise.all([
        atlasListCorpora(),
        atlasListConvCorpora(),
      ]);
      summaries = atoms;
      convSummaries = convs;
      maybeStartConvPoll();
      await refreshExtractionProgress();
      maybeStartExtractionPoll();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  });

  // Lifecycle: poll intervals are cleared in onDestroy rather than via
  // onMount's return value. Svelte's onMount typing rejects a
  // `Promise<() => void>` (the async + cleanup combo) — splitting into
  // onMount(async) + onDestroy keeps the polls cleanly cancelable
  // without fighting the type system.
  onDestroy(() => {
    if (convPollIntervalId !== null) {
      clearInterval(convPollIntervalId);
    }
    if (extractionPollIntervalId !== null) {
      clearInterval(extractionPollIntervalId);
    }
  });

  async function refreshExtractionProgress() {
    const updates: Record<string, ChunkEntityProgressRow | null> = {};
    for (const c of convSummaries) {
      try {
        updates[c.corpus_id] = await atlasGetChunkEntityProgress(c.corpus_id);
      } catch {
        updates[c.corpus_id] = null;
      }
    }
    extractionProgress = updates;
  }

  function anyExtractionRunning(): boolean {
    for (const p of Object.values(extractionProgress)) {
      // Poll while the snapshot Phase A is still running. The
      // Phase B "incremental" steady-state doesn't need 5s polling
      // — its updates only land on chat-history rebuilds, not
      // tick-by-tick — so we drop out of polling once Phase A
      // graduates.
      if (p && p.state === "running") return true;
    }
    return false;
  }

  function maybeStartExtractionPoll() {
    if (extractionPollIntervalId !== null) return;
    if (!anyExtractionRunning()) return;
    extractionPollIntervalId = setInterval(async () => {
      try {
        await refreshExtractionProgress();
        if (!anyExtractionRunning() && extractionPollIntervalId !== null) {
          clearInterval(extractionPollIntervalId);
          extractionPollIntervalId = null;
        }
      } catch {
        // Swallow — initial load already surfaced systemic errors.
      }
    }, 5000);
  }

  /** Render-side helper: 0-100 percent extracted or null if no
   *  extraction row exists for this corpus yet. */
  function extractionPct(corpusId: string): number | null {
    const p = extractionProgress[corpusId];
    if (!p || p.chunks_total === 0) return null;
    return Math.min(
      100,
      Math.round((p.chunks_processed / p.chunks_total) * 100),
    );
  }

  /** Convs still mid-enrichment? Per-state counts where any state
   *  other than "Ready" or "Failed" is non-zero. */
  function hasPendingConvs(rows: ConvCorpusSummary[]): boolean {
    for (const c of rows) {
      for (const [state, n] of Object.entries(c.state_counts ?? {})) {
        if ((n ?? 0) > 0 && state !== "Ready" && state !== "Failed") {
          return true;
        }
      }
    }
    return false;
  }

  function maybeStartConvPoll() {
    if (convPollIntervalId !== null) {
      return;
    }
    if (!hasPendingConvs(convSummaries)) {
      return;
    }
    // Glassbox: re-fetch every 5s while convs are still being enriched
    // so the user sees real-time progress without a manual refresh.
    convPollIntervalId = setInterval(async () => {
      try {
        const refreshed = await atlasListConvCorpora();
        convSummaries = refreshed;
        if (!hasPendingConvs(refreshed) && convPollIntervalId !== null) {
          clearInterval(convPollIntervalId);
          convPollIntervalId = null;
        }
      } catch {
        // Swallow poll errors; the initial load already set the
        // error state if there's a systemic problem.
      }
    }, 5000);
  }

  function formatTimestamp(unix: number | undefined): string {
    if (!unix) return "";
    const d = new Date(unix * 1000);
    return d.toLocaleString();
  }

  function nonZeroCounts(s: AtlasCorpusSummary): Array<[AtomType, number]> {
    return ATOM_TYPE_ORDER.flatMap<[AtomType, number]>((t) => {
      const n = s.atom_counts[t] ?? 0;
      return n > 0 ? [[t, n]] : [];
    });
  }

  // Human-friendly section heading for a `display_category` value.
  // Unknown categories fall back to a title-cased rendering so newly-
  // added categories (declared in a recipe `[display]` block) light up
  // without a frontend round-trip.
  const CATEGORY_TITLE: Record<string, string> = {
    conversation: "Conversations",
    reference: "Reference",
    argument: "Argument",
    personal: "Personal",
  };
  const OTHER_GROUP = "__other__";

  function categoryKey(s: AtlasCorpusSummary): string {
    return s.display_category ?? OTHER_GROUP;
  }

  function categoryTitle(key: string): string {
    if (key === OTHER_GROUP) return "Other";
    return (
      CATEGORY_TITLE[key] ??
      key.charAt(0).toUpperCase() + key.slice(1).replace(/[-_]/g, " ")
    );
  }

  // Stable order: known categories first in the order they appear in
  // CATEGORY_TITLE (conversation comes first since it's the
  // newest-feature surface), then any unknown categories alphabetically,
  // then Other last so legacy / untagged corpora don't crowd the top.
  const KNOWN_CATEGORY_ORDER = Object.keys(CATEGORY_TITLE);

  function convCategoryKey(s: ConvCorpusSummary): string {
    return s.display_category ?? OTHER_GROUP;
  }

  let grouped = $derived.by(() => {
    const buckets = new Map<string, IndexRow[]>();
    for (const s of summaries) {
      const key = categoryKey(s);
      const list = buckets.get(key) ?? [];
      list.push({ kind: "atom", data: s });
      buckets.set(key, list);
    }
    for (const s of convSummaries) {
      const key = convCategoryKey(s);
      const list = buckets.get(key) ?? [];
      list.push({ kind: "conv", data: s });
      buckets.set(key, list);
    }
    const ordered: Array<{ key: string; title: string; rows: IndexRow[] }> = [];
    for (const k of KNOWN_CATEGORY_ORDER) {
      const rows = buckets.get(k);
      if (rows && rows.length > 0) {
        ordered.push({ key: k, title: categoryTitle(k), rows });
        buckets.delete(k);
      }
    }
    const remainingKnown = Array.from(buckets.keys())
      .filter((k) => k !== OTHER_GROUP)
      .sort();
    for (const k of remainingKnown) {
      ordered.push({ key: k, title: categoryTitle(k), rows: buckets.get(k)! });
      buckets.delete(k);
    }
    const other = buckets.get(OTHER_GROUP);
    if (other && other.length > 0) {
      ordered.push({ key: OTHER_GROUP, title: categoryTitle(OTHER_GROUP), rows: other });
    }
    return ordered;
  });

  /** Per-state badge label + count for a conv corpus, suppressing
   *  zero counts so the row stays scannable. */
  /** Plain-language labels for the per-conv enrichment states.
   *  Backend reports `Ready / MultiHopReady / PartiallyReady /
   *  Pending / Failed`; users see "Ready" / "Partly ready" /
   *  "Indexing…" / "Waiting" / "Failed". Keeps the raw state name
   *  as the data attribute so CSS + tests still target it. */
  const CONV_STATE_LABEL: Record<string, string> = {
    Ready: "Ready",
    MultiHopReady: "Partly ready",
    PartiallyReady: "Indexing…",
    Pending: "Waiting",
    Failed: "Failed",
  };

  function convStateBadges(c: ConvCorpusSummary): Array<[string, number, string]> {
    const order = ["Ready", "MultiHopReady", "PartiallyReady", "Pending", "Failed"];
    return order.flatMap<[string, number, string]>((state) => {
      const n = c.state_counts[state] ?? 0;
      const label = CONV_STATE_LABEL[state] ?? state;
      return n > 0 ? [[state, n, label]] : [];
    });
  }

  function totalConvRows(): boolean {
    return summaries.length > 0 || convSummaries.length > 0;
  }
</script>

<div class="atlas-index">
  <header class="atlas-header">
    <h1>Atlas</h1>
    <p class="subtitle">
      Inspect the structured enrichments extracted from each corpus.
      Read-only today; curation is on the roadmap.
    </p>
  </header>

  {#if loading}
    <!-- Skeleton list: two stub category blocks of three rows each.
         Each placeholder mirrors the real row's height + structure
         (name lozenge + total · two count chips · meta line) so the
         page doesn't reflow when the data arrives. The shimmer
         keyframe runs on each placeholder via CSS — no JS work
         needed. Settles into the real grid in place rather than
         replacing a tiny "Loading…" line with a tall list. -->
    <div class="category-stack" aria-busy="true" aria-live="polite">
      {#each [0, 1] as i (i)}
        <section class="category-section">
          <div class="skeleton-line skeleton-heading"></div>
          <ul class="corpus-list">
            {#each [0, 1, 2] as j (j)}
              <li class="corpus-row">
                <div class="row-button row-button--skeleton">
                  <div class="row-header">
                    <span class="skeleton-line skeleton-title"></span>
                    <span class="skeleton-line skeleton-total"></span>
                  </div>
                  <div class="counts">
                    <span class="skeleton-line skeleton-chip"></span>
                    <span class="skeleton-line skeleton-chip skeleton-chip--narrow"></span>
                  </div>
                  <div class="skeleton-line skeleton-meta"></div>
                </div>
              </li>
            {/each}
          </ul>
        </section>
      {/each}
    </div>
  {:else if error}
    <div class="status error" role="alert">
      Failed to load atlases: {error}
    </div>
  {:else if !totalConvRows()}
    <div class="status empty">
      <p>No atlases on disk yet.</p>
      <p class="hint">
        Atlases are generated by the extraction pipeline (Tier-2
        enrichment). Install a corpus and run extraction to populate
        this surface.
      </p>
    </div>
  {:else}
    <div class="category-stack atlas-loaded">
      {#each grouped as group, gi (group.key)}
        <section
          class="category-section atlas-fade-in"
          data-testid="atlas-category-section"
          data-category={group.key}
          style="--atlas-fade-delay: {gi * 60}ms"
        >
          <h2 class="category-heading">{group.title}</h2>
          <ul class="corpus-list">
            {#each group.rows as row, ri (row.kind + ":" + row.data.corpus_id)}
              {#if row.kind === "atom"}
                <li
                  class="corpus-row atlas-fade-in"
                  data-testid="atlas-corpus-row"
                  style="--atlas-fade-delay: {(gi * 60) + (ri * 40) + 60}ms"
                >
                  <button
                    class="row-button"
                    type="button"
                    disabled={!onSelect}
                    onclick={() => onSelect?.(row.data.corpus_id, "atom")}
                    aria-label={`Open ${row.data.display_name}`}
                  >
                    <div class="row-header">
                      <span class="corpus-id">{row.data.display_name}</span>
                      <span class="total">{row.data.total_atoms.toLocaleString()} atoms</span>
                    </div>
                    <div class="counts">
                      {#each nonZeroCounts(row.data) as [t, n] (t)}
                        <span class="count-chip" title={t}>
                          <span class="count-label">{ATOM_TYPE_LABEL[t]}</span>
                          <span class="count-n">{n.toLocaleString()}</span>
                        </span>
                      {/each}
                    </div>
                    {#if row.data.last_extracted_unix}
                      <div class="meta">
                        Last extracted: {formatTimestamp(row.data.last_extracted_unix)}
                      </div>
                    {/if}
                  </button>
                </li>
              {:else}
                <li
                  class="corpus-row atlas-fade-in"
                  data-testid="atlas-corpus-row"
                  data-row-kind="conv"
                  style="--atlas-fade-delay: {(gi * 60) + (ri * 40) + 60}ms"
                >
                  <button
                    class="row-button"
                    type="button"
                    disabled={!onSelect}
                    onclick={() => onSelect?.(row.data.corpus_id, "conv")}
                    aria-label={`Open ${row.data.display_name}`}
                  >
                    <div class="row-header">
                      <span class="corpus-id">{row.data.display_name}</span>
                      <span class="total">
                        {row.data.conv_count.toLocaleString()} conversation{row.data.conv_count === 1 ? "" : "s"}
                      </span>
                    </div>
                    <div class="counts">
                      {#each convStateBadges(row.data) as [state, n, label] (state)}
                        <span
                          class="count-chip"
                          data-state={state.toLowerCase()}
                          title={`${label} (${state})`}
                        >
                          <span class="count-label">{label}</span>
                          <span class="count-n">{n.toLocaleString()}</span>
                        </span>
                      {/each}
                    </div>
                    {#if row.data.last_updated_unix}
                      <div class="meta">
                        Last updated: {formatTimestamp(row.data.last_updated_unix)}
                      </div>
                    {/if}
                    {#if extractionProgress[row.data.corpus_id]}
                      {@const prog = extractionProgress[row.data.corpus_id]!}
                      {@const pct = extractionPct(row.data.corpus_id)}
                      <div class="extraction-row">
                        {#if prog.state === "running"}
                          <span class="extraction-pill running">
                            Finding names &amp; topics · {pct}%
                            <span class="extraction-detail">
                              ({prog.chunks_processed.toLocaleString()} of {prog.chunks_total.toLocaleString()} messages read · {prog.mentions_extracted.toLocaleString()} highlights so far)
                            </span>
                          </span>
                        {:else if prog.state === "complete"}
                          <span class="extraction-pill complete">
                            ✓ Smart highlights ready
                            <span class="extraction-detail">
                              ({prog.mentions_extracted.toLocaleString()} names &amp; topics found across your chats)
                            </span>
                          </span>
                        {:else if prog.state === "incremental"}
                          <span class="extraction-pill incremental">
                            ✓ Smart highlights — auto-updating
                            <span class="extraction-detail">
                              ({prog.mentions_extracted.toLocaleString()} names &amp; topics · new chats analysed in the background)
                            </span>
                          </span>
                        {:else if prog.state === "failed"}
                          <span class="extraction-pill failed">
                            Couldn't analyse highlights — re-run from Settings
                          </span>
                        {/if}
                      </div>
                    {/if}
                  </button>
                </li>
              {/if}
            {/each}
          </ul>
        </section>
      {/each}
    </div>
  {/if}
</div>

<style>
  .atlas-index {
    max-width: 920px;
    margin: 0 auto;
    padding: 40px 32px 80px;
    color: var(--text-primary);
    font-family: var(--font-sans);
    /* Reserve enough vertical real estate that the page doesn't
       grow taller mid-load when categories arrive. Two stub categories
       × three skeleton rows is ~520px; this keeps the layout from
       jumping when the real data lands. */
    min-height: 640px;
  }

  .atlas-header {
    margin-bottom: 32px;
  }

  .atlas-header h1 {
    font-size: 1.6rem;
    font-weight: 600;
    margin: 0 0 6px;
    letter-spacing: -0.01em;
  }

  .subtitle {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.9rem;
    line-height: 1.5;
    max-width: 60ch;
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

  .status.empty .hint {
    margin-top: 8px;
    font-size: 0.85rem;
    color: var(--text-muted);
    max-width: 50ch;
    margin-left: auto;
    margin-right: auto;
    line-height: 1.5;
  }

  .category-stack {
    display: flex;
    flex-direction: column;
    gap: 28px;
  }

  /* `.category-section` has no own styling — the heading + nested
     `.corpus-list` carry the visual weight. Comment retained so a
     future maintainer doesn't add a background here without
     reading why we didn't. */

  .category-heading {
    margin: 0 0 12px;
    font-size: 0.78rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-muted);
  }

  .corpus-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .corpus-row {
    list-style: none;
  }

  .row-button {
    width: 100%;
    padding: 16px 18px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    transition: border-color 150ms ease, background 150ms ease;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    display: block;
  }

  .row-button:hover:not(:disabled) {
    border-color: var(--border-mid);
    background: var(--bg-elevated, var(--bg-secondary));
  }

  .row-button:disabled {
    cursor: default;
  }

  .row-button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .row-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin-bottom: 10px;
  }

  .corpus-id {
    font-weight: 600;
    font-size: 0.98rem;
  }

  .total {
    color: var(--text-muted);
    font-size: 0.82rem;
    font-variant-numeric: tabular-nums;
  }

  .counts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .count-chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 3px 9px;
    background: var(--bg-elevated, var(--bg-primary));
    border: 1px solid var(--border-mid, var(--border));
    border-radius: 10px;
    font-size: 0.76rem;
    color: var(--text-secondary);
  }

  .count-label {
    color: var(--text-muted);
  }

  .count-n {
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }

  .meta {
    margin-top: 10px;
    font-size: 0.74rem;
    color: var(--text-muted);
    letter-spacing: 0.01em;
  }

  .extraction-row {
    margin-top: 8px;
  }
  .extraction-pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 10px;
    border-radius: 10px;
    font-size: 0.74rem;
    font-variant-numeric: tabular-nums;
  }
  .extraction-pill.running {
    background: var(--lavender-dim);
    color: var(--lavender-light);
  }
  .extraction-pill.complete {
    background: var(--growth-dim);
    color: var(--growth);
  }
  .extraction-pill.incremental {
    background: var(--growth-glow);
    color: var(--growth);
    border: 1px dashed var(--growth);
  }
  .extraction-pill.failed {
    background: var(--coral-dim);
    color: var(--coral);
  }
  .extraction-detail {
    color: var(--text-muted);
    font-size: 0.7rem;
    font-weight: normal;
  }

  /* ── Skeleton placeholders ───────────────────────────────────────
     Shown while the initial `atlasListCorpora` + `atlasListConvCorpora`
     promises resolve. Mirrors the real row's shape (header bar +
     count chips + meta) so the eventual swap-in happens in place
     without reflow. The shimmer is purely cosmetic — the user reads
     it as "loading" without needing a spinner.
     ───────────────────────────────────────────────────────────── */
  .row-button--skeleton {
    cursor: default;
    pointer-events: none;
    background: var(--bg-secondary);
  }
  .skeleton-line {
    display: inline-block;
    height: 12px;
    border-radius: 6px;
    background: linear-gradient(
      90deg,
      var(--border) 0%,
      var(--border-mid) 50%,
      var(--border) 100%
    );
    background-size: 200% 100%;
    animation: atlas-shimmer 1.6s ease-in-out infinite;
    vertical-align: middle;
  }
  .skeleton-heading {
    height: 10px;
    width: 110px;
    margin: 0 0 12px;
    background: linear-gradient(
      90deg,
      var(--border) 0%,
      var(--border-mid) 50%,
      var(--border) 100%
    );
    background-size: 200% 100%;
  }
  .skeleton-title {
    height: 14px;
    width: 38%;
  }
  .skeleton-total {
    height: 11px;
    width: 18%;
  }
  .skeleton-chip {
    height: 18px;
    width: 80px;
    border-radius: 10px;
  }
  .skeleton-chip--narrow {
    width: 56px;
  }
  .skeleton-meta {
    height: 10px;
    width: 32%;
    margin-top: 10px;
  }
  @keyframes atlas-shimmer {
    0%   { background-position: 200% 50%; }
    100% { background-position: -100% 50%; }
  }

  /* ── Real-data fade-in ────────────────────────────────────────
     Lightweight staggered reveal once the initial promises resolve.
     Categories fade first (delay 0–60ms), then rows cascade
     (40ms each). Pure CSS — runs once on insertion, no JS hooks.
     The delay var is set inline per element so the cascade order
     reads naturally as you scroll the markup. */
  .atlas-fade-in {
    opacity: 0;
    transform: translateY(4px);
    animation: atlas-fade-in 320ms ease-out var(--atlas-fade-delay, 0ms) forwards;
  }
  @keyframes atlas-fade-in {
    to {
      opacity: 1;
      transform: none;
    }
  }
  /* Respect users who'd rather not see motion. */
  @media (prefers-reduced-motion: reduce) {
    .atlas-fade-in {
      animation: none;
      opacity: 1;
      transform: none;
    }
    .skeleton-line,
    .skeleton-heading {
      animation: none;
    }
  }
</style>
