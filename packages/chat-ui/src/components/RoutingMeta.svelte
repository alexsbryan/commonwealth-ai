<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  interface Props {
    provenance?: {
      intent: string;
      search_method?: string;
      sources?: {
        origin: string;
        count: number;
        /// Set when the originating corpus_id wasn't present
        /// locally and the hit was served via the mesh
        /// fan-out. Rendered as "origin (count) via <peer>" so
        /// same-corpus-two-ways ("sep" locally + "sep" from a
        /// peer) can't be confused for a single source.
        from_peer?: string;
        /// Folder-ingest v1 §6.3: when the corpus is a watched
        /// folder, the user-typed display name. Rendered in
        /// place of the opaque corpus_id slug.
        display_name?: string;
      }[];
      total_latency_ms: number;
      tokens_used: number;
      /// Completion tokens generated this turn (excludes the prompt).
      /// Present on streamed paths; pairs with `total_latency_ms`
      /// (which is synthesis-scoped, not whole-turn) to derive an
      /// honest generation tok/s. Absent → no rate is shown.
      completion_tokens?: number;
      inference_backend: string;
      oicp_match?: string;
      coarse_intent?: string;
      self_assessment?: string;
      /// Human-readable rationale for the coarse classification
      /// from the router (e.g. "current/time-sensitive signal →
      /// external tool", "factual-lookup shape (what/who/when/where)
      /// → knowledge query"). Surfaced beneath the Routing line so
      /// the operator can tell heuristic-shortcut from LLM-Pass-1
      /// from fallback paths without scraping daemon logs.
      routing_trigger?: string;
      /// Active chat-slot context window (sourced from
      /// `InferenceProvider::effective_context_size`). When set, the
      /// meta chip renders `tokens_used / context_window (X%)` and
      /// brightens as the cap approaches — see `.ctx-tight` /
      /// `.ctx-critical` below. `null`/absent on remote-only
      /// providers (no local slot).
      context_window?: number | null;
    };
    retrievedChunks?: Array<{
      title: string;
      corpus_id: string;
      url?: string;
      snippet: string;
    }>;
  }

  let { provenance, retrievedChunks = [] }: Props = $props();
  let expanded = $state(false);
  let sourcesExpanded = $state(false);

  // Prefer the user-typed folder display name when present; fall
  // back to the corpus_id slug for non-folder corpora (SEP, etc).
  function sourceLabel(s: {
    origin: string;
    display_name?: string;
  }): string {
    return s.display_name?.trim() || s.origin;
  }

  let corporaSearched = $derived(
    (provenance?.sources ?? [])
      .filter((s) => s.count > 0)
      .map((s) => sourceLabel(s)),
  );

  // "sep (6)" for local hits, "sep (6) via mac-peer" when the
  // mesh fan-out served this corpus. `from_peer` is stamped by
  // `prepare_knowledge_context` when the originating corpus_id
  // isn't present locally — so same-corpus-two-ways never lies
  // to the user about where a hit came from.
  let corporaDetail = $derived(
    (provenance?.sources ?? [])
      .filter((s) => s.count > 0)
      .map((s) => {
        const label = sourceLabel(s);
        return s.from_peer
          ? `${label} (${s.count}) via ${s.from_peer}`
          : `${label} (${s.count})`;
      }),
  );

  let elapsedLabel = $derived(
    provenance
      ? provenance.total_latency_ms < 1000
        ? `${provenance.total_latency_ms}ms`
        : `${(provenance.total_latency_ms / 1000).toFixed(1)}s`
      : "",
  );

  // Glassbox budget — `tokens_used / context_window (X%)`. The chip
  // changes color at 75% (yellow) and 90% (red) so long-running
  // marathon-style chats surface their pressure honestly before the
  // synth aborts. `tokensPct` is null when context_window is missing
  // (remote-only provider) so the chip falls back to the plain
  // "{tokens_used} tok" rendering.
  let tokensPct = $derived.by(() => {
    if (!provenance?.context_window) return null;
    if (provenance.tokens_used <= 0) return null;
    return Math.min(
      100,
      Math.round((provenance.tokens_used / provenance.context_window) * 100),
    );
  });
  let ctxBudgetClass = $derived.by(() => {
    if (tokensPct == null) return "";
    if (tokensPct >= 90) return "ctx-critical";
    if (tokensPct >= 75) return "ctx-tight";
    return "";
  });

  // Glassbox "answered-by" — the model that actually produced this
  // turn. `inference_backend` is the gguf stem for a local slot, or
  // "<model> @ peer <name>" when the mesh fan-out served it. We surface
  // it as an always-visible chip (not only in the expanded detail) so
  // the user can always tell which model answered without a click. This
  // is the *visibility* half of model agency — routing stays automatic;
  // we just stop hiding the result.
  let modelLabel = $derived(provenance?.inference_backend?.trim() ?? "");
  let modelIsPeer = $derived(modelLabel.includes("@ peer"));

  // Honest generation throughput. `total_latency_ms` is scoped to the
  // synthesis call (the embedded `complete()`/stream span), NOT the
  // whole turn — retrieval and routing complete before the timer starts
  // (in the runtime, `started` is set inside the synthesis spawn). So
  // `completion_tokens / synthesis_seconds` is a defensible tok/s — the
  // same number `ollama --verbose` reports as eval rate. We use
  // completion tokens only (never `tokens_used`, which can include the
  // prompt) and suppress the rate on sub-50ms spans where a few-token
  // turn would yield a meaningless number.
  let tokPerSec = $derived.by(() => {
    const toks = provenance?.completion_tokens;
    if (!toks || toks <= 0) return null;
    const secs = (provenance?.total_latency_ms ?? 0) / 1000;
    if (secs < 0.05) return null;
    const rate = toks / secs;
    return isFinite(rate) && rate > 0 ? rate : null;
  });
  let tokPerSecLabel = $derived(
    tokPerSec == null
      ? ""
      : `${tokPerSec >= 100 ? Math.round(tokPerSec) : tokPerSec.toFixed(1)} tok/s`,
  );
