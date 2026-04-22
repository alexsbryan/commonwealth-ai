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

  let phaseLabel = $derived.by(() => {
    if (!progress) return "Starting";
    switch (progress.phase) {
      case "scanning":      return "Scanning";
      case "staging":       return "Reading documents";
      case "ingesting":     return progress.data.phase_label;
      case "clustering":    return "Clustering";
      case "snapshotting":  return "Saving restore point";
      case "writing":       return "Writing tags";
      case "rolling_back":  return "Restoring";
      case "complete":      return "Done";
      case "error":         return "Failed";
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

  let counter = $derived.by(() => {
    if (!progress) return { done: 0, total: 0, file: "" };
    switch (progress.phase) {
      case "scanning":
      case "staging":
      case "snapshotting":
      case "writing":
      case "rolling_back":
        return {
          done: progress.data.done,
          total: progress.data.total,
          file: "current_file" in progress.data ? progress.data.current_file : "",
        };
      case "ingesting":
        return {
          done: Number(progress.data.done),
          total: Number(progress.data.total),
          file: progress.data.current_file ?? "",
        };
      default:
        return { done: 0, total: 0, file: "" };
    }
  });

  let detail = $derived.by(() => {
    if (!progress) return "";
    if (progress.phase === "error") return progress.data.message;
    return "";
  });
</script>

<section class="panel" class:is-error={progress?.phase === "error"}>
  <header class="head">
    <h2 class="phase">{phaseLabel}</h2>
    {#if counter.total > 0}
      <span class="ratio lk-folio">
        {counter.done.toLocaleString()} / {counter.total.toLocaleString()}
      </span>
    {:else}
      <span class="ratio lk-folio">{percent}%</span>
    {/if}
  </header>

  <div
    class="bar"
    role="progressbar"
    aria-valuenow={percent}
    aria-valuemin={0}
    aria-valuemax={100}
  >
    <div class="bar-fill" style="width: {percent}%"></div>
  </div>

  {#if counter.file}
    <p class="file lk-folio" title={counter.file}>{counter.file}</p>
  {/if}

  {#if detail}
    <p class="detail">{detail}</p>
  {/if}
</section>

<style>
  .panel {
    padding: 20px 0;
    animation: lk-fade-in 220ms ease-out both;
  }

  .head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 16px;
    margin-bottom: 10px;
  }
  .phase {
    margin: 0;
    font-size: var(--lk-size-lead);
    font-weight: 500;
    color: var(--lk-ink);
  }
  .ratio {
    color: var(--lk-ink-faded);
    font-variant-numeric: tabular-nums;
  }

  .bar {
    height: 4px;
    background: var(--lk-paper-deep);
    border-radius: 2px;
    overflow: hidden;
  }
  .bar-fill {
    height: 100%;
    background: var(--lk-crown);
    transition: width 240ms cubic-bezier(0.2, 0.8, 0.2, 1);
  }
  .is-error .bar-fill {
    background: var(--lk-err);
  }

  .file {
    margin: 10px 0 0;
    color: var(--lk-ink-soft);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .detail {
    margin: 10px 0 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-err);
  }
</style>
