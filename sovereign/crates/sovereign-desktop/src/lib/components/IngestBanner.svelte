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

  // ETA estimates calibrated against measured ingest rates (Conrad
  // 1006-chunk doc on Primary 35B, lean-skeleton + grammar):
  //   - Embedding (Indexing): ~60s for 1000 chunks → 0.06s/chunk
  //   - Skeleton (BuildingSkeleton): ~7 min for 1006 chunks → 0.42s/chunk
  // Real per-chunk speed depends on hardware + chunk content; these
  // are good-enough for the user to know whether to wait or come back.
  const SECS_PER_CHUNK_INDEX = 0.06;
  const SECS_PER_CHUNK_SKELETON = 0.42;

  function etaSecs(s: AssetState): number {
    if (typeof s === "object" && "Indexing" in s) {
      const remainingIndex = Math.max(
        0,
        s.Indexing.chunks_total - s.Indexing.chunks_done,
      );
      const skeletonSecs = s.Indexing.chunks_total * SECS_PER_CHUNK_SKELETON;
      return remainingIndex * SECS_PER_CHUNK_INDEX + skeletonSecs;
    }
    if (s === "PartiallyReady") {
      // We don't know chunks_total at this state — show a rough
      // mid-range estimate until BuildingSkeleton begins.
      return 7 * 60;
    }
    if (typeof s === "object" && "BuildingSkeleton" in s) {
      const remaining = Math.max(
        0,
        s.BuildingSkeleton.chunks_total - s.BuildingSkeleton.chunks_done,
      );
      return remaining * SECS_PER_CHUNK_SKELETON;
    }
    return 0;
  }

  function fmtEta(secs: number): string {
    if (secs <= 0) return "any moment";
    if (secs < 60) return `~${Math.round(secs)}s`;
    const mins = Math.round(secs / 60);
    return mins === 1 ? "~1 min" : `~${mins} min`;
  }

  // Phase-specific status text. Honesty over optimism: each line
  // describes the actual capability the user has *right now*, not
  // a promise of what will arrive.
  function statusText(s: AssetState): string {
    if (typeof s === "object" && "Indexing" in s) {
      return `Reading document — ${s.Indexing.chunks_done} of ${s.Indexing.chunks_total} sections indexed`;
    }
    if (s === "PartiallyReady") {
      return "Searchable now — character recognition still building";
    }
    if (typeof s === "object" && "BuildingSkeleton" in s) {
      return `Searchable now — character recognition ${s.BuildingSkeleton.chunks_done} of ${s.BuildingSkeleton.chunks_total} sections`;
    }
    return "";
  }

  // What works at this phase, told concretely.
  function capabilityText(s: AssetState): string {
    if (typeof s === "object" && "Indexing" in s) {
      return "Search will be available once indexing completes.";
    }
    if (s === "PartiallyReady") {
      return "Ask factual questions now — the document is fully searchable. Character-aware analysis (who said what, scene comparison) will be sharper once the structural pass finishes.";
    }
    if (typeof s === "object" && "BuildingSkeleton" in s) {
      return "Ask anything. Cross-scene questions are getting more accurate as character recognition catches up.";
    }
    return "";
  }

  let fraction = $derived(progressFraction(assetState));
  let status = $derived(statusText(assetState));
  let capability = $derived(capabilityText(assetState));
  let eta = $derived(fmtEta(etaSecs(assetState)));
</script>

{#if assetState !== "Ready"}
  <div class="ingest-banner">
    <div class="ingest-bar-track">
      <div class="ingest-bar-fill" style="width: {fraction * 100}%"></div>
    </div>

    <div class="ingest-status">
      <span class="mark">{"◈"}</span>
      {status}
      <span class="eta">· full analysis in {eta}</span>
    </div>

    <div class="ingest-capability">{capability}</div>
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
  .eta {
    color: var(--text-muted);
    margin-left: 4px;
  }
  .ingest-capability {
    font-size: 11px;
    color: var(--text-muted);
    line-height: 1.5;
  }
</style>
