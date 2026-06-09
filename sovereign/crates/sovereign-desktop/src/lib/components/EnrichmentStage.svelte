<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  EnrichmentStage — inline atlas-build progress UI.

  Single source of truth for rendering an in-flight or terminal
  `EnrichJobState`. Consumed by:

    - `EnrichmentPanel.svelte` (Settings → Enrichment tab, one row per corpus)
    - `FolderDropFlow.svelte` (Settings → Local Knowledge, after ingest)
    - `OrganizerPanel.svelte` (Obsidian, after cluster writeback)
    - `FirstCorpusFlow.svelte` (onboarding, post-ingest)

  Callers own the subprocess lifecycle (enrich_init_for_local_corpus +
  enrichBuildAsync + store.track). Stage is a pure view over one
  `EnrichJobState` plus a Cancel button that calls the store-shared
  cancellation flag.
-->
<script lang="ts">
  import type { EnrichBuildStep } from "../types";
  import type { EnrichJobState } from "../stores/enrichProgress.svelte";
  import { enrichCancelBuild } from "../api";

  interface Props {
    /// When `null`, the stage renders nothing (caller hasn't wired
    /// a build yet). When `EnrichJobState`, renders the progress
    /// block and — while non-terminal — a Cancel button.
    job: EnrichJobState | null;
    /// Optional label shown above the progress bar. Default: omitted
    /// (Settings panel uses the corpus row header instead; onboarding
    /// flows surface "Building knowledge atlas" here).
    label?: string;
    /// Fired exactly once per terminal transition. Lets the caller
    /// advance a parent state machine without having to subscribe to
    /// the progress store independently. Duplicates are suppressed.
    onTerminal?: (
      kind: "complete" | "aborted" | "cancelled" | "spawn_failed",
      job: EnrichJobState,
    ) => void;
    /// Hide the Cancel button. Useful when the parent wants to own
    /// cancellation UX (e.g. place Cancel in a wizard footer).
    hideCancel?: boolean;
  }

  let { job, label, onTerminal, hideCancel = false }: Props = $props();

  let cancelling = $state(false);
  // Guard against duplicate terminal fires — `$effect` retriggers on
  // every prop change even when `job.terminal` is stable.
  let lastTerminalFired: string | null = null;

  async function cancel() {
    if (!job) return;
    cancelling = true;
    try {
      await enrichCancelBuild(job.job_id);
      // The subprocess polls its cancel flag on the next stdout read,
      // emits a terminal `cancelled` event, and the store transitions
      // the job. No local state update needed here.
    } finally {
      cancelling = false;
    }
  }

  $effect(() => {
    if (!job || !job.terminal) return;
    if (job.terminal === lastTerminalFired) return;
    lastTerminalFired = job.terminal;
    onTerminal?.(job.terminal, job);
  });

  function stepLabel(step: EnrichBuildStep): string {
    switch (step) {
      case "seed":      return "Seed entities";
      case "extract":   return "Per-section extraction";
      case "cluster":   return "Cluster by facet";
      case "name":      return "Name clusters";
      case "resolve":   return "Resolve atoms + edges";
      case "tensions":  return "Tension candidates";
      case "gaps":      return "Structural gaps";
      case "configure": return "Configurations";
      case "report":    return "Schema validation";
    }
  }
</script>

