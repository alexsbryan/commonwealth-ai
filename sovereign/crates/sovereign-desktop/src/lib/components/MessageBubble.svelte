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
        <span class="prov-chip">{provenance.inference_backend}</span>
        <span class="prov-chip">{provenance.tokens_used} tokens</span>
        <span class="prov-chip">{provenance.total_latency_ms}ms</span>
        {#if provenance.oicp_match}
          <span class="prov-chip">{provenance.oicp_match}</span>
        {/if}
      </div>
      {#if provenanceExpanded}
        <div class="provenance-detail">
          <div>Intent: {provenance.intent}</div>
          {#if provenance.search_method}
            <div>Search: {provenance.search_method}</div>
          {/if}
          {#each provenance.sources ?? [] as src}
            <div>{src.origin}: {src.count} chunks</div>
          {/each}
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
</style>
