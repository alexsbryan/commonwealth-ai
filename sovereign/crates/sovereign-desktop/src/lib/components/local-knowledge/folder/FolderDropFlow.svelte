<script lang="ts">
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy } from "svelte";

  import { lcCancel, lcIngest, lcPreScan } from "../../../api";
  import type {
    IngestStats,
    LocalCorpusProgress,
    PreScanResult,
  } from "../../../types";

  import FolderSelectPanel from "./FolderSelectPanel.svelte";
  import PreScanPanel from "./PreScanPanel.svelte";
  import FolderCompletePanel from "./FolderCompletePanel.svelte";
  import IngestProgressPanel from "../IngestProgressPanel.svelte";

  type Step =
    | { kind: "select"; initialPath?: string }
    | {
        kind: "scanning";
        path: string;
      }
    | {
        kind: "confirm";
        path: string;
        corpusId: string;
        displayName: string;
        result: PreScanResult;
      }
    | {
        kind: "ingesting";
        corpusId: string;
        displayName: string;
        progress: LocalCorpusProgress | null;
      }
    | { kind: "complete"; stats: IngestStats }
    | { kind: "error"; message: string };

  interface Props {
    /// Optional initial path — populated when the flow was entered via
    /// a drag-and-drop event on the settings page.
    initialPath?: string | null;
    /// `"folder"` (PDFs + TXT) or `"obsidian"` (markdown vault). Drives
    /// which `LocalCorpusConfig` factory the backend applies and which
    /// pre-scan rules run.
    sourceType?: "folder" | "obsidian";
    /// When set, skip the select + scan + confirm steps and go
    /// straight to ingesting. Used by the resume-on-relaunch prompt:
    /// the corpus is already registered, the engine has a partial
    /// checkpoint, we just need to re-invoke ingest and subscribe to
    /// progress.
    resumeCorpusId?: string | null;
    /// Optional display name used in the "Resuming …" label.
    resumeDisplayName?: string | null;
    onExit: () => void;
  }

  let {
    initialPath = null,
    sourceType = "folder",
    resumeCorpusId = null,
    resumeDisplayName = null,
    onExit,
  }: Props = $props();

  let step: Step = $state({ kind: "select", initialPath: initialPath ?? undefined });
  let unlisten: UnlistenFn | null = null;
  let cancelling = $state(false);

  onDestroy(() => {
    if (unlisten) unlisten();
  });

  // Resume-on-relaunch: if a caller passes `resumeCorpusId`, skip
  // directly into ingesting. The engine's source-file manifest
  // causes the re-invocation to pick up from the last completed
  // shard — no separate resume API needed.
  $effect(() => {
    if (resumeCorpusId && step.kind === "select") {
      const id = resumeCorpusId;
      const name = resumeDisplayName ?? id;
      step = {
        kind: "ingesting",
        corpusId: id,
        displayName: name,
        progress: null,
      };
      void kickOffIngest(id);
    }
  });

  async function handleSelected(path: string) {
    step = { kind: "scanning", path };
    try {
      const response = await lcPreScan(path, sourceType);
      step = {
        kind: "confirm",
        path,
        corpusId: response.corpus_id,
        displayName: response.display_name,
        result: response.result,
      };
    } catch (e: unknown) {
      step = { kind: "error", message: `Pre-scan failed: ${e}` };
    }
  }

  async function handleConfirmIngest() {
    if (step.kind !== "confirm") return;
    const { corpusId, displayName } = step;
    step = {
      kind: "ingesting",
      corpusId,
      displayName,
      progress: null,
    };
    await kickOffIngest(corpusId);
  }

  async function kickOffIngest(corpusId: string) {
    try {
      const jobId = await lcIngest(corpusId);
      const channel = `local-corpus://progress/${jobId}`;
      unlisten = await listen<LocalCorpusProgress>(channel, (event) => {
        if (step.kind === "ingesting") {
          step = { ...step, progress: event.payload };
        }
        if (event.payload.phase === "complete") {
          step = { kind: "complete", stats: event.payload.data.result };
          if (unlisten) {
            unlisten();
            unlisten = null;
          }
        }
        if (event.payload.phase === "error") {
          step = {
            kind: "error",
            message: event.payload.data.message,
          };
          if (unlisten) {
            unlisten();
            unlisten = null;
          }
        }
      });
    } catch (e: unknown) {
      step = { kind: "error", message: `Ingest failed to start: ${e}` };
    }
  }

  async function handleCancel() {
    if (step.kind !== "ingesting") return;
    cancelling = true;
    try {
      await lcCancel(step.corpusId);
      // The engine's ingest loop will emit its own terminal event
      // (usually an `error` with a cancellation message). We keep the
      // current state until that arrives; the Cancel button flips to
      // "Cancelling…" so the user knows the signal went out.
    } catch (e) {
      cancelling = false;
      window.alert(`Cancel failed: ${e}`);
    }
  }

  function handleChooseAgain() {
    step = { kind: "select" };
  }
</script>

<div class="folder-drop-flow">
  {#if step.kind === "select"}
    <FolderSelectPanel
      initialPath={step.initialPath ?? null}
      onSelected={handleSelected}
      onCancel={onExit}
    />
  {:else if step.kind === "scanning"}
    <p class="scanning-note">Scanning {step.path}…</p>
  {:else if step.kind === "confirm"}
    <PreScanPanel
      result={step.result}
      onConfirm={handleConfirmIngest}
      onChooseAgain={handleChooseAgain}
    />
  {:else if step.kind === "ingesting"}
    <IngestProgressPanel progress={step.progress} />
    <div class="ingest-actions">
      <button
        class="btn-secondary"
        onclick={handleCancel}
        disabled={cancelling}
      >
        {cancelling ? "Cancelling…" : "Cancel"}
      </button>
    </div>
  {:else if step.kind === "complete"}
    <FolderCompletePanel stats={step.stats} onDone={onExit} />
  {:else if step.kind === "error"}
    <div class="error-panel">
      <p class="error-title">Something went wrong</p>
      <p class="error-body">{step.message}</p>
      <button class="btn-secondary" onclick={handleChooseAgain}>Try again</button>
    </div>
  {/if}
</div>

<style>
  .folder-drop-flow {
    padding: 0;
  }
  .scanning-note {
    padding: 20px 0;
    font-size: 14px;
    color: var(--color-text-muted, #6b6b6b);
  }
  .error-panel {
    padding: 20px 0;
  }
  .error-title {
    font-weight: 500;
    color: var(--color-error, #c92a2a);
    margin: 0 0 8px;
  }
  .error-body {
    font-size: 13px;
    margin: 0 0 16px;
  }
  .btn-secondary {
    padding: 8px 16px;
    border-radius: 6px;
    font-size: 14px;
    cursor: pointer;
    border: 1px solid var(--color-border, #d4d4d4);
    background: transparent;
    color: var(--color-text, #1a1a1a);
  }
  .btn-secondary:hover:not(:disabled) {
    background: var(--color-surface-subtle, #f4f4f4);
  }
  .btn-secondary:disabled {
    opacity: 0.6;
    cursor: wait;
  }
  .ingest-actions {
    margin-top: 12px;
    display: flex;
    justify-content: flex-end;
  }
</style>
