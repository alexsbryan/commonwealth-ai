<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { OutlierNote } from "../../../types";

  interface Props {
    outliers: OutlierNote[];
  }

  let { outliers }: Props = $props();

  function pct(n: number): number {
    return Math.round(n * 100);
  }

  function reasonCopy(o: OutlierNote): string {
    switch (o.reason.type) {
      case "low_confidence":
        return `${pct(o.best_cluster_confidence)}% — below ${pct(o.reason.threshold)}% threshold`;
      case "ambiguous_cluster":
        return "spans multiple clusters";
      case "too_short":
        return `too short — ${o.reason.char_count} characters`;
      case "singleton_cluster":
        return "only note in its cluster — needs at least 2 to be tagged";
    }
  }
</script>

{#if outliers.length > 0}
  <section class="outliers" aria-label="Outliers">
    <header class="head">
      <h4 class="title">Outliers</h4>
      <span class="count lk-folio">{outliers.length}</span>
    </header>
    <p class="hint">
      These notes don't fit a cluster. svrnmesh won't tag them.
      Drop the threshold above to include more.
    </p>

    <ol class="list">
      {#each outliers as o (o.chunk_id)}
        <li class="row">
          <span class="note-title">{o.note_title}</span>
          <span class="reason" data-reason={o.reason.type}>
            {reasonCopy(o)}
          </span>
        </li>
      {/each}
    </ol>
  </section>
{/if}

<style>
  .outliers {
    margin-top: 24px;
    padding: 16px 18px 14px;
    background: var(--lk-paper-deep);
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-bottom: 4px;
  }
  .title {
    margin: 0;
    font-size: var(--lk-size-lead);
    font-weight: 500;
    color: var(--lk-ink);
  }
  .count {
    color: var(--lk-ink-faded);
    font-variant-numeric: tabular-nums;
  }
  .hint {
    margin: 0 0 12px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    max-width: 62ch;
    line-height: 1.45;
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    border-top: 1px solid var(--lk-rule-soft);
  }
  .row {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 14px;
    padding: 8px 0;
    border-bottom: 1px solid var(--lk-rule-soft);
    align-items: baseline;
  }
  .row:last-child { border-bottom: 0; }
  .note-title {
    font-size: var(--lk-size-body);
    color: var(--lk-ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .reason {
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
    text-align: right;
    white-space: nowrap;
  }
  .reason[data-reason="singleton_cluster"] { color: var(--lk-crown-light); }
  .reason[data-reason="too_short"] { color: var(--lk-warn); }
  .reason[data-reason="ambiguous_cluster"] { color: var(--lk-stamp-ink); }
</style>
