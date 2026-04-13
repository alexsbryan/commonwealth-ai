<script lang="ts">
  import type { DocumentAssetOperation } from "../types";

  interface Props {
    operation: DocumentAssetOperation;
  }

  let { operation }: Props = $props();

  let expanded = $state(false);

  const labels: Record<string, string> = {
    Rag: "Retrieved passages",
    Synthesis: "Synthesised across full document",
    Aggregation: "Found all instances",
    Transformation: "Applied transformation",
  };

  const icons: Record<string, string> = {
    Rag: "\u2197",
    Synthesis: "\u25C8",
    Aggregation: "\u2295",
    Transformation: "\u2298",
  };

  let opType = $derived(
    typeof operation === "string" ? operation : Object.keys(operation)[0],
  );
  let label = $derived(labels[opType] || opType);
  let icon = $derived(icons[opType] || "\u00B7");
  let details = $derived(
    typeof operation === "string" ? {} : (Object.values(operation)[0] as Record<string, unknown>),
  );
</script>

<button
  class="operation-badge"
  onclick={() => (expanded = !expanded)}
  title="How this was answered"
>
  <span class="op-icon">{icon}</span>
  <span class="op-label">{label}</span>
  {#if opType === "Synthesis" && (details as { entities?: string[] }).entities?.length}
    <span class="op-detail"
      >&mdash; {(details as { entities: string[] }).entities.join(", ")}</span
    >
  {/if}
  <span class="op-expand">{expanded ? "\u25B2" : "\u25BC"}</span>
</button>

{#if expanded}
  <div class="op-explanation">
    {#if opType === "Rag"}
      This question was answered by retrieving the most relevant passages
      from the document and synthesising them.
    {:else if opType === "Synthesis"}
      This question required understanding the full document. The system
      traced how themes and entities develop across all sections.
    {:else if opType === "Aggregation"}
      The system searched every section of the document for instances
      matching your query.
    {:else if opType === "Transformation"}
      A transformation was applied to the document content.
    {/if}
  </div>
{/if}

<style>
  .operation-badge {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 3px 8px;
    border: 0.5px solid var(--border);
    border-radius: 4px;
    font-size: 11px;
    color: var(--text-secondary);
    background: none;
    cursor: pointer;
    margin-bottom: 8px;
  }
  .operation-badge:hover {
    background: var(--bg-elevated);
  }
  .op-icon {
    font-size: 10px;
  }
  .op-detail {
    color: var(--text-muted);
  }
  .op-expand {
    font-size: 8px;
    margin-left: 2px;
  }
  .op-explanation {
    font-size: 11px;
    color: var(--text-secondary);
    padding: 6px 8px;
    background: var(--bg-surface);
    border-radius: 4px;
    margin-bottom: 10px;
    line-height: 1.5;
  }
</style>
