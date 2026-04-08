<script lang="ts">
  import SourceAttribution, { stripSources } from "./SourceAttribution.svelte";

  interface Props {
    role: string;
    content: string;
    metadata?: Record<string, unknown>;
  }

  let { role, content, metadata }: Props = $props();

  let displayContent = $derived(
    role !== "user" ? stripSources(content) : content,
  );

  let provenance = $derived(
    metadata?.provenance as
      | {
          intent: string;
          search_method?: string;
          sources?: { origin: string; count: number }[];
          inference_backend: string;
          oicp_match?: string;
          total_latency_ms: number;
          tokens_used: number;
        }
      | undefined,
  );

  let provenanceExpanded = $state(false);
</script>

<div class="bubble" class:user={role === "user"} class:assistant={role !== "user"}>
  <div class="role-label">{role === "user" ? "You" : "◈ SOVEREIGN"}</div>
  <div class="content">{displayContent}</div>
  {#if role !== "user"}
    <SourceAttribution {content} />
    {#if provenance}
      <div
        class="provenance-bar"
        role="button"
        tabindex="0"
        onclick={() => (provenanceExpanded = !provenanceExpanded)}
        onkeydown={(e) => e.key === "Enter" && (provenanceExpanded = !provenanceExpanded)}
      >
        <span class="prov-chip">{provenance.intent}</span>
        {#each (provenance.sources ?? []).filter(s => s.count > 0) as src}
          <span class="prov-chip prov-source">{src.origin} · {src.count}</span>
        {/each}
        <span class="prov-chip prov-meta">{provenance.total_latency_ms}ms</span>
        {#if provenance.tokens_used > 0}
          <span class="prov-chip prov-meta">{provenance.tokens_used} tok</span>
        {/if}
      </div>
      {#if provenanceExpanded}
        <div class="provenance-detail">
          <div><strong>Model:</strong> {provenance.inference_backend}</div>
          {#if provenance.search_method}
            <div><strong>Search:</strong> {provenance.search_method}</div>
          {/if}
          {#if provenance.oicp_match}
            <div><strong>OICP:</strong> {provenance.oicp_match}</div>
          {/if}
          {#if (provenance.sources ?? []).length > 0}
            <div style="margin-top: 4px"><strong>Sources:</strong></div>
            {#each provenance.sources ?? [] as src}
              <div class="prov-source-row">
                <span class="prov-origin">{src.origin}</span>
                <span class="prov-count">{src.count === 0 ? "searched, 0 results" : `${src.count} chunks`}</span>
              </div>
            {/each}
          {/if}
        </div>
      {/if}
    {/if}
  {/if}
</div>

<style>
  .bubble {
    max-width: 82%;
    margin-bottom: 18px;
    word-wrap: break-word;
    white-space: pre-wrap;
  }

  /* User messages — contained, warm */
  .user {
    background: var(--user-bubble);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg) var(--radius-lg) var(--radius) var(--radius-lg);
    padding: 12px 16px;
    align-self: flex-end;
    margin-left: auto;
  }

  .user .role-label {
    text-align: right;
    color: var(--text-muted);
    font-size: 0.7rem;
    font-weight: 500;
    letter-spacing: 0.05em;
    margin-bottom: 5px;
    text-transform: uppercase;
  }

  /* Assistant messages — open, left-anchored */
  .assistant {
    align-self: flex-start;
    padding: 0 0 0 14px;
    border-left: 2px solid color-mix(in srgb, var(--growth) 30%, transparent);
  }

  .assistant .role-label {
    font-size: 0.67rem;
    font-weight: 700;
    letter-spacing: 0.12em;
    color: var(--accent);
    margin-bottom: 6px;
    text-transform: uppercase;
    filter: drop-shadow(0 0 4px rgba(212, 136, 42, 0.25));
  }

  .content {
    line-height: 1.65;
    color: var(--text-primary);
  }

  /* ── Provenance ── */
  .provenance-bar {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    margin-top: 10px;
    cursor: pointer;
  }

  .prov-chip {
    font-size: 0.67rem;
    padding: 2px 9px;
    background: transparent;
    border: 1px solid var(--border-mid);
    border-radius: 100px;
    color: var(--text-muted);
    white-space: nowrap;
    font-family: 'Syne Mono', monospace;
    letter-spacing: 0.02em;
    transition: border-color 0.15s, color 0.15s;
  }

  .provenance-bar:hover .prov-chip {
    border-color: var(--border-bright);
  }

  .prov-source {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 25%, transparent);
    background: var(--accent-glow);
  }

  .prov-meta {
    opacity: 0.65;
  }

  .provenance-detail {
    margin-top: 8px;
    padding: 10px 14px;
    background: var(--bg-surface);
    border-radius: var(--radius);
    border: 1px solid var(--border-mid);
    font-size: 0.78rem;
    color: var(--text-secondary);
    line-height: 1.55;
    white-space: normal;
  }

  .prov-source-row {
    display: flex;
    justify-content: space-between;
    padding: 2px 0 2px 10px;
    gap: 12px;
  }

  .prov-origin {
    font-weight: 600;
  }

  .prov-count {
    color: var(--text-muted);
    font-family: 'Syne Mono', monospace;
    font-size: 0.72rem;
  }
</style>
