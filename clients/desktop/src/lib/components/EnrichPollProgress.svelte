<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Shared poll-based enrichment progress.

  Renders the live phase + fraction of an in-process tiered build
  (POST /internal/corpus/enrich-once, read via `enrichmentStatus`) and
  fires `onComplete` / `onFailed` exactly once when the build reaches a
  terminal state.

  This replaces the old job-based `EnrichmentStage`, which tracked a
  `sovereign-cli enrich build` SUBPROCESS through `enrichProgressStore`.
  `sovereign-cli` is not bundled with the desktop, so that path was
  structurally broken in shipped builds (exit 127). Every enrichment
  surface now drives the daemon's in-process RAPTOR/entity build and
  polls this shared status instead.

  Separation of concerns: the PARENT kicks the build (`lcEnrichReset` +
  `lcEnrichNow`, plus any pre-steps like writing a governance recipe);
  this component only OBSERVES. It starts polling on mount, so a build
  already in flight (auto-enrich after ingest, or a prior session) shows
  immediately without a kick.
-->
<script lang="ts">
  import { onDestroy } from "svelte";

  import { enrichmentStatus } from "../api";
  import type { EnrichmentStatus } from "../api";

  interface Props {
    /// Corpus whose enrichment state to poll.
    corpusId: string;
    /// Optional heading rendered above the phase caption.
    label?: string;
    /// Fired exactly once when the build reaches phase `complete`.
    onComplete?: () => void;
    /// Fired exactly once when the build fails or stalls. `reason`
    /// carries the daemon's stamped error when one was recorded.
    onFailed?: (reason: string) => void;
  }
  let { corpusId, label, onComplete, onFailed }: Props = $props();

  let status = $state<EnrichmentStatus | null>(null);
  let pollHandle: ReturnType<typeof setInterval> | null = null;
  let firedTerminal = false;

  /// User-facing phase captions. Mirrors the taxonomy in
  /// `corpus-engine::enrichment::state::EnrichmentPhase`.
  function phaseLabel(phase?: string): string {
    switch (phase) {
      case "starting":
        return "Starting…";
      case "scanning":
        return "Scanning documents";
      case "entity_extraction":
        return "Finding people, places, and ideas";
      case "raptor_leaves":
        return "Summarizing sections";
      case "raptor_tree":
        return "Building the summary tree";
      case "motif_extraction":
        return "Finding recurring themes";
      case "atom_extraction":
        return "Extracting claims";
      case "persisting":
        return "Saving the map";
      default:
        return "Building…";
    }
  }

  function stopPoll() {
    if (pollHandle) {
      clearInterval(pollHandle);
      pollHandle = null;
    }
  }

  async function pollOnce() {
    let s: EnrichmentStatus;
    try {
      s = await enrichmentStatus(corpusId);
    } catch {
      return; // transient daemon hiccup — keep polling
    }
    status = s;
    const phase = s.state?.phase;
    if (phase === "complete") {
      stopPoll();
      if (!firedTerminal) {
        firedTerminal = true;
        onComplete?.();
      }
    } else if (phase === "failed") {
      stopPoll();
      if (!firedTerminal) {
        firedTerminal = true;
        onFailed?.(s.state?.error ?? "Enrichment failed.");
      }
    } else if (s.is_stalled) {
      stopPoll();
      if (!firedTerminal) {
        firedTerminal = true;
        onFailed?.("Enrichment stalled — no progress. Try again.");
      }
    }
  }

  // (Re)start polling whenever the corpus changes. Cleanup stops the
  // interval on unmount or corpus switch.
  $effect(() => {
    void corpusId; // track
    firedTerminal = false;
    status = null;
    stopPoll();
    pollHandle = setInterval(() => void pollOnce(), 2000);
    void pollOnce();
    return stopPoll;
  });

  onDestroy(stopPoll);

  let frac = $derived(status?.fraction_complete ?? 0);
</script>

<div
  class="enrich-progress"
  role="status"
  aria-live="polite"
  data-testid="enrich-poll-progress"
>
  {#if label}<div class="enrich-heading">{label}</div>{/if}
  <div class="enrich-phase">
    <span>{phaseLabel(status?.state?.phase)}</span>
    {#if frac > 0}
      <span class="enrich-pct">{Math.round(frac * 100)}%</span>
    {/if}
  </div>
  <div class="enrich-bar">
    <div class="enrich-fill" style:width={`${Math.max(frac * 100, 2)}%`}></div>
  </div>
  {#if status?.state?.message}
    <p class="enrich-msg">{status.state.message}</p>
  {/if}
</div>

<style>
  .enrich-progress {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .enrich-heading {
    font-weight: 600;
    font-size: 0.9rem;
  }
  .enrich-phase {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    font-size: 0.85rem;
    color: var(--text-2, #556077);
  }
  .enrich-pct {
    font-variant-numeric: tabular-nums;
    opacity: 0.8;
  }
  .enrich-bar {
    height: 6px;
    border-radius: 999px;
    background: var(--surface-3, rgba(0, 0, 0, 0.08));
    overflow: hidden;
  }
  .enrich-fill {
    height: 100%;
    border-radius: 999px;
    background: var(--accent, #6b5cff);
    transition: width 0.4s ease;
  }
  .enrich-msg {
    margin: 0;
    font-size: 0.78rem;
    color: var(--text-3, #7a8699);
  }
</style>
