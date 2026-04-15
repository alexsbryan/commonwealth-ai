<script lang="ts">
  interface Props {
    provenance?: {
      intent: string;
      search_method?: string;
      sources?: { origin: string; count: number }[];
      total_latency_ms: number;
      tokens_used: number;
      inference_backend: string;
      oicp_match?: string;
      coarse_intent?: string;
      self_assessment?: string;
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

  let corporaSearched = $derived(
    (provenance?.sources ?? [])
      .filter((s) => s.count > 0)
      .map((s) => s.origin),
  );

  // "sep (6)" for local hits, "sep (6) via BeefyMac" when the
  // mesh fan-out served this corpus. `from_peer` is stamped by
  // `prepare_knowledge_context` when the originating corpus_id
  // isn't present locally — so same-corpus-two-ways never lies
  // to the user about where a hit came from.
  let corporaDetail = $derived(
    (provenance?.sources ?? [])
      .filter((s) => s.count > 0)
      .map((s) =>
        s.from_peer
          ? `${s.origin} (${s.count}) via ${s.from_peer}`
          : `${s.origin} (${s.count})`,
      ),
  );

  let elapsedLabel = $derived(
    provenance
      ? provenance.total_latency_ms < 1000
        ? `${provenance.total_latency_ms}ms`
        : `${(provenance.total_latency_ms / 1000).toFixed(1)}s`
      : "",
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
    {#if corporaSearched.length > 0}
      <span class="meta-chip meta-source"
        >Searched {corporaSearched.join(", ")}</span
      >
    {/if}
    <span class="meta-chip">{elapsedLabel}</span>
    {#if provenance.tokens_used > 0}
      <span class="meta-chip">{provenance.tokens_used} tok</span>
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
          : ""}
      </div>

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
