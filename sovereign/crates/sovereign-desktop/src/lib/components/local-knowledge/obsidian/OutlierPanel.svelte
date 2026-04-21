<script lang="ts">
  import type { OutlierNote } from "../../../types";

  interface Props {
    outliers: OutlierNote[];
  }

  let { outliers }: Props = $props();

  function pct(n: number): string {
    return `${Math.round(n * 100)}%`;
  }
</script>

{#if outliers.length > 0}
  <div class="outlier-panel">
    <div class="outlier-header">
      <h5>Notes that didn't fit a cluster</h5>
      <p class="outlier-explanation">
        These {outliers.length} notes are likely the most distinctive in your
        vault. Sovereign won't tag them. You can review them or ignore them —
        no action required.
      </p>
    </div>

    <div class="outlier-list">
      {#each outliers as o (o.chunk_id)}
        <div class="outlier-row">
          <span class="outlier-title">{o.note_title}</span>
          <span class="outlier-reason">
            {#if o.reason.type === "low_confidence"}
              Best match: {pct(o.best_cluster_confidence)} — below {pct(o.reason.threshold)} threshold
            {:else if o.reason.type === "ambiguous_cluster"}
              Spans multiple clusters
            {:else if o.reason.type === "too_short"}
              Too brief to cluster ({o.reason.char_count} chars)
            {:else if o.reason.type === "singleton_cluster"}
              Only note in its cluster — needs at least 2 to be tagged
            {/if}
          </span>
        </div>
      {/each}
    </div>
  </div>
{/if}

<style>
  .outlier-panel {
    margin-top: 20px;
    padding: 16px;
    border: 1px dashed var(--color-border, #d4d4d4);
    border-radius: 6px;
    background: var(--color-surface-subtle, #f4f4f4);
  }
  h5 {
    margin: 0 0 4px;
    font-size: 14px;
    font-weight: 500;
  }
  .outlier-explanation {
    margin: 0 0 12px;
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
  }
  .outlier-row {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    padding: 6px 0;
    border-bottom: 1px dotted var(--color-border, #e4e4e4);
  }
  .outlier-row:last-child {
    border-bottom: none;
  }
  .outlier-title {
    font-size: 13px;
    font-weight: 500;
  }
  .outlier-reason {
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
    text-align: right;
  }
</style>
