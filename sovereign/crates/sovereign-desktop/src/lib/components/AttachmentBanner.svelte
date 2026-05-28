<script lang="ts">
  import { onMount } from "svelte";
  import { documentIngestionStore } from "../stores/documentIngestion.svelte";
  import type { AssetState } from "../types";

  interface Props {
    filename: string;
    chunksCreated: number;
    onremove: () => void;
    /** Optional asset id. When supplied, the banner subscribes to the
     *  shared documentIngestionStore and surfaces live state — a
     *  spinner + "Indexing…" sublabel during Pending / Indexing /
     *  BuildingSkeleton, falling back to the chunk count once Ready.
     *  Without an asset id the banner renders the legacy static shape. */
    assetId?: string;
    /** Initial state the banner should display before any progress
     *  event arrives for `assetId`. Defaults to "Ready" so the legacy
     *  path (no asset id) keeps its previous behaviour. */
    initialState?: AssetState;
  }

  let {
    filename,
    chunksCreated,
    onremove,
    assetId,
    initialState = "Ready" as AssetState,
  }: Props = $props();

  onMount(async () => {
    if (assetId) await documentIngestionStore.init();
  });

  let liveState: AssetState = $derived.by(() => {
    if (!assetId) return "Ready" as AssetState;
    return documentIngestionStore.state(assetId) ?? initialState;
  });

  let isFailed = $derived(
    typeof liveState === "object" && liveState !== null && "Failed" in liveState,
  );

  let indexingPct = $derived.by(() => {
    if (typeof liveState === "object" && liveState !== null) {
      if ("Indexing" in liveState && liveState.Indexing.chunks_total > 0) {
        return Math.round(
          (liveState.Indexing.chunks_done / liveState.Indexing.chunks_total) * 100,
        );
      }
    }
    return null;
  });

  let skeletonPct = $derived.by(() => {
    if (typeof liveState === "object" && liveState !== null) {
      if ("BuildingSkeleton" in liveState && liveState.BuildingSkeleton.chunks_total > 0) {
        return Math.round(
          (liveState.BuildingSkeleton.chunks_done / liveState.BuildingSkeleton.chunks_total) * 100,
        );
      }
    }
    return null;
  });

  let stateLabel = $derived.by(() => {
    if (liveState === "Pending") return "Queued…";
    // PartiallyReady (T1) and MultiHopReady (T2) are mid-pipeline
    // milestones, NOT completion: the user can ask questions, but the
    // skeleton (T2) and RAPTOR atlas (T3, the bulk of the wall-clock)
    // are still building. Say so explicitly — "Ready for questions"
    // alone read as "done" while the logs were clearly still busy.
    if (liveState === "PartiallyReady") return "Ready for easy questions · still enriching";
    if (liveState === "MultiHopReady") return "Deeper answers ready · still enriching";
    if (typeof liveState === "object" && liveState !== null) {
      if ("Indexing" in liveState) {
        // chunks_done === 0 means embedding hasn't returned a batch
        // yet — usually the embed model warming up. Say "Preparing…"
        // rather than a stuck-looking "Indexing 0%".
        if (liveState.Indexing.chunks_done === 0) return "Preparing…";
        const pct = indexingPct;
        return pct != null ? `Indexing ${pct}%` : "Indexing…";
      }
      if ("BuildingSkeleton" in liveState) {
        const pct = skeletonPct;
        return pct != null && pct > 0
          ? `Building structure ${pct}%`
          : "Building structure…";
      }
      if ("Failed" in liveState) return "Failed";
    }
    return "";
  });

  // Show the in-progress treatment (spinner + ETA) for EVERY
  // non-terminal state — including PartiallyReady and MultiHopReady.
  // Those unlock RAG/multi-hop retrieval but the ingest keeps running
  // (T2 skeleton, then the ~4-min T3 RAPTOR build). Rendering them as a
  // static, spinner-less green label made a half-finished ingest look
  // complete — the exact "it said Ready but the logs kept going"
  // confusion. Only the terminal `Ready` (and `Failed`) drop the spinner.
  let showProgress = $derived(liveState !== "Ready" && !isFailed);

  // Re-evaluate the ETA ~once a second so it counts down even during
  // long phases that emit no progress events — the T3 RAPTOR build can
  // run a minute between ticks. `nowTick` is referenced by `etaLabel`
  // purely to force the recompute on each interval.
  let nowTick = $state(Date.now());
  $effect(() => {
    if (!showProgress || !assetId) return;
    const timer = setInterval(() => (nowTick = Date.now()), 1000);
    return () => clearInterval(timer);
  });

  let etaLabel = $derived.by(() => {
    void nowTick; // recompute each tick
    if (!assetId || !showProgress) return "";
    const secs = documentIngestionStore.etaSeconds(assetId);
    if (secs == null) return "estimating…";
    if (secs < 20) return "almost done";
    if (secs < 75) return "~1 min left";
    return `~${Math.round(secs / 60)} min left`;
  });
