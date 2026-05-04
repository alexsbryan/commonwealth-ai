<script lang="ts">
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";

  import { lcIncompleteJobs, lcList, lcRemove, lcWatchList } from "../../api";
  import type {
    IncompleteJob,
    LocalCorpusConfig,
    StarterQuestion,
    WatchedFolderListEntry,
  } from "../../types";

  import FolderDropFlow from "./folder/FolderDropFlow.svelte";
  import LocalKnowledgeAdd from "./LocalKnowledgeAdd.svelte";
  import LocalKnowledgeList from "./LocalKnowledgeList.svelte";
  import ResumePrompt from "./ResumePrompt.svelte";
  import WatchedFolderRegisterFlow from "./WatchedFolderRegisterFlow.svelte";
  import WatchedFolderList from "./WatchedFolderList.svelte";
  import WatchedFolderBanner from "./WatchedFolderBanner.svelte";
  import WatchedFolderDetail from "./WatchedFolderDetail.svelte";

  interface Props {
    /// Pipe-through from SettingsPanel → App.svelte: when a user
    /// clicks a starter chip on the atlas-complete screen, fire so
    /// the chat view opens with the question seeded + auto-submitted.
    onOpenChatWithSeed?: (question: StarterQuestion) => void;
    /// Pipe-through: "Start chatting — atlas keeps building" button
    /// on the sample-atlas progress screen. App closes Settings + the
    /// toast fires when the atlas finishes.
    onDropToChat?: () => void;
  }
  let { onOpenChatWithSeed, onDropToChat }: Props = $props();

  import "./_theme.css";

  type Mode =
    | { kind: "idle" }
    | {
        kind: "folder-flow";
        initialPath: string | null;
        sourceType: "folder" | "obsidian";
        resumeCorpusId?: string | null;
        resumeDisplayName?: string | null;
      }
    | { kind: "watched-folder-register" }
    | { kind: "watched-folder-detail"; corpusId: string };

  let corpora = $state<LocalCorpusConfig[]>([]);
  let incomplete = $state<IncompleteJob[]>([]);
  let watchedCorpora = $state<WatchedFolderListEntry[]>([]);
  let mode: Mode = $state({ kind: "idle" });
  let loadError = $state<string | null>(null);
  let unlistenDrop: UnlistenFn | null = null;

  // Periodic refresh while idle so a sweep state change (Idle →
  // Sweeping → Idle, or Idle → PausedAwaitingConfirmation) shows up
  // without the user navigating away. 5s cadence matches the
  // scheduler's dispatch interval — fast enough to feel live, slow
  // enough not to thrash the daemon.
  let pollHandle: ReturnType<typeof setInterval> | null = null;

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
    if (pollHandle) clearInterval(pollHandle);
  });

  async function reload() {
    try {
      loadError = null;
      const [c, i, w] = await Promise.all([
        lcList(),
        lcIncompleteJobs(),
        lcWatchList().catch(() => ({ corpora: [] })),
      ]);
      corpora = c;
      incomplete = i;
      watchedCorpora = w.corpora;
    } catch (e) {
      loadError = String(e);
      corpora = [];
      incomplete = [];
      watchedCorpora = [];
    }
  }

  // Start polling once mounted; stop when the section unmounts.
  $effect(() => {
    if (mode.kind === "idle" && !pollHandle) {
      pollHandle = setInterval(() => {
        // Soft refresh — don't spread errors into loadError.
        lcWatchList()
          .then((w) => {
            watchedCorpora = w.corpora;
          })
          .catch(() => {});
      }, 5_000);
    } else if (mode.kind !== "idle" && pollHandle) {
      clearInterval(pollHandle);
      pollHandle = null;
    }
  });

  let blockedWatched = $derived(
    watchedCorpora.filter(
      (e) =>
        e.status.kind === "paused_awaiting_confirmation" ||
        e.status.kind === "errored",
    ),
  );

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

  function enterWatchedFolderFlow() {
    mode = { kind: "watched-folder-register" };
  }

  async function exitFlow() {
    mode = { kind: "idle" };
    await reload();
  }
