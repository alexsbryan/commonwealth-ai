<script lang="ts">
  import type { LocalCorpusProgress } from "../../types";

  interface Props {
    progress: LocalCorpusProgress | null;
  }

  let { progress }: Props = $props();

  function pct(done: number, total: number): number {
    if (total <= 0) return 0;
    return Math.min(100, Math.round((done / total) * 100));
  }

  // Phase-aware labels. Each phase of the pipeline gets its own
  // human phrasing so the user can tell whether we're reading files,
  // chunking, embedding, or indexing. Matches spec §8.4.
  let phaseLabel = $derived.by(() => {
    if (!progress) return "Starting…";
    switch (progress.phase) {
      case "scanning":
        return "Scanning your files";
      case "staging":
        return "Reading your documents";
      case "ingesting":
        return progress.data.phase_label;
      case "clustering":
        return "Finding patterns";
      case "snapshotting":
        return "Saving a restore point";
      case "writing":
        return "Writing tags";
      case "rolling_back":
        return "Restoring previous state";
      case "complete":
        return "Done";
      case "error":
        return "Something went wrong";
    }
  });

  let percent = $derived.by(() => {
    if (!progress) return 0;
    if (
      progress.phase === "scanning" ||
      progress.phase === "staging" ||
      progress.phase === "snapshotting" ||
      progress.phase === "writing" ||
      progress.phase === "rolling_back"
    ) {
      return pct(progress.data.done, progress.data.total);
    }
    if (progress.phase === "ingesting") {
      return pct(Number(progress.data.done), Number(progress.data.total));
    }
    if (progress.phase === "complete") return 100;
    return 0;
  });

  let detailLine = $derived.by(() => {
    if (!progress) return "";
    if (progress.phase === "staging") {
      return `${progress.data.done} of ${progress.data.total} · ${progress.data.current_file}`;
    }
    if (progress.phase === "scanning") {
      return `${progress.data.done} of ${progress.data.total}`;
    }
    if (progress.phase === "ingesting" && Number(progress.data.total) > 0) {
      return `${progress.data.done} of ${progress.data.total}`;
    }
    if (progress.phase === "error") {
      return progress.data.message;
    }
    return "";
  });
</script>

<div class="progress-panel">
  <div class="phase-label">{phaseLabel}</div>
  <div class="progress-bar" class:is-error={progress?.phase === "error"}>
    <div class="progress-fill" style="width: {percent}%"></div>
  </div>
  {#if detailLine}
    <div class="progress-detail">{detailLine}</div>
  {/if}
</div>

<style>
  .progress-panel {
    padding: 16px 0;
  }
  .phase-label {
    font-size: 14px;
    font-weight: 500;
    margin-bottom: 8px;
    color: var(--color-text, #1a1a1a);
  }
  .progress-bar {
    height: 6px;
    background: var(--color-surface-subtle, #eee);
    border-radius: 3px;
    overflow: hidden;
  }
  .progress-bar.is-error {
    background: color-mix(in srgb, var(--color-error, #c92a2a) 15%, transparent);
  }
  .progress-fill {
    height: 100%;
    background: var(--color-accent, #3a5fc9);
    transition: width 200ms ease-out;
  }
  .is-error .progress-fill {
    background: var(--color-error, #c92a2a);
  }
  .progress-detail {
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
    margin-top: 6px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