</script>

<div class="attachment-banner" class:indexing={showProgress}>
  <div class="attachment-info">
    <span class="attachment-icon" aria-hidden="true">
      {#if showProgress}
        <span class="attachment-spinner"></span>
      {:else}
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none">
          <path
            d="M14 4.5V14a1 1 0 01-1 1H3a1 1 0 01-1-1V2a1 1 0 011-1h6.5L14 4.5z"
            stroke="currentColor"
            stroke-width="1.2"
          />
          <path d="M9.5 1v4H14" stroke="currentColor" stroke-width="1.2" />
        </svg>
      {/if}
    </span>
    <span class="attachment-name">{filename}</span>
    {#if showProgress}
      <span class="attachment-state">{stateLabel}</span>
      {#if etaLabel}
        <span class="attachment-eta">· {etaLabel}</span>
      {/if}
    {:else if stateLabel}
      <span class="attachment-state" class:ready={!isFailed} class:failed={isFailed}
        >{stateLabel}</span>
    {:else}
      <span class="attachment-chunks">{chunksCreated} chunks</span>
    {/if}
  </div>
  <button class="attachment-remove" onclick={onremove} title="Remove attachment">
    &times;
  </button>
</div>

<style>
  .attachment-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 12px;
    margin: 0 0 4px 0;
    background: var(--bg-surface);
    border: 0.5px solid var(--border-mid);
    border-radius: var(--radius);
    font-size: 0.75rem;
    color: var(--text-secondary);
    font-family: var(--font-sans);
    transition: border-color 0.2s, background 0.2s;
  }
  .attachment-banner.indexing {
    border-color: var(--lavender);
    background: var(--lavender-dim);
  }

  .attachment-info {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .attachment-icon {
    color: var(--accent);
    display: flex;
    align-items: center;
  }

  .attachment-spinner {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    border: 1.5px solid var(--lavender);
    border-top-color: transparent;
    animation: attach-spin 0.9s linear infinite;
  }
  @keyframes attach-spin {
    to {
      transform: rotate(360deg);
    }
  }

  .attachment-name {
    font-weight: 600;
    color: var(--text-primary);
  }

  .attachment-chunks {
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 0.65rem;
  }

  .attachment-state {
    color: var(--lavender-light);
    font-size: 0.68rem;
    font-style: italic;
  }
  .attachment-state.ready {
    color: var(--growth);
    font-style: normal;
  }
  .attachment-state.failed {
    color: var(--error);
    font-style: normal;
  }

  .attachment-eta {
    color: var(--text-muted);
    font-size: 0.66rem;
    font-variant-numeric: tabular-nums;
  }

  .attachment-remove {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 1rem;
    cursor: pointer;
    padding: 0 2px;
    line-height: 1;
  }
  .attachment-remove:hover {
    color: var(--error);
  }
</style>
