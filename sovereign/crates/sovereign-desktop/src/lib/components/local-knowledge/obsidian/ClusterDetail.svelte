<script lang="ts">
  import type { ClusterSummary } from "../../../types";

  interface Props {
    summary: ClusterSummary;
  }

  let { summary }: Props = $props();

  function pct(c: number): number {
    return Math.round(c * 100);
  }

  function band(c: number): "high" | "mid" | "low" {
    if (c > 0.8) return "high";
    if (c > 0.6) return "mid";
    return "low";
  }
</script>

<article class="detail">
  <header class="header">
    <h3 class="title">{summary.cluster.display_name}</h3>
    <code class="tag">{summary.cluster.tag_path}</code>
    {#if summary.cluster.description}
      <p class="description">{summary.cluster.description}</p>
    {/if}
  </header>

  <div class="ledger-head">
    <span class="lk-label col-name">Note</span>
    <span class="lk-label col-confidence">Confidence</span>
  </div>

  <ol class="ledger">
    {#each summary.assignments as a (a.chunk_id)}
      <li class="row" class:is-low={a.confidence <= 0.6}>
        <span class="note-title">{a.note_title}</span>
        <span class="confidence">
          <span class="meter" aria-hidden="true">
            <span
              class="meter-fill"
              data-band={band(a.confidence)}
              style="width: {Math.round(a.confidence * 100)}%"
            ></span>
          </span>
          <span class="pct lk-num" data-band={band(a.confidence)}>
            {pct(a.confidence)}%
          </span>
        </span>
      </li>
    {/each}
  </ol>

  {#if summary.assignments.length === 0}
    <p class="empty">No notes pass the current threshold for this cluster.</p>
  {/if}
</article>

<style>
  .detail {
    padding: 4px 12px 8px;
  }
  .header {
    margin-bottom: 14px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--lk-rule);
  }
  .title {
    margin: 0 0 4px;
    font-size: var(--lk-size-display);
    font-weight: 600;
    color: var(--lk-ink);
    line-height: 1.2;
    letter-spacing: -0.01em;
  }
  .tag {
    font-family: var(--lk-font-mono);
    font-size: 12px;
    color: var(--lk-stamp-ink);
    background: transparent;
    padding: 0;
  }
  .description {
    margin: 8px 0 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    line-height: 1.5;
    max-width: 62ch;
  }

  .ledger-head {
    display: grid;
    grid-template-columns: 1fr 200px;
    gap: 16px;
    padding: 6px 0 8px;
    border-bottom: 1px solid var(--lk-rule);
  }
  .col-confidence {
    text-align: right;
  }

  .ledger {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .row {
    display: grid;
    grid-template-columns: 1fr 200px;
    gap: 16px;
    padding: 10px 0;
    align-items: center;
    border-bottom: 1px solid var(--lk-rule-soft);
    transition: background 120ms ease;
  }
  .row:hover {
    background: var(--lk-paper-deep);
  }
  .row.is-low .note-title {
    color: var(--lk-ink-soft);
  }
  .note-title {
    font-size: var(--lk-size-body);
    color: var(--lk-ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  .confidence {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 10px;
    align-items: center;
    justify-self: end;
    width: 100%;
  }
  .meter {
    position: relative;
    height: 4px;
    background: var(--lk-paper-deep);
    border-radius: 2px;
    overflow: hidden;
  }
  .meter-fill {
    display: block;
    height: 100%;
    border-radius: 2px;
  }
  .meter-fill[data-band="high"] { background: var(--lk-crown-light); }
  .meter-fill[data-band="mid"]  { background: var(--lk-gold); }
  .meter-fill[data-band="low"]  { background: var(--lk-warn); }

  .pct {
    font-size: 13px;
    min-width: 42px;
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--lk-ink);
  }
  .pct[data-band="high"] { color: var(--lk-crown-light); }
  .pct[data-band="mid"]  { color: var(--lk-gold); }
  .pct[data-band="low"]  { color: var(--lk-warn); }

  .empty {
    padding: 16px 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
  }
</style>