</script>

<div class="lk lk-section">
  {#if mode.kind === "idle"}
    <header class="head">
      <h1 class="title">Local knowledge</h1>
      <p class="lede">
        Folders and vaults on this machine. Indexed here, searched here,
        never uploaded.
      </p>
    </header>
  {/if}

  {#if loadError}
    <p class="load-error">{loadError}</p>
  {/if}

  {#if mode.kind === "idle"}
    <WatchedFolderBanner blocked={blockedWatched} onChanged={reload} />

    <ResumePrompt
      jobs={incomplete}
      onResume={(id) => {
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

    <section class="plate">
      <div class="plate-head">
        <p class="lk-label">Sources</p>
        <span class="plate-count lk-folio">{corpora.length}</span>
      </div>
      <LocalKnowledgeList {corpora} onRemove={handleRemove} {onOpenChatWithSeed} />
    </section>

    {#if watchedCorpora.length > 0}
      <section class="plate">
        <div class="plate-head">
          <p class="lk-label">Watched folders</p>
          <span class="plate-count lk-folio">{watchedCorpora.length}</span>
        </div>
        <WatchedFolderList
          corpora={watchedCorpora}
          onChanged={reload}
          onOpenDetail={(corpusId) =>
            (mode = { kind: "watched-folder-detail", corpusId })}
        />
      </section>
    {/if}

    <section class="plate">
      <div class="plate-head">
        <p class="lk-label">Add</p>
      </div>
      <LocalKnowledgeAdd
        onPickFolder={enterFolderFlow}
        onPickObsidian={enterObsidianFlow}
        onPickWatchedFolder={enterWatchedFolderFlow}
      />
    </section>
  {:else if mode.kind === "folder-flow"}
    <FolderDropFlow
      initialPath={mode.initialPath}
      sourceType={mode.sourceType}
      resumeCorpusId={mode.resumeCorpusId ?? null}
      resumeDisplayName={mode.resumeDisplayName ?? null}
      onExit={exitFlow}
      {onOpenChatWithSeed}
      {onDropToChat}
    />
  {:else if mode.kind === "watched-folder-register"}
    <WatchedFolderRegisterFlow
      onCancel={exitFlow}
      onRegistered={() => exitFlow()}
    />
  {:else if mode.kind === "watched-folder-detail"}
    <WatchedFolderDetail
      corpusId={mode.corpusId}
      onClose={exitFlow}
    />
  {/if}
</div>

<style>
  .lk-section {
    padding: 28px 32px 44px;
    position: relative;
  }

  .head {
    margin-bottom: 28px;
    animation: lk-fade-in 300ms ease-out both;
  }
  .title {
    margin: 0 0 6px;
    font-family: var(--lk-font-display);
    font-size: var(--lk-size-hero);
    font-weight: 600;
    line-height: 1.1;
    letter-spacing: -0.02em;
    color: var(--lk-ink);
  }
  .lede {
    margin: 0;
    max-width: 64ch;
    font-size: var(--lk-size-body);
    color: var(--lk-ink-soft);
    line-height: 1.5;
  }

  .plate {
    margin-top: 28px;
    animation: lk-fade-in 400ms ease-out both;
    animation-delay: 80ms;
  }
  .plate + .plate { animation-delay: 140ms; }
  .plate-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin-bottom: 12px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--lk-rule);
  }
  .plate-count {
    color: var(--lk-ink-faded);
  }

  .load-error {
    margin: 0 0 20px;
    padding: 10px 14px;
    border-left: 3px solid var(--lk-err);
    background: var(--lk-err-wash);
    color: var(--lk-ink);
    font-size: var(--lk-size-meta);
  }
</style>
