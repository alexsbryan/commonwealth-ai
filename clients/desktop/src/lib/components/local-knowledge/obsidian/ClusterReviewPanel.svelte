<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { untrack } from "svelte";
  import type { VaultPreview } from "../../../types";
  import ClusterCard from "./ClusterCard.svelte";
  import ClusterDetail from "./ClusterDetail.svelte";
  import OutlierPanel from "./OutlierPanel.svelte";

  interface Props {
    preview: VaultPreview;
    onCancel: () => void;
    onWrite?: () => void;
    minConfidence?: number;
    onMinConfidenceChange?: (value: number) => void;
  }

  let {
    preview,
    onCancel,
    onWrite,
    minConfidence = 0.4,
    onMinConfidenceChange,
  }: Props = $props();

  function handleSlider(e: Event) {
    const value = Number((e.target as HTMLInputElement).value);
    onMinConfidenceChange?.(value);
  }

  // Initial selection comes from the prop on first render only —
  // the `$effect` below keeps `selectedId` in sync with `preview`
  // changes thereafter. `untrack` silences `state_referenced_locally`.
  let selectedId: number | null = $state(
    untrack(() =>
      preview.clusters.length > 0 ? preview.clusters[0].cluster.id : null,
    ),
  );

  let selected = $derived(
    preview.clusters.find((c) => c.cluster.id === selectedId) ?? null,
  );

  $effect(() => {
    if (selectedId === null && preview.clusters.length > 0) {
      selectedId = preview.clusters[0].cluster.id;
    } else if (
      selectedId !== null &&
      !preview.clusters.some((c) => c.cluster.id === selectedId)
    ) {
      selectedId =
        preview.clusters.length > 0 ? preview.clusters[0].cluster.id : null;
    }
  });

  function fmtPct(n: number): string {
    return `${Math.round(n * 100)}`;
  }
</script>

