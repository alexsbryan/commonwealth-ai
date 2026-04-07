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
  <div class="role-label">{role === "user" ? "You" : "Sovereign"}</div>
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
    max-width: 80%;
    padding: 12px 16px;
    border-radius: var(--radius-lg);
    margin-bottom: 12px;
    word-wrap: break-word;
    white-space: pre-wrap;
  }

  .user {
    background: var(--user-bubble);
    align-self: flex-end;
    margin-left: auto;
    border-bottom-right-radius: 4px;
  }

  .assistant {
    background: var(--assistant-bubble);
    align-self: flex-start;
    border: 1px solid var(--border);
    border-bottom-left-radius: 4px;
  }

  .role-label {
    font-size: 0.75rem;
    color: var(--text-muted);
    margin-bottom: 4px;
    font-weight: 500;
  }

  .content {
    line-height: 1.6;
  }

  .provenance-bar {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
    cursor: pointer;
  }

  .prov-chip {
    font-size: 0.7rem;
    padding: 1px 8px;
    background: var(--bg-primary);
    border-radius: 10px;
    color: var(--text-muted);
    white-space: nowrap;
  }

  .prov-source {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }

  .prov-meta {
    opacity: 0.7;
  }

  .provenance-detail {
    margin-top: 6px;
    padding: 8px 12px;
    background: var(--bg-surface);
    border-radius: var(--radius);
    border: 1px solid var(--border);
    font-size: 0.8rem;
    color: var(--text-secondary);
    line-height: 1.5;
    white-space: normal;
  }

  .prov-source-row {
    display: flex;
    justify-content: space-between;
    padding: 1px 0 1px 8px;
    gap: 12px;
  }

  .prov-origin {
    font-weight: 500;
  }

  .prov-count {
    color: var(--text-muted);
  }
</style>
