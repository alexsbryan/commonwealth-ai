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
  }

  let { provenance }: Props = $props();
  let expanded = $state(false);

  let corporaSearched = $derived(
    (provenance?.sources ?? [])
      .filter((s) => s.count > 0)
      .map((s) => s.origin),
  );

  let corporaDetail = $derived(
    (provenance?.sources ?? [])
      .filter((s) => s.count > 0)
      .map((s) => `${s.origin} (${s.count})`),
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
    <!-- DEV-PROVENANCE:START — remove entire block before shipping to end users -->
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
          ? ` · ${provenance.tokens_used} tok`
          : ""}
      </div>
    </div>
    <!-- DEV-PROVENANCE:END -->
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
</style>