</script>

{#if provenance}
  <div
    class="routing-meta"
    role="button"
    tabindex="0"
    onclick={() => (expanded = !expanded)}
    onkeydown={(e) => e.key === "Enter" && (expanded = !expanded)}
  >
    {#if modelLabel}
      <span
        class="meta-chip meta-model"
        class:meta-peer={modelIsPeer}
        title={modelIsPeer
          ? `Answered by ${modelLabel}`
          : `Answered locally by ${modelLabel}`}>{modelLabel}</span
      >
    {/if}
    {#if corporaSearched.length > 0}
      <span class="meta-chip meta-source"
        >Searched {corporaSearched.join(", ")}</span
      >
    {/if}
    <span class="meta-chip">{elapsedLabel}</span>
    {#if provenance.tokens_used > 0}
      {#if tokensPct != null && provenance.context_window}
        <span class="meta-chip {ctxBudgetClass}">
          {provenance.tokens_used.toLocaleString()} / {provenance.context_window.toLocaleString()} tok ({tokensPct}%)
        </span>
      {:else}
        <span class="meta-chip">{provenance.tokens_used} tok</span>
      {/if}
    {/if}
    {#if tokPerSecLabel}
      <span
        class="meta-chip"
        title="Generation throughput — completion tokens ÷ synthesis time">{tokPerSecLabel}</span
      >
    {/if}
  </div>
  {#if expanded}
    <div class="routing-detail">
      <div>
        <strong>Routing:</strong>
        {#if provenance.coarse_intent}
          {provenance.coarse_intent}{provenance.self_assessment
            ? ` (${provenance.self_assessment})`
            : ""} &rarr; {provenance.intent}
        {:else}
          &rarr; {provenance.intent}
        {/if}
      </div>
      {#if provenance.routing_trigger}
        <div class="trigger-line">
          <strong>Why:</strong> {provenance.routing_trigger}
        </div>
      {/if}
      <div>
        <strong>Corpora:</strong>
        {#if corporaDetail.length > 0}
          {corporaDetail.join(", ")}
        {:else}
          &mdash;
        {/if}
      </div>
      {#if provenance.search_method}
        <div><strong>Search:</strong> {provenance.search_method}</div>
      {/if}
      {#if provenance.inference_backend}
        <div><strong>Backend:</strong> {provenance.inference_backend}</div>
      {/if}
      {#if provenance.oicp_match}
        <div><strong>OICP:</strong> {provenance.oicp_match}</div>
      {/if}
      <div>
        <strong>Timing:</strong>
        {elapsedLabel}{provenance.tokens_used > 0
          ? ` \u00B7 ${provenance.tokens_used} tok`
          : ""}{tokPerSecLabel ? ` \u00B7 ${tokPerSecLabel}` : ""}
      </div>
      {#if provenance.context_window}
        <div>
          <strong>Context budget:</strong>
          {provenance.tokens_used.toLocaleString()} / {provenance.context_window.toLocaleString()} tokens
          {#if tokensPct != null}
            ({tokensPct}% of the chat-slot window)
          {/if}
        </div>
      {/if}

      {#if retrievedChunks.length > 0}
        <div class="sources-section">
          <button
            class="sources-toggle"
            onclick={(e) => {
              e.stopPropagation()
              return sourcesExpanded = !sourcesExpanded
            }}
          >
            <strong>Retrieved passages ({retrievedChunks.length})</strong>
            <span class="toggle-arrow">{sourcesExpanded ? "\u25B4" : "\u25BE"}</span>
          </button>
          {#if sourcesExpanded}
            <div class="sources-list">
              {#each retrievedChunks as chunk, i}
                <div class="source-item">
                  <div class="source-header">
                    <span class="source-badge">{chunk.corpus_id}</span>
                    <span class="source-title">{chunk.title || `Passage ${i + 1}`}</span>
                  </div>
                  <div class="source-snippet">{chunk.snippet}</div>
                </div>
              {/each}
            </div>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
{/if}

<style>
  .routing-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    margin-bottom: 8px;
    cursor: pointer;
  }

  .meta-chip {
    font-size: 0.65rem;
    padding: 1px 8px;
    border: 0.5px solid var(--border-mid);
    border-radius: 100px;
    color: var(--text-muted);
    font-family: var(--font-mono);
    letter-spacing: 0.02em;
    transition: border-color 0.15s;
  }

  .routing-meta:hover .meta-chip {
    border-color: var(--border-bright);
  }

  .meta-source {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 25%, transparent);
    background: var(--accent-glow);
  }

  /* Always-visible "answered-by" model chip. Long gguf stems are
     ellipsized so they don't dominate the row; the full string
     (incl. "@ peer <name>" for mesh-served turns) stays available
     in the `title` tooltip, so nothing is hidden — just compacted. */
  .meta-model {
    color: var(--lavender-light, var(--accent));
    border-color: color-mix(
      in srgb,
      var(--lavender-light, var(--accent)) 30%,
      transparent
    );
    background: color-mix(
      in srgb,
      var(--lavender-light, var(--accent)) 8%,
      transparent
    );
    max-width: 24ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Mesh-served turns lean on the accent so "this ran on a peer"
     reads at a glance, distinct from the local lavender. */
  .meta-model.meta-peer {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 35%, transparent);
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }

  /* Context-budget glassbox — chip tints warmer as the turn
     approaches the slot's n_ctx ceiling. Crosses 75% → soft yellow
     (still safe but worth noticing); crosses 90% → red (next big
     retrieval might trim aggressively or hit the synth budget).
     Matches the cutoff-chip palette so the surface family reads as
     one "budget pressure" vocabulary, not three competing signals. */
  .meta-chip.ctx-tight {
    color: var(--warning, #c08a3e);
    border-color: color-mix(in srgb, var(--warning, #c08a3e) 35%, transparent);
    background: color-mix(in srgb, var(--warning, #c08a3e) 8%, transparent);
  }

  .meta-chip.ctx-critical {
    color: var(--error, #c45650);
    border-color: color-mix(in srgb, var(--error, #c45650) 45%, transparent);
    background: color-mix(in srgb, var(--error, #c45650) 10%, transparent);
  }

  .routing-detail {
    margin-bottom: 10px;
    padding: 8px 12px;
    background: var(--bg-surface);
    border-radius: var(--radius);
    border: 0.5px solid var(--border-mid);
    font-size: 0.75rem;
    color: var(--text-secondary);
    line-height: 1.55;
  }

  .trigger-line {
    color: var(--text-muted);
    font-size: 0.72rem;
    margin-top: -2px;
    margin-bottom: 2px;
    padding-left: 4px;
  }

  .sources-section {
    margin-top: 8px;
    border-top: 0.5px solid var(--border);
    padding-top: 6px;
  }

  .sources-toggle {
    display: flex;
    align-items: center;
    gap: 6px;
    background: none;
    border: none;
    color: var(--text-secondary);
    font-size: 0.75rem;
    cursor: pointer;
    padding: 2px 0;
    font-family: var(--font-sans);
  }
  .sources-toggle:hover {
    color: var(--text-primary);
  }

  .toggle-arrow {
    font-size: 0.7em;
    opacity: 0.6;
  }

  .sources-list {
    margin-top: 6px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .source-item {
    background: var(--bg-elevated);
    border-radius: var(--radius);
    padding: 8px 10px;
    border: 0.5px solid var(--border);
  }

  .source-header {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 4px;
  }

  .source-badge {
    font-size: 0.65rem;
    font-family: var(--font-mono);
    padding: 0 5px;
    border-radius: 3px;
    background: var(--lavender-dim);
    color: var(--lavender-light);
    white-space: nowrap;
  }

  .source-title {
    font-weight: 600;
    color: var(--text-primary);
    font-size: 0.75rem;
  }

  .source-snippet {
    font-size: 0.72rem;
    color: var(--text-muted);
    line-height: 1.5;
    font-family: var(--font-serif);
  }
</style>
