<script lang="ts">
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy } from "svelte";

  import { lcCluster, lcGetPreview, lcWriteTags } from "../../../api";
  import type {
    ClusterConfig,
    LocalCorpusConfig,
    LocalCorpusProgress,
    VaultPreview,
    WriteBackResult,
  } from "../../../types";

  // v1 defaults — spec §6.3 plus the M5.1 additions. The slider in
  // the review panel mutates `min_confidence` and
  // `min_notes_per_cluster`; the rest are fixed until M4b exposes the
  // full OrganizerControls panel. `min_notes_per_cluster` defaults to
  // 2 so a lone-note "cluster" never gets its own tag — the user
  // flagged this as premature tagging during the first demo.
  const DEFAULT_CLUSTER_CONFIG: ClusterConfig = {
    min_cluster_size: 5,
    min_confidence: 0.4,
    multi_tag_threshold: 0.6,
    multi_cluster_strategy: "Dominant",
    min_notes_per_cluster: 2,
  };

  import IngestProgressPanel from "../IngestProgressPanel.svelte";
  import ClusterReviewPanel from "./ClusterReviewPanel.svelte";
  import ConfirmWritePanel from "./ConfirmWritePanel.svelte";
  import CorpusDangerZone from "./CorpusDangerZone.svelte";

  interface Props {
    config: LocalCorpusConfig;
  }

  let { config }: Props = $props();

  type Step =
    | { kind: "idle" }
    | {
        kind: "clustering";
        progress: LocalCorpusProgress | null;
      }
    | { kind: "loading_preview" }
    | { kind: "review"; preview: VaultPreview }
    | { kind: "confirm"; preview: VaultPreview }
    | { kind: "writing" }
    | { kind: "write_complete"; result: WriteBackResult }
    | { kind: "error"; message: string };

  let step: Step = $state({ kind: "idle" });
  let unlisten: UnlistenFn | null = null;
  let dangerZoneReloadKey = $state(0);
  // Live cluster-config used when fetching the preview. Only
  // `min_confidence` is user-adjustable in v1 (via the slider in the
  // review panel); the rest ride the defaults.
  let clusterConfig: ClusterConfig = $state({ ...DEFAULT_CLUSTER_CONFIG });
  let previewReloadTimer: ReturnType<typeof setTimeout> | null = null;

  onDestroy(() => {
    if (unlisten) unlisten();
  });

  async function runOrganize() {
    step = { kind: "clustering", progress: null };
    try {
      const jobId = await lcCluster(config.id);
      const channel = `local-corpus://progress/${jobId}`;
      unlisten = await listen<LocalCorpusProgress>(channel, async (event) => {
        if (step.kind === "clustering") {
          step = { ...step, progress: event.payload };
        }
        if (event.payload.phase === "complete") {
          if (unlisten) {
            unlisten();
            unlisten = null;
          }
          step = { kind: "loading_preview" };
          try {
            const preview = await lcGetPreview(config.id, clusterConfig);
            step = { kind: "review", preview };
          } catch (e: unknown) {
            step = {
              kind: "error",
              message: `Preview fetch failed: ${e}`,
            };
          }
        }
        if (event.payload.phase === "error") {
          if (unlisten) {
            unlisten();
            unlisten = null;
          }
          step = {
            kind: "error",
            message: event.payload.data.message,
          };
        }
      });
    } catch (e: unknown) {
      step = {
        kind: "error",
        message: `Could not start clustering: ${e}`,
      };
    }
  }

  function enterConfirm() {
    if (step.kind !== "review") return;
    step = { kind: "confirm", preview: step.preview };
  }

  function backFromConfirm() {
    if (step.kind !== "confirm") return;
    step = { kind: "review", preview: step.preview };
  }

  async function runWrite(gitCommit: boolean) {
    if (step.kind !== "confirm") return;
    step = { kind: "writing" };
    try {
      const result = await lcWriteTags(config.id, gitCommit);
      step = { kind: "write_complete", result };
      dangerZoneReloadKey += 1;
    } catch (e: unknown) {
      step = { kind: "error", message: `Write failed: ${e}` };
    }
  }

  function closeReview() {
    step = { kind: "idle" };
  }

  /// Called by ClusterReviewPanel when the user drags the confidence
  /// slider. Debounced so a scrubbing gesture doesn't fire a burst of
  /// backend calls. The preview call is cheap (no LLM, no embedding —
  /// it re-runs the classifier against the cached LabeledClusterResult)
  /// but it does walk every chunk, so 250ms of grace is polite.
  async function handleConfidenceChange(newMinConfidence: number) {
    clusterConfig = { ...clusterConfig, min_confidence: newMinConfidence };
    if (previewReloadTimer) clearTimeout(previewReloadTimer);
    previewReloadTimer = setTimeout(async () => {
      if (step.kind !== "review") return;
      try {
        const preview = await lcGetPreview(config.id, clusterConfig);
        if (step.kind === "review") {
          step = { kind: "review", preview };
        }
      } catch (e) {
        // Slider should never nuke the whole review; fall through
        // and keep the prior preview rendered.
        console.error("live preview refresh failed:", e);
      }
    }, 250);
  }
