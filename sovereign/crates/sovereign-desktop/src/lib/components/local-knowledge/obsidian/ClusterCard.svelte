<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { ClusterSummary } from "../../../types";

  interface Props {
    summary: ClusterSummary;
    selected: boolean;
    onclick: () => void;
    index?: number;
  }

  let { summary, selected, onclick }: Props = $props();
</script>

<button
  class="card"
  class:selected
  {onclick}
  aria-current={selected ? "true" : "false"}
>
  <div class="head">
    <code class="tag">{summary.cluster.tag_path}</code>
    <span class="count">
      <span class="count-num lk-num">{summary.cluster.note_count}</span>
      <span class="count-label">notes</span>
    </span>
  </div>
  <h3 class="title">{summary.cluster.display_name}</h3>
  {#if summary.cluster.description}
    <p class="description">{summary.cluster.description}</p>
  {/if}
</button>

<style>
  .card {
    position: relative;
    display: block;
    width: 100%;
    padding: 12px 14px;
    border: 0;
    border-bottom: 1px solid var(--lk-rule);
    background: transparent;
    color: var(--lk-ink);
    text-align: left;
    cursor: pointer;
    transition: background 140ms ease;
  }
  .card:last-child { border-bottom: 0; }
  .card:hover { background: var(--lk-paper-subtle); }
  .card:focus-visible {
    outline: 2px solid var(--lk-crown);
    outline-offset: -2px;
  }

  /* Left-edge selection mark — subtle, gold when active. */
  .card::before {
    content: "";
    position: absolute;
    top: 0;
    left: 0;
    bottom: 0;
    width: 3px;
    background: transparent;
    transition: background 160ms ease;
  }
  .card.selected::before {
    background: var(--lk-stamp);
  }
  .card.selected {
    background: var(--lk-paper-subtle);
  }

  .head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 10px;
    margin-bottom: 4px;
  }
  .tag {
    font-family: var(--lk-font-mono);
    font-size: 11.5px;
    color: var(--lk-stamp-ink);
    background: transparent;
    padding: 0;
    letter-spacing: 0;
  }
  .count {
    display: inline-flex;
    align-items: baseline;
    gap: 4px;
  }
  .count-num {
    font-size: 1rem;
    color: var(--lk-ink);
  }
  .count-label {
    font-size: 11px;
    color: var(--lk-ink-faded);
  }

  .title {
    margin: 0;
    font-size: var(--lk-size-body);
    font-weight: 500;
    color: var(--lk-ink);
    line-height: 1.25;
  }
  .description {
    margin: 4px 0 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    line-height: 1.4;
    overflow: hidden;
    text-overflow: ellipsis;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }
</style>
