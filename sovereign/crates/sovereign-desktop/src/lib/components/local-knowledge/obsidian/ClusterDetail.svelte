<script lang="ts">
  import type { ClusterSummary, FileAssignment } from "../../../types";

  interface Props {
    summary: ClusterSummary;
  }

  let { summary }: Props = $props();

  function pct(c: number): string {
    return `${Math.round(c * 100)}%`;
  }

  function confidenceClass(c: number): string {
    if (c > 0.8) return "high";
    if (c > 0.6) return "mid";
    return "low";
  }
</script>

<div class="detail">
  <div class="header">
    <h4>{summary.cluster.display_name}</h4>
    <div class="tag-line">
      <code>sovereign/{summary.cluster.tag_path}</code>
    </div>
  </div>

  {#if summary.cluster.description}
    <p class="description">{summary.cluster.description}</p>
  {/if}

  <div class="assignments">
    {#each summary.assignments as a (a.chunk_id)}
      <div class="row" class:low-confidence={a.confidence <= 0.6}>
        <div class="note-info">
          <span class="note-title">{a.note_title}</span>
        </div>
        <div class="confidence-display">
          <div class="meter">
            <div
              class="fill {confidenceClass(a.confidence)}"
              style="width: {Math.min(100, Math.round(a.confidence * 100))}%"
            ></div>
          </div>
          <span class="value">{pct(a.confidence)}</span>
        </div>
      </div>
    {/each}
  </div>
</div>

<style>
  .detail {
    padding: 0 4px;
  }
  .header {
    margin-bottom: 12px;
  }
  h4 {
    margin: 0 0 4px;
    font-size: 15px;
    font-weight: 500;
  }
  .tag-line code {
    font-size: 12px;
    color: var(--color-accent, #3a5fc9);
    background: color-mix(in srgb, var(--color-accent, #3a5fc9) 8%, transparent);
    padding: 2px 6px;
    border-radius: 3px;
  }
  .description {
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
    margin: 0 0 16px;
    line-height: 1.4;
  }
  .assignments {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding: 8px 10px;
    border-radius: 4px;
    background: var(--color-surface-subtle, #f4f4f4);
  }
  .row.low-confidence {
    border: 1px dashed var(--color-border, #d4d4d4);
    background: transparent;
  }
  .note-title {
    font-size: 13px;
    font-weight: 500;
  }
  .confidence-display {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }
  .meter {
    width: 80px;
    height: 4px;
    background: var(--color-border, #d4d4d4);
    border-radius: 2px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    transition: width 200ms ease-out;
  }
  .fill.high {
    background: #4ca15c;
  }
  .fill.mid {
    background: #c9a53a;
  }
  .fill.low {
    background: #c96a3a;
  }
  .value {
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
    font-variant-numeric: tabular-nums;
    min-width: 34px;
    text-align: right;
  }
</style>