</script>

<div class="organizer">
  {#if step.kind === "idle"}
    <div class="idle-card">
      <h4>Organize {config.display_name}</h4>
      <p class="desc">
        Sovereign will cluster your notes and propose a hierarchy of tags.
        Nothing is written to your vault; you see the proposal first.
      </p>
      <button class="btn-primary" onclick={runOrganize}>Organize</button>
    </div>
  {:else if step.kind === "clustering"}
    <IngestProgressPanel progress={step.progress} />
  {:else if step.kind === "loading_preview"}
    <p class="loading">Building preview…</p>
  {:else if step.kind === "review"}
    <ClusterReviewPanel
      preview={step.preview}
      minConfidence={clusterConfig.min_confidence}
      onMinConfidenceChange={handleConfidenceChange}
      onCancel={closeReview}
      onWrite={enterConfirm}
    />
  {:else if step.kind === "confirm"}
    <ConfirmWritePanel
      corpusId={config.id}
      preview={step.preview}
      onBack={backFromConfirm}
      onConfirm={runWrite}
    />
  {:else if step.kind === "writing"}
    <p class="loading">Writing tags to your vault…</p>
  {:else if step.kind === "write_complete"}
    <div class="write-complete">
      <h4>Done</h4>
      <p>
        Tagged <strong>{step.result.files_tagged}</strong> notes and created
        <strong>{step.result.index_notes_created}</strong> index notes.
        {#if step.result.files_skipped.length > 0}
          {step.result.files_skipped.length} files were skipped.
        {/if}
      </p>
      <p class="hint">
        A restore point was saved. You can roll back below if anything
        looks wrong in your vault.
      </p>
      <button class="btn-secondary" onclick={closeReview}>Close</button>
    </div>
  {:else if step.kind === "error"}
    <div class="error-panel">
      <p class="error-title">Clustering failed</p>
      <p>{step.message}</p>
      <button class="btn-secondary" onclick={() => (step = { kind: "idle" })}>
        Try again
      </button>
    </div>
  {/if}

  {#key dangerZoneReloadKey}
    <div class="danger-zone-wrap">
      <CorpusDangerZone corpusId={config.id} onReset={() => (dangerZoneReloadKey += 1)} />
    </div>
  {/key}
</div>

<style>
  .organizer {
    padding: 16px 0;
  }
  .idle-card {
    padding: 16px;
    border: 1px solid var(--color-border, #d4d4d4);
    border-radius: 6px;
  }
  h4 {
    margin: 0 0 8px;
    font-size: 15px;
    font-weight: 500;
  }
  .desc {
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
    margin: 0 0 12px;
    line-height: 1.4;
  }
  .loading {
    padding: 16px;
    color: var(--color-text-muted, #6b6b6b);
    font-size: 13px;
  }
  .error-panel {
    padding: 16px;
    border: 1px solid var(--color-error, #c92a2a);
    border-radius: 6px;
  }
  .error-title {
    font-weight: 500;
    color: var(--color-error, #c92a2a);
    margin: 0 0 8px;
  }
  .btn-primary {
    padding: 8px 16px;
    border-radius: 6px;
    font-size: 14px;
    cursor: pointer;
    border: none;
    background: var(--color-accent, #3a5fc9);
    color: #fff;
  }
  .btn-primary:hover {
    background: var(--color-accent-hover, #2f4fb3);
  }
  .btn-secondary {
    padding: 8px 16px;
    border-radius: 6px;
    font-size: 14px;
    cursor: pointer;
    border: 1px solid var(--color-border, #d4d4d4);
    background: transparent;
    color: var(--color-text, #1a1a1a);
    margin-top: 12px;
  }
  .write-complete {
    padding: 16px;
    border: 1px solid var(--color-accent, #3a5fc9);
    border-radius: 6px;
    background: color-mix(in srgb, var(--color-accent, #3a5fc9) 5%, transparent);
  }
  .write-complete h4 {
    margin: 0 0 8px;
    font-size: 15px;
    font-weight: 500;
  }
  .write-complete p {
    margin: 0 0 10px;
    font-size: 13px;
  }
  .write-complete .hint {
    color: var(--color-text-muted, #6b6b6b);
    font-style: italic;
  }
  .danger-zone-wrap {
    margin-top: 24px;
    padding-top: 20px;
    border-top: 1px solid var(--color-border, #d4d4d4);
  }
</style>
