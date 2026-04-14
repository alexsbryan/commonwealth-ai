<script lang="ts">
  import { onMount } from "svelte";
  import { documentIngestionStore } from "../stores/documentIngestion.svelte";
  import type { DocumentAsset, AssetState } from "../types";

  interface Props {
    asset: DocumentAsset;
  }

  let { asset }: Props = $props();

  // Read ingestion state from the singleton store; fall back to the
  // asset's initial `state` field before any progress event has
  // arrived for this id. `ragReady` derives from the state rather
  // than a separate flag — PartiallyReady means the RAG path is up.
  let assetState: AssetState = $derived(
    documentIngestionStore.state(asset.id) ?? asset.state,
  );
  let ragReady = $derived(
    assetState === "PartiallyReady" ||
      (typeof assetState === "object" &&
        assetState !== null &&
        "BuildingSkeleton" in assetState),
  );

  onMount(async () => {
    await documentIngestionStore.init();
  });

  function progressFraction(s: AssetState): number {
    if (typeof s === "object" && "Indexing" in s) {
      return s.Indexing.chunks_total > 0
        ? (s.Indexing.chunks_done / s.Indexing.chunks_total) * 0.5
        : 0;
    }
    if (s === "PartiallyReady") return 0.5;
    if (typeof s === "object" && "BuildingSkeleton" in s) {
      return s.BuildingSkeleton.chunks_total > 0
        ? 0.5 +
            (s.BuildingSkeleton.chunks_done / s.BuildingSkeleton.chunks_total) *
              0.5
        : 0.5;
    }
    return 0;
  }

  function statusText(s: AssetState): string {
    if (typeof s === "object" && "Indexing" in s) {
      return `Reading document\u2026 ${s.Indexing.chunks_done} of ${s.Indexing.chunks_total} sections`;
    }
    if (s === "PartiallyReady") {
      return "Basic questions available. Building full structure\u2026";
    }
    if (typeof s === "object" && "BuildingSkeleton" in s) {
      return `Understanding structure\u2026 ${s.BuildingSkeleton.chunks_done} of ${s.BuildingSkeleton.chunks_total} sections`;
    }
    return "";
  }

  let fraction = $derived(progressFraction(assetState));
  let status = $derived(statusText(assetState));
</script>

{#if assetState !== "Ready"}
  <div class="ingest-banner">
    <div class="ingest-bar-track">
      <div class="ingest-bar-fill" style="width: {fraction * 100}%"></div>
    </div>

    <div class="ingest-status">
      <span class="mark">{"\u25C8"}</span>
      {status}
    </div>

    {#if ragReady}
      <div class="ingest-available">
        Basic questions are available. Full character and structure analysis
        will be ready when processing completes.
      </div>
    {:else}
      <div class="ingest-waiting">
        This document is being read carefully. This cost is paid once &mdash;
        every question after this will be answered instantly.
      </div>
    {/if}
  </div>
{/if}

<style>
  .ingest-banner {
    margin: 0 24px 16px;
    padding: 12px 14px;
    background: var(--bg-surface);
    border-radius: var(--radius);
    border: 1px solid var(--border);
  }
  .ingest-bar-track {
    height: 2px;
    background: var(--border);
    border-radius: 1px;
    margin-bottom: 10px;
    overflow: hidden;
  }
  .ingest-bar-fill {
    height: 100%;
    background: var(--accent);
    transition: width 2s ease;
  }
  .ingest-status {
    font-size: 12px;
    color: var(--text-secondary);
    margin-bottom: 6px;
  }
  .mark {
    color: var(--accent);
    margin-right: 4px;
  }
  .ingest-available,
  .ingest-waiting {
    font-size: 11px;
    color: var(--text-muted);
    line-height: 1.5;
  }
</style>
