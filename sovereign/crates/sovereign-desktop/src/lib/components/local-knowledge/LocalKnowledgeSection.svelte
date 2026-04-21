<script lang="ts">
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";

  import { lcIncompleteJobs, lcList, lcRemove } from "../../api";
  import type { IncompleteJob, LocalCorpusConfig } from "../../types";

  import FolderDropFlow from "./folder/FolderDropFlow.svelte";
  import LocalKnowledgeAdd from "./LocalKnowledgeAdd.svelte";
  import LocalKnowledgeList from "./LocalKnowledgeList.svelte";
  import ResumePrompt from "./ResumePrompt.svelte";

  type Mode =
    | { kind: "idle" }
    | {
        kind: "folder-flow";
        initialPath: string | null;
        sourceType: "folder" | "obsidian";
        resumeCorpusId?: string | null;
        resumeDisplayName?: string | null;
      };

  let corpora = $state<LocalCorpusConfig[]>([]);
  let incomplete = $state<IncompleteJob[]>([]);
  let mode: Mode = $state({ kind: "idle" });
  let loadError = $state<string | null>(null);
  let unlistenDrop: UnlistenFn | null = null;

  onMount(async () => {
    await reload();
    try {
      // Tauri 2 emits window.drop when folders are dropped on the
      // app. Payload shape: { paths: string[], position: {x, y} }.
      unlistenDrop = await listen<{ paths: string[] }>(
        "tauri://drag-drop",
        (event) => {
          const firstDir = event.payload?.paths?.[0];
          if (firstDir) {
            // Drag-drop always enters the folder flow; if the user
            // dropped a vault they can switch via the "Connect
            // Obsidian vault" tile afterwards (the current flow is
            // file-type aware via the folder config's extension list).
            mode = {
              kind: "folder-flow",
              initialPath: firstDir,
              sourceType: "folder",
            };
          }
        },
      );
    } catch {
      // File-drop unsupported on this platform — silently ignore.
    }
  });

  onDestroy(() => {
    if (unlistenDrop) unlistenDrop();
  });

  async function reload() {
    try {
      loadError = null;
      [corpora, incomplete] = await Promise.all([
        lcList(),
        lcIncompleteJobs(),
      ]);
    } catch (e) {
      loadError = String(e);
      corpora = [];
      incomplete = [];
    }
  }

  async function handleRemove(id: string) {
    if (
      !window.confirm(
        "Remove this knowledge source? The index will be deleted; the original files are not touched.",
      )
    )
      return;
    try {
      await lcRemove(id);
      await reload();
    } catch (e) {
      window.alert(`Could not remove: ${e}`);
    }
  }

  function handleDiscardIncomplete(id: string) {
    // Discard means "drop the partial index". Same code path as remove.
    handleRemove(id);
  }

  function enterFolderFlow() {
    mode = { kind: "folder-flow", initialPath: null, sourceType: "folder" };
  }

  function enterObsidianFlow() {
    mode = { kind: "folder-flow", initialPath: null, sourceType: "obsidian" };
  }

  async function exitFlow() {
    mode = { kind: "idle" };
    await reload();
  }
</script>

<div class="section">
  {#if loadError}
    <p class="load-error">
      Could not load local knowledge: {loadError}
    </p>
  {/if}

  {#if mode.kind === "idle"}
    <ResumePrompt
      jobs={incomplete}
      onResume={(id) => {
        // Resume by re-invoking ingest. The engine's source-file
        // manifest causes the new run to pick up from the last
        // completed shard — no separate "resume" API needed. We
        // route the user through `FolderDropFlow` in resume mode so
        // they see progress instead of a silent background run.
        const job = incomplete.find((j) => j.corpus_id === id);
        const sourceTypeGuess =
          corpora.find((c) => c.id === id)?.source_type ===
          "DocumentFolder"
            ? ("folder" as const)
            : ("obsidian" as const);
        mode = {
          kind: "folder-flow",
          initialPath: null,
          sourceType: sourceTypeGuess,
          resumeCorpusId: id,
          resumeDisplayName: job?.display_name ?? null,
        };
      }}
      onDiscard={handleDiscardIncomplete}
    />

    <p class="section-label">Your local knowledge</p>
    <LocalKnowledgeList {corpora} onRemove={handleRemove} />

    <LocalKnowledgeAdd
      onPickFolder={enterFolderFlow}
      onPickObsidian={enterObsidianFlow}
    />
  {:else if mode.kind === "folder-flow"}
    <FolderDropFlow
      initialPath={mode.initialPath}
      sourceType={mode.sourceType}
      resumeCorpusId={mode.resumeCorpusId ?? null}
      resumeDisplayName={mode.resumeDisplayName ?? null}
      onExit={exitFlow}
    />
  {/if}
</div>

<style>
  .section {
    padding: 8px 0;
  }
  .section-label {
    font-size: 13px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.02em;
    color: var(--color-text-muted, #6b6b6b);
    margin: 0 0 10px;
  }
  .load-error {
    padding: 8px 12px;
    background: color-mix(in srgb, var(--color-error, #c92a2a) 10%, transparent);
    border-radius: 4px;
    font-size: 13px;
    color: var(--color-error, #c92a2a);
  }
</style>