{#if job}
  {@const stepsDone = job.stepsCompleted.length}
  {@const stepsTotal = job.plannedSteps.length || (job.currentStep?.total ?? 0)}
  {@const pct = stepsTotal > 0 ? Math.round((stepsDone / stepsTotal) * 100) : 0}
  <div
    class="progress-block"
    class:is-error={job.terminal === "aborted" || job.terminal === "spawn_failed"}
    class:is-cancelled={job.terminal === "cancelled"}
  >
    {#if label}
      <p class="stage-label">{label}</p>
    {/if}
    <div class="progress-head">
      {#if job.terminal === "complete"}
        <span class="progress-label">Complete — {job.stepsCompleted.length} step(s) finished</span>
      {:else if job.terminal === "aborted"}
        <span class="progress-label">
          Failed at {job.failedStep ? stepLabel(job.failedStep) : "unknown step"}
          (exit {job.exitCode ?? "?"})
        </span>
      {:else if job.terminal === "cancelled"}
        <span class="progress-label">
          Cancelled{job.failedStep ? ` mid-${stepLabel(job.failedStep).toLowerCase()}` : ""}
        </span>
      {:else if job.terminal === "spawn_failed"}
        <span class="progress-label">
          Could not start: {job.spawnErrorMessage ?? "unknown error"}
        </span>
      {:else if job.currentStep}
        <span class="progress-label">
          Step {job.currentStep.ordinal}/{job.currentStep.total}:
          {stepLabel(job.currentStep.step)}
        </span>
      {:else}
        <span class="progress-label">Starting…</span>
      {/if}
      <span class="progress-pct">{pct}%</span>
    </div>
    <div class="bar" role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100}>
      <div class="bar-fill" style="width: {pct}%"></div>
    </div>
    {#if job.chapterProgress}
      <p class="chapter-line">
        ↳ chapter {job.chapterProgress.index}/{job.chapterProgress.total}
        · <code>{job.chapterProgress.chapter_id}</code>
        {#if job.chapterProgress.question_count !== null}
          · {job.chapterProgress.question_count} q
        {/if}
      </p>
    {/if}
    {#if job.chapterFailures.length > 0}
      <p class="chapter-failures">
        {job.chapterFailures.length} chapter failure(s) captured.
      </p>
    {/if}
    {#if !job.terminal && !hideCancel}
      <div class="stage-actions">
        <button class="stage-cancel" onclick={cancel} disabled={cancelling}>
          {cancelling ? "Cancelling…" : "Cancel"}
        </button>
      </div>
    {/if}
  </div>
{/if}

<style>
  .progress-block {
    margin-top: 12px;
    padding-top: 10px;
    border-top: 1px solid var(--border, #333);
  }
  .stage-label {
    margin: 0 0 8px;
    font-size: 0.85em;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-secondary, var(--text-primary));
  }
  .progress-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 12px;
  }
  .progress-label {
    font-size: 0.9em;
    color: var(--text-primary, #eee);
  }
  .progress-pct {
    font-variant-numeric: tabular-nums;
    color: var(--text-muted, var(--text-secondary));
    font-size: 0.9em;
  }
  .bar {
    margin-top: 6px;
    height: 4px;
    background: var(--border, #333);
    border-radius: 2px;
    overflow: hidden;
  }
  .bar-fill {
    height: 100%;
    background: var(--accent, #c4a46a);
    transition: width 240ms cubic-bezier(0.2, 0.8, 0.2, 1);
  }
  .progress-block.is-error .bar-fill {
    background: var(--error, #d27979);
  }
  /* Cancellation is neither success nor failure — muted grey so it
     reads as "stopped" rather than pulling eye like error red. */
  .progress-block.is-cancelled .bar-fill {
    background: var(--text-muted, #888);
  }
  .chapter-line {
    margin: 8px 0 0;
    color: var(--text-secondary, var(--text-primary));
    font-size: 0.85em;
  }
  .chapter-failures {
    margin: 6px 0 0;
    font-size: 0.85em;
    color: var(--error, #d27979);
  }
  .stage-actions {
    margin-top: 10px;
    display: flex;
    justify-content: flex-end;
  }
  .stage-cancel {
    background: transparent;
    border: 1px solid var(--error, #d27979);
    color: var(--error, #d27979);
    padding: 4px 12px;
    border-radius: 4px;
    font-size: 0.85em;
    cursor: pointer;
  }
  .stage-cancel:hover:not(:disabled) {
    background: color-mix(in oklab, var(--error, #d27979) 12%, transparent);
  }
  .stage-cancel:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