<section class="review">
  <header class="head">
    <h2 class="title">Proposed tags</h2>
    <p class="lede">
      {#if preview.clusters.length > 0}
        {preview.clusters.length} clusters across {preview.tagged_notes} notes.
        {#if preview.outlier_count > 0}
          {preview.outlier_count} outliers.
        {/if}
      {:else}
        Nothing cleared the threshold. Try a lower one.
      {/if}
      Nothing is written until you confirm.
    </p>
  </header>

  {#if onMinConfidenceChange}
    <div class="threshold">
      <div class="threshold-head">
        <label for="min-confidence-slider" class="lk-label">
          Confidence threshold
        </label>
        <span class="threshold-value">
          <span class="lk-num">{fmtPct(minConfidence)}</span>%
        </span>
      </div>
      <input
        id="min-confidence-slider"
        class="slider"
        type="range"
        min="0"
        max="1"
        step="0.01"
        value={minConfidence}
        oninput={handleSlider}
        style="--v: {minConfidence * 100}%"
        aria-label="Minimum confidence for tagging"
      />
      <p class="threshold-hint">
        Notes below this aren't tagged. Drag left to include more, right
        for stricter.
      </p>
    </div>
  {/if}

  {#if preview.clusters.length === 0}
    <div class="empty">
      <p>No cluster cleared the threshold. Drop it, or re-cluster with a smaller minimum cluster size.</p>
    </div>
  {:else}
    <div class="body">
      <nav class="cluster-list" aria-label="Clusters">
        {#each preview.clusters as summary (summary.cluster.id)}
          <ClusterCard
            {summary}
            selected={selectedId === summary.cluster.id}
            onclick={() => (selectedId = summary.cluster.id)}
          />
        {/each}
      </nav>

      <div class="cluster-detail">
        {#if selected}
          <ClusterDetail summary={selected} />
        {/if}
      </div>
    </div>
  {/if}

  <OutlierPanel outliers={preview.outliers} />

  {#if preview.open_questions.length > 0}
    <section class="gaps">
      <p class="lk-label gaps-label">Gaps svrnmesh noticed</p>
      <ul class="gap-list">
        {#each preview.open_questions as q}
          <li>{q.gap_description}</li>
        {/each}
      </ul>
    </section>
  {/if}

  <footer class="footer">
    <p class="summary">
      <span class="lk-num">{preview.tagged_notes}</span> will be tagged ·
      <span class="lk-num">{preview.outlier_count}</span> won't be touched
    </p>
    <div class="actions">
      <button class="lk-btn lk-btn--quiet" onclick={onCancel}>Cancel</button>
      {#if onWrite && preview.tagged_notes > 0}
        <button class="lk-btn lk-btn--mark" onclick={onWrite}>
          Write tags
        </button>
      {/if}
    </div>
  </footer>
</section>

<style>
  .review {
    padding: 4px 0 0;
    animation: lk-fade-in 300ms ease-out both;
  }

  .head {
    margin-bottom: 20px;
  }
  .title {
    margin: 0 0 6px;
    font-size: var(--lk-size-hero);
    font-weight: 600;
    line-height: 1.1;
    letter-spacing: -0.02em;
    color: var(--lk-ink);
  }
  .lede {
    margin: 0;
    max-width: 64ch;
    font-size: var(--lk-size-body);
    color: var(--lk-ink-soft);
    line-height: 1.5;
  }

  .threshold {
    padding: 14px 16px;
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
    background: var(--lk-paper-subtle);
    margin-bottom: 24px;
  }
  .threshold-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-bottom: 10px;
  }
  .threshold-value {
    font-size: 1.25rem;
    color: var(--lk-stamp-ink);
    font-variant-numeric: tabular-nums;
  }
  .threshold-value .lk-num {
    color: inherit;
    font-weight: 600;
  }
  .slider {
    width: 100%;
    appearance: none;
    -webkit-appearance: none;
    height: 4px;
    background:
      linear-gradient(
        to right,
        var(--lk-stamp) 0 var(--v),
        var(--lk-paper-deep) var(--v) 100%
      );
    border: 0;
    border-radius: 2px;
    outline: none;
    cursor: pointer;
    margin: 4px 0;
  }
  .slider::-webkit-slider-thumb {
    appearance: none;
    -webkit-appearance: none;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--lk-stamp-ink);
    border: 2px solid var(--bg-root);
    cursor: grab;
    margin-top: -5px;
    box-shadow: 0 0 0 1px var(--lk-stamp);
  }
  .slider::-moz-range-thumb {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--lk-stamp-ink);
    border: 2px solid var(--bg-root);
    cursor: grab;
    box-shadow: 0 0 0 1px var(--lk-stamp);
  }
  .slider:active::-webkit-slider-thumb { cursor: grabbing; }
  .threshold-hint {
    margin: 8px 0 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
  }

  .empty {
    padding: 24px 16px;
    border: 1px dashed var(--lk-rule);
    border-radius: var(--radius);
    background: var(--lk-paper-subtle);
    color: var(--lk-ink-soft);
  }
  .empty p { margin: 0; }

  .body {
    display: grid;
    grid-template-columns: minmax(240px, 300px) 1fr;
    gap: 24px;
    margin-bottom: 8px;
    align-items: start;
  }
  .cluster-list {
    display: flex;
    flex-direction: column;
    max-height: 540px;
    overflow-y: auto;
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
  }
  .cluster-detail {
    min-width: 0;
    max-height: 540px;
    overflow-y: auto;
    padding: 0 4px;
  }

  .gaps {
    margin: 24px 0 8px;
    padding: 14px 16px;
    border-left: 2px solid var(--lk-crown);
    background: var(--lk-crown-wash);
    border-radius: var(--radius);
  }
  .gaps-label {
    color: var(--lk-crown-light);
    margin: 0 0 8px;
  }
  .gap-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .gap-list li {
    font-size: var(--lk-size-body);
    color: var(--lk-ink);
    line-height: 1.45;
  }

  .footer {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 20px;
    margin-top: 24px;
    padding-top: 16px;
    border-top: 1px solid var(--lk-rule);
    flex-wrap: wrap;
  }
  .summary {
    margin: 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
  }
  .summary .lk-num {
    color: var(--lk-ink);
    font-size: 1.125rem;
    margin: 0 2px;
  }
  .actions {
    display: flex;
    gap: 8px;
  }
</style>
