<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy } from "svelte";

  import {
    enrichGetStarterQuestions,
    lcCluster,
    lcEnrichNow,
    lcEnrichReset,
    lcGetPreview,
    lcWriteTags,
  } from "../../../api";
  import type {
    ClusterConfig,
    LocalCorpusConfig,
    LocalCorpusProgress,
    StarterQuestion,
    VaultPreview,
    WriteBackResult,
  } from "../../../types";
  import EnrichPollProgress from "../../EnrichPollProgress.svelte";
  import StarterChips from "../../StarterChips.svelte";

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
    /// Optional: when the user clicks a starter chip on atlas_complete,
    /// fire this so the caller can switch to chat + seed the input.
    onOpenChatWithSeed?: (question: StarterQuestion) => void;
  }

  let { config, onOpenChatWithSeed }: Props = $props();

  type AtlasPipelineId = "literary_atlas" | "philosophy_atlas";

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
    | {
        kind: "atlas_running";
        result: WriteBackResult;
        pipelineId: AtlasPipelineId;
        initError: string | null;
      }
    | {
        kind: "atlas_complete";
        result: WriteBackResult;
        starters: StarterQuestion[];
      }
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
    // The tiered build runs in the daemon; unmounting just stops the
    // <EnrichPollProgress> poll — the build keeps going and the Library
    // reflects completion.
  });

  // ── Atlas enrichment (post-writeback offer) ──────────────────────

  async function startAtlas(pipelineId: AtlasPipelineId) {
    if (step.kind !== "write_complete") return;
    const baseResult = step.result;
    step = {
      kind: "atlas_running",
      result: baseResult,
      pipelineId,
      initError: null,
    };
    try {
      // In-process tiered enrichment — no `sovereign-cli` subprocess
      // (it isn't bundled). Clear any zombie status, then kick the daemon
      // build; progress is polled by <EnrichPollProgress>.
      await lcEnrichReset(config.id);
      await lcEnrichNow(config.id);
    } catch (e: unknown) {
      if (step.kind === "atlas_running") {
        step = { ...step, initError: String(e) };
      }
    }
  }

  async function handleAtlasComplete() {
    if (step.kind !== "atlas_running") return;
    let starters: StarterQuestion[] = [];
    try {
      starters = await enrichGetStarterQuestions(config.id, 5);
    } catch (e) {
      console.warn("enrichGetStarterQuestions failed:", e);
    }
    step = { kind: "atlas_complete", result: step.result, starters };
  }

  function handleAtlasFailed(reason: string) {
    if (step.kind !== "atlas_running") return;
    step = { ...step, initError: reason };
  }

  function handleStarterPick(question: StarterQuestion) {
    if (onOpenChatWithSeed) {
      onOpenChatWithSeed(question);
    } else {
      closeReview();
    }
  }

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
    <section class="prompt">
      <div class="prompt-text">
        <h2 class="prompt-title">Organize {config.display_name}</h2>
        <p class="prompt-desc">
          svrnmesh clusters your notes by topic and proposes a tag for each
          cluster. Nothing is written to your vault until you confirm.
        </p>
      </div>
      <div class="prompt-actions">
        <button class="lk-btn lk-btn--mark" onclick={runOrganize}>Cluster</button>
      </div>
    </section>
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
    <p class="loading">Writing tags…</p>
  {:else if step.kind === "write_complete"}
    <section class="completion">
      <h3 class="completion-title">
        Tagged <span class="lk-num">{step.result.files_tagged}</span> notes,
        created <span class="lk-num">{step.result.index_notes_created}</span>
        index notes.
      </h3>
      {#if step.result.files_skipped.length > 0}
        <p class="completion-skipped">
          {step.result.files_skipped.length}
          {step.result.files_skipped.length === 1 ? "file" : "files"} skipped.
        </p>
      {/if}
      <p class="completion-hint">
        A restore point was saved. Roll back from the Restore section below
        if anything looks wrong.
      </p>
      <div class="atlas-offer">
        <p class="atlas-offer-title">Build a knowledge atlas?</p>
        <p class="atlas-offer-desc">
          Extract entities, events, and claims across the vault so you can
          ask nuanced questions. Takes several minutes.
        </p>
        <div class="atlas-offer-actions">
          <button
            class="lk-btn lk-btn--mark"
            onclick={() => startAtlas("literary_atlas")}
          >
            Build atlas (notes)
          </button>
          <button
            class="lk-btn lk-btn--quiet"
            onclick={() => startAtlas("philosophy_atlas")}
          >
            Build atlas (argumentative)
          </button>
          <button class="lk-btn lk-btn--quiet" onclick={closeReview}>
            Later
          </button>
        </div>
      </div>
    </section>
  {:else if step.kind === "atlas_running"}
    <section class="completion">
      <h3 class="completion-title">Building atlas.</h3>
      {#if step.initError}
        <p class="completion-skipped">{step.initError}</p>
      {/if}
      <EnrichPollProgress
        corpusId={config.id}
        label="Atlas pipeline"
        onComplete={() => void handleAtlasComplete()}
        onFailed={(r) => handleAtlasFailed(r)}
      />
      <div class="atlas-offer-actions" style="margin-top: 12px;">
        <button class="lk-btn lk-btn--quiet" onclick={closeReview}>
          Close (atlas keeps running)
        </button>
      </div>
    </section>
  {:else if step.kind === "atlas_complete"}
    <section class="completion">
      <h3 class="completion-title">Atlas ready.</h3>
      {#if step.starters.length > 0}
        <div style="margin: 14px 0;">
          <StarterChips
            questions={step.starters}
            onPick={handleStarterPick}
            heading="Try asking"
          />
        </div>
      {:else}
        <p class="completion-hint">
          No starter questions yet — the chat empty state will show new
          suggestions as the atlas indexes more of your vault.
        </p>
      {/if}
      <button class="lk-btn lk-btn--quiet" onclick={closeReview}>Close</button>
    </section>
  {:else if step.kind === "error"}
    <section class="error-panel">
      <p class="lk-label error-label">Failed</p>
      <p class="error-body">{step.message}</p>
      <button
        class="lk-btn lk-btn--quiet"
        onclick={() => (step = { kind: "idle" })}
      >
        Try again
      </button>
    </section>
  {/if}

  {#key dangerZoneReloadKey}
    <div class="register-wrap">
      <hr class="lk-rule-h lk-rule-h--heavy register-rule" />
      <p class="lk-label register-label">Restore</p>
      <CorpusDangerZone corpusId={config.id} onReset={() => (dangerZoneReloadKey += 1)} />
    </div>
  {/key}
</div>

<style>
  .organizer {
    padding: 16px 0 8px;
  }

  /* ── Atlas offer (post-writeback) ───────────────────── */
  .atlas-offer {
    margin-top: 16px;
    padding-top: 14px;
    border-top: 1px solid var(--lk-rule, #333);
  }
  .atlas-offer-title {
    margin: 0 0 4px;
    font-size: var(--lk-size-lead);
    font-weight: 600;
    color: var(--lk-ink);
  }
  .atlas-offer-desc {
    margin: 0 0 10px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    line-height: 1.5;
  }
  .atlas-offer-actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .prompt {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 20px;
    padding: 18px 20px;
    align-items: center;
    border: 1px solid var(--lk-rule);
    background: var(--lk-paper-subtle);
    border-radius: var(--radius);
  }
  .prompt-title {
    margin: 0 0 4px;
    font-size: var(--lk-size-display);
    font-weight: 600;
    line-height: 1.2;
    color: var(--lk-ink);
    letter-spacing: -0.01em;
  }
  .prompt-desc {
    margin: 0;
    max-width: 58ch;
    font-size: var(--lk-size-body);
    color: var(--lk-ink-soft);
    line-height: 1.5;
  }

  .loading {
    padding: 24px 0;
    font-size: var(--lk-size-body);
    color: var(--lk-ink-soft);
  }

  .completion {
    padding: 16px 20px;
    border: 1px solid var(--lk-crown);
    background: var(--lk-crown-wash);
    border-left: 3px solid var(--lk-crown);
    border-radius: var(--radius);
  }
  .completion-title {
    margin: 0 0 10px;
    font-size: var(--lk-size-lead);
    font-weight: 500;
    color: var(--lk-ink);
    line-height: 1.4;
  }
  .completion-title .lk-num {
    color: var(--lk-stamp-ink);
    font-size: 1.125em;
    margin: 0 2px;
  }
  .completion-skipped,
  .completion-hint {
    margin: 0 0 10px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    max-width: 58ch;
  }

  .error-panel {
    padding: 16px 20px;
    border: 1px solid var(--lk-err);
    background: var(--lk-err-wash);
    border-radius: var(--radius);
  }
  .error-label { color: var(--lk-err); }
  .error-body {
    margin: 8px 0 14px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink);
    line-height: 1.5;
  }

  .register-wrap {
    margin-top: 28px;
  }
  .register-rule { margin-bottom: 8px; }
  .register-label { margin: 0 0 12px; }
</style>
