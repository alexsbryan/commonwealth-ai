<script lang="ts">
  import type { ClusterSummary } from "../../../types";

  interface Props {
    summary: ClusterSummary;
    selected: boolean;
    onclick: () => void;
  }

  let { summary, selected, onclick }: Props = $props();
</script>

<button class="cluster-card" class:selected {onclick}>
  <div class="cluster-header">
    <span class="tag-path">{summary.cluster.tag_path}</span>
    <span class="count">{summary.cluster.note_count}</span>
  </div>
  <div class="display-name">{summary.cluster.display_name}</div>
  {#if summary.cluster.description}
    <div class="description">{summary.cluster.description}</div>
  {/if}
</button>

<style>
  .cluster-card {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 12px 14px;
    border: 1px solid var(--color-border, #d4d4d4);
    border-radius: 6px;
    background: var(--color-surface, #fff);
    text-align: left;
    cursor: pointer;
    color: var(--color-text, #1a1a1a);
    transition: border-color 120ms, background 120ms;
  }
  .cluster-card:hover {
    background: var(--color-surface-subtle, #f4f4f4);
  }
  .cluster-card.selected {
    border-color: var(--color-accent, #3a5fc9);
    background: color-mix(in srgb, var(--color-accent, #3a5fc9) 6%, transparent);
  }
  .cluster-header {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
    font-family: var(--font-mono, ui-monospace, monospace);
  }
  .count {
    font-weight: 500;
    color: var(--color-text, #1a1a1a);
  }
  .display-name {
    font-size: 14px;
    font-weight: 500;
  }
  .description {
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
    line-height: 1.4;
    overflow: hidden;
    text-overflow: ellipsis;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
  }
</style>
