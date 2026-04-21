<script lang="ts">
  import type { VaultPreview } from "../../../types";
  import ClusterCard from "./ClusterCard.svelte";
  import ClusterDetail from "./ClusterDetail.svelte";
  import OutlierPanel from "./OutlierPanel.svelte";

  interface Props {
    preview: VaultPreview;
    onCancel: () => void;
    /// Optional: when provided, a "Write tags to vault" action appears
    /// in the footer and invokes this callback. When absent, the
    /// panel is strictly read-only (M4 default).
    onWrite?: () => void;
    /// Current value of `min_confidence` reflected in the slider.
    /// When `onMinConfidenceChange` is wired the slider is enabled;
    /// otherwise it is hidden.
    minConfidence?: number;
    /// Called with the new threshold when the user moves the slider.
    /// The parent is expected to debounce + refetch the preview.
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

  let selectedId: number | null = $state(
    preview.clusters.length > 0 ? preview.clusters[0].cluster.id : null,
  );

  let selected = $derived(
    preview.clusters.find((c) => c.cluster.id === selectedId) ?? null,
  );

  // Keep the selected cluster pointed at something valid even when
  // the preview changes (e.g. user dropped the threshold and a new
  // cluster now has the most notes).
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
</script>

<div class="review">
  <div class="review-header">
    <h3>Proposed organization</h3>
    <p class="framing">
      Sovereign found <strong>{preview.clusters.length} clusters</strong>
      across {preview.tagged_notes} notes.
      {#if preview.outlier_count > 0}
        <strong>{preview.outlier_count} notes</strong> didn't fit confidently
        into any cluster — shown separately below.
      {/if}
      Review before anything is written.
    </p>

    {#if onMinConfidenceChange}
      <div class="confidence-slider">
        <label class="slider-label" for="min-confidence-slider">
          <span class="slider-title">Confidence threshold</span>
          <span class="slider-value">{Math.round(minConfidence * 100)}%</span>
        </label>
        <input
          id="min-confidence-slider"
          type="range"
          min="0"
          max="1"
          step="0.01"
          value={minConfidence}
          oninput={handleSlider}
        />
        <p class="slider-hint">
          Notes below this confidence land in the outlier panel rather than
          getting tagged. Drag left to include borderline notes; drag right
          to keep only the clearest matches.
        </p>
      </div>
    {/if}
  </div>

  {#if preview.clusters.length === 0}
    <p class="empty-note">
      No clusters emerged from this vault. That usually means the vault is
      too small or its notes are too varied to group usefully. Try again
      once you have more notes.
    </p>
  {:else}
    <div class="review-body">
      <div class="cluster-list">
        {#each preview.clusters as summary (summary.cluster.id)}
          <ClusterCard
            {summary}
            selected={selectedId === summary.cluster.id}
            onclick={() => (selectedId = summary.cluster.id)}
          />
        {/each}
      </div>

      <div class="cluster-detail">
        {#if selected}
          <ClusterDetail summary={selected} />
        {/if}
      </div>
    </div>
  {/if}

  <OutlierPanel outliers={preview.outliers} />

  {#if preview.open_questions.length > 0}
    <div class="open-questions">
      <h5>Gaps Sovereign noticed</h5>
      {#each preview.open_questions as q, i}
        <blockquote>{q.gap_description}</blockquote>
      {/each}
    </div>
  {/if}

  <div class="review-footer">
    <p class="footer-summary">
      {preview.tagged_notes} notes would be tagged · {preview.outlier_count}
      would not be touched.
    </p>
    <div class="footer-actions">
      <button class="btn-secondary" onclick={onCancel}>Cancel</button>
      {#if onWrite && preview.tagged_notes > 0}
        <button class="btn-primary" onclick={onWrite}>
          Write tags to vault
        </button>
      {/if}
    </div>
  </div>
</div>

<style>
  .review {
    padding: 16px 0;
  }
  .review-header {
    margin-bottom: 16px;
  }
  h3 {
    font-size: 16px;
    font-weight: 500;
    margin: 0 0 6px;
  }
  .framing {
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
    margin: 0 0 12px;
    line-height: 1.45;
  }
  .confidence-slider {
    padding: 12px 14px;
    background: var(--color-surface-subtle, #f4f4f4);
    border-radius: 6px;
    margin-top: 8px;
  }
  .slider-label {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-bottom: 8px;
  }
  .slider-title {
    font-size: 13px;
    font-weight: 500;
  }
  .slider-value {
    font-size: 13px;
    font-variant-numeric: tabular-nums;
    color: var(--color-accent, #3a5fc9);
    font-weight: 500;
  }
  .confidence-slider input[type="range"] {
    width: 100%;
    accent-color: var(--color-accent, #3a5fc9);
  }
  .slider-hint {
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
    margin: 6px 0 0;
    line-height: 1.4;
  }
  .empty-note {
    padding: 16px;
    background: var(--color-surface-subtle, #f4f4f4);
    border-radius: 6px;
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
  }
  .review-body {
    display: grid;
    grid-template-columns: 260px 1fr;
    gap: 20px;
    margin-bottom: 16px;
  }
  .cluster-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-height: 500px;
    overflow-y: auto;
  }
  .cluster-detail {
    min-width: 0;
    max-height: 500px;
    overflow-y: auto;
  }
  .open-questions {
    margin-top: 20px;
    padding: 16px;
    border: 1px solid var(--color-border, #d4d4d4);
    border-radius: 6px;
  }
  .open-questions h5 {
    font-size: 14px;
    font-weight: 500;
    margin: 0 0 10px;
  }
  blockquote {
    margin: 0 0 8px;
    padding-left: 12px;
    border-left: 3px solid var(--color-accent, #3a5fc9);
    font-size: 13px;
    color: var(--color-text, #1a1a1a);
    font-style: italic;
  }
  .review-footer {
    margin-top: 20px;
    padding-top: 16px;
    border-top: 1px solid var(--color-border, #d4d4d4);
  }
  .footer-summary {
    font-size: 13px;
    margin: 0 0 4px;
    font-weight: 500;
  }
  .footer-actions {
    display: flex;
    gap: 12px;
    margin-top: 12px;
  }
  .btn-primary,
  .btn-secondary {
    padding: 8px 16px;
    border-radius: 6px;
    font-size: 14px;
    cursor: pointer;
    border: none;
  }
  .btn-primary {
    background: var(--color-accent, #3a5fc9);
    color: #fff;
  }
  .btn-primary:hover {
    background: var(--color-accent-hover, #2f4fb3);
  }
  .btn-secondary {
    background: transparent;
    color: var(--color-text, #1a1a1a);
    border: 1px solid var(--color-border, #d4d4d4);
  }
  .btn-secondary:hover {
    background: var(--color-surface-subtle, #f4f4f4);
  }
</style>
