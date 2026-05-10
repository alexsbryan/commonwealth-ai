<script lang="ts">
  import type { LocalCorpusProgress } from "../../types";
  import InkStamp from "../onboarding/InkStamp.svelte";
  import ProgressRule from "../onboarding/ProgressRule.svelte";

  interface Props {
    progress: LocalCorpusProgress | null;
  }

  let { progress }: Props = $props();

  let phaseLabel = $derived.by(() => {
    if (!progress) return "Starting";
    switch (progress.phase) {
      case "scanning":      return "Scanning";
      case "staging":       return "Reading documents";
      case "ocr_page":      return "Recognising text";
      case "ingesting":     return progress.data.phase_label;
      case "clustering":    return "Clustering";
      case "snapshotting":  return "Saving restore point";
      case "writing":       return "Writing tags";
      case "rolling_back":  return "Restoring";
      case "complete":      return "Done";
      case "error":         return "Failed";
    }
  });

  /// null → indeterminate (ProgressRule renders a sweep). 0..1
  /// otherwise. We can't compute a fraction for `scanning` until
  /// the walker emits a total; let it read as indeterminate there.
  let fraction = $derived.by<number | null>(() => {
    if (!progress) return null;
    if (progress.phase === "complete") return 1;
    if (progress.phase === "error") return null;
    if (
      progress.phase === "scanning" ||
      progress.phase === "staging" ||
      progress.phase === "snapshotting" ||
      progress.phase === "writing" ||
      progress.phase === "rolling_back"
    ) {
      const total = progress.data.total;
      const done = progress.data.done;
      if (!total || total <= 0) return null;
      return done / total;
    }
    if (progress.phase === "ingesting") {
      const total = Number(progress.data.total);
      const done = Number(progress.data.done);
      if (!total || total <= 0) return null;
      return done / total;
    }
    if (progress.phase === "ocr_page") {
      // Page-level fraction within the current file. Across-files
      // fraction would need running totals we don't track today.
      const total = progress.data.total_pages;
      const done = progress.data.page;
      if (!total || total <= 0) return null;
      return done / total;
    }
    return null;
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
      case "ocr_page":
        // The "counter" for OCR is "page N of M" within the current
        // file, with the file name surfaced as `file`.
        return {
          done: progress.data.page,
          total: progress.data.total_pages,
          file: `${progress.data.file} — page ${progress.data.page} of ${progress.data.total_pages}`,
        };
      default:
        return { done: 0, total: 0, file: "" };
    }
  });

  let counterStr = $derived.by(() => {
    if (counter.total > 0) {
      return `${counter.done.toLocaleString()} / ${counter.total.toLocaleString()}`;
    }
    if (fraction !== null) {
      return `${Math.round(fraction * 100)}%`;
    }
    return "";
  });

  let detail = $derived.by(() => {
    if (!progress) return "";
    if (progress.phase === "error") return progress.data.message;
    return "";
  });

  let active = $derived(
    !!progress && progress.phase !== "complete" && progress.phase !== "error",
  );
  let tone = $derived<"error" | "neutral" | "rest">(
    progress?.phase === "error" ? "error" : "neutral",
  );
</script>

<section class="panel" class:is-error={progress?.phase === "error"}>
  <header class="head">
    <span class="head-mark">
      <InkStamp size="sm" {active} />
    </span>
    <h2 class="phase">{phaseLabel}</h2>
  </header>

  <ProgressRule
    value={fraction}
    counter={counterStr || undefined}
    {tone}
  />

  {#if counter.file}
    <p class="file" title={counter.file}>
      <span class="file-dot" aria-hidden="true">·</span>
      <span class="file-name">{counter.file}</span>
    </p>
  {/if}

  {#if detail}
    <p class="detail">{detail}</p>
  {/if}
</section>

<style>
  .panel {
    padding: 24px 0 4px;
    animation: lk-fade-in 240ms ease-out both;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 12px;
  }
  .head-mark {
    display: inline-flex;
  }
  .phase {
    margin: 0;
    font-size: var(--lk-size-lead);
    font-weight: 500;
    color: var(--lk-ink);
    letter-spacing: -0.01em;
  }

  .file {
    margin: 10px 0 0;
    display: flex;
    gap: 6px;
    align-items: baseline;
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 0.78rem;
    min-width: 0;
  }
  .file-dot {
    color: var(--accent);
    flex-shrink: 0;
  }
  .file-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }
  .detail {
    margin: 10px 0 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-err);
  }
</style>
