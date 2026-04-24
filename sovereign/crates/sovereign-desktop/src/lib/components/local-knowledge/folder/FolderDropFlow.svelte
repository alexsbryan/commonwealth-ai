<script lang="ts">
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy } from "svelte";

  import {
    enrichBuildAsync,
    enrichCancelBuild,
    enrichGetStarterQuestions,
    enrichInitForLocalCorpus,
    lcCancel,
    lcIngest,
    lcPreScan,
  } from "../../../api";
  import type {
    IngestStats,
    LocalCorpusProgress,
    PreScanResult,
    SampledDocuments,
    StarterQuestion,
  } from "../../../types";
  import { enrichProgressStore } from "../../../stores/enrichProgress.svelte";
  import { chatSeedStore } from "../../../stores/chatSeed.svelte";
  import { notifyReadyToAsk } from "../../../stores/toast.svelte";

  import FolderSelectPanel from "./FolderSelectPanel.svelte";
  import PreScanPanel from "./PreScanPanel.svelte";
  import FolderCompletePanel from "./FolderCompletePanel.svelte";
  import IngestProgressPanel from "../IngestProgressPanel.svelte";
  import EnrichmentStage from "../../EnrichmentStage.svelte";
  import StarterChips from "../../StarterChips.svelte";
  import InkStamp from "../../onboarding/InkStamp.svelte";
  import ProgressRule from "../../onboarding/ProgressRule.svelte";
  import RotatingMessage from "../../onboarding/RotatingMessage.svelte";
  import { cleanExcerptTitle } from "../../../onboarding/excerpt_helpers";

  /// Time-to-first-value dial. 5 docs typically lands the sample
  /// atlas in 2–3 min on an M2 Max. Remaining docs stay searchable
  /// (full-text + semantic) without atlas dependency.
  const SAMPLE_SIZE = 5;

  /// Default pipeline for the sample atlas. Literary is more
  /// forgiving on heterogeneous prose (notes, mixed PDFs, etc.).
  /// Advanced users can still pin philosophy_atlas from the CLI or
  /// the Enrichment Settings tab.
  const DEFAULT_PIPELINE = "literary_atlas" as const;

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
    | {
        /// Ingest has just completed; we're calling
        /// `enrich_init_for_local_corpus` to scaffold the atlas
        /// config before spawning the build subprocess. Usually
        /// sub-second; if it fails we transition to `complete` so
        /// the user keeps the ingest-only surface.
        kind: "initializing_atlas";
        corpusId: string;
        displayName: string;
        stats: IngestStats;
      }
    | {
        kind: "enriching";
        corpusId: string;
        displayName: string;
        stats: IngestStats;
        sampled: SampledDocuments;
        /// job_id from enrichBuildAsync. `null` until the spawn
        /// callback returns.
        jobId: string | null;
        /// Fatal-ish error from init or spawn. Non-null falls through
        /// to a "continue without atlas" affordance.
        spawnError: string | null;
      }
    | {
        kind: "atlas_complete";
        corpusId: string;
        displayName: string;
        stats: IngestStats;
        sampled: SampledDocuments;
        starters: StarterQuestion[];
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
    /// When the user clicks a starter chip on the atlas-complete
    /// screen, fire so the chat view opens with the question
    /// pre-filled and auto-submitted. Also fired via the toast's
    /// "Ask a question" action when a seed is supplied.
    onOpenChatWithSeed?: (question: StarterQuestion) => void;
    /// When present, allows the user to drop out to chat during the
    /// atlas build. The chat empty state picks up starter chips as
    /// soon as the atlas finishes in the background.
    onDropToChat?: () => void;
  }

  let {
    initialPath = null,
    sourceType = "folder",
    resumeCorpusId = null,
    resumeDisplayName = null,
    onExit,
    onOpenChatWithSeed,
    onDropToChat,
  }: Props = $props();

  let step: Step = $state({ kind: "select", initialPath: initialPath ?? undefined });
  let unlisten: UnlistenFn | null = null;
  let cancelling = $state(false);

  onDestroy(() => {
    if (unlisten) unlisten();
    // If the user closes the flow mid-enrichment, DO NOT cancel the
    // subprocess — we want it to keep running in the background.
    // Cancellation is only appropriate when the user explicitly asks.
    // (Previous behaviour cancelled on unmount; that contradicted the
    // "chat empty state upgrades chips when atlas finishes" design.)
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
          void handleIngestComplete(event.payload.data.result);
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

  /// After ingest: auto-kick the sample atlas build. No gate screen,
  /// no "do you want to?" question — we just get the user to value
  /// in a few minutes.
  async function handleIngestComplete(stats: IngestStats) {
    if (step.kind !== "ingesting") {
      // UI may have navigated away; land in `complete` so we don't
      // clobber a user-initiated transition.
      step = { kind: "complete", stats };
      return;
    }
    const { corpusId, displayName } = step;
    step = { kind: "initializing_atlas", corpusId, displayName, stats };
    try {
      const sampled = await enrichInitForLocalCorpus(
        corpusId,
        DEFAULT_PIPELINE,
        SAMPLE_SIZE,
      );
      const handle = await enrichBuildAsync(corpusId, null, null);
      await enrichProgressStore.track(handle);
      step = {
        kind: "enriching",
        corpusId,
        displayName,
        stats,
        sampled,
        jobId: handle.job_id,
        spawnError: null,
      };
    } catch (e: unknown) {
      // Init or spawn failed. Fall through to the ingest-only
      // completion screen — search still works, just without atlas.
      console.warn("auto enrich init/spawn failed:", e);
      step = { kind: "complete", stats };
    }
  }

  async function handleCancel() {
    if (step.kind !== "ingesting") return;
    cancelling = true;
    try {
      await lcCancel(step.corpusId);
    } catch (e) {
      cancelling = false;
      window.alert(`Cancel failed: ${e}`);
    }
  }

  async function cancelAtlasBuild() {
    if (step.kind !== "enriching" || !step.jobId) return;
    try {
      await enrichCancelBuild(step.jobId);
    } catch (e) {
      console.warn("cancel atlas failed:", e);
    }
    // Terminal event will flip the Stage into cancelled; user can
    // then hit "Continue without atlas".
  }

  async function handleAtlasTerminal(kind: string) {
    if (step.kind !== "enriching") return;
    if (kind === "complete") {
      const { corpusId, displayName, stats, sampled } = step;
      let starters: StarterQuestion[] = [];
      try {
        starters = await enrichGetStarterQuestions(corpusId, 5);
      } catch (e) {
        console.warn("enrichGetStarterQuestions failed:", e);
      }
      step = {
        kind: "atlas_complete",
        corpusId,
        displayName,
        stats,
        sampled,
        starters,
      };
      // Fire a global toast so the user gets the news even if they
      // navigated to chat while the build was running. Route the
      // "Ask a question" action through `chatSeedStore` — a stable
      // module-level store that doesn't depend on this component
      // still being mounted when the toast fires.
      notifyReadyToAsk({
        corpusId,
        titles: sampled.titles,
        total: sampled.total,
        firstStarter: starters[0] ?? null,
        onAsk: (question) => chatSeedStore.set(question),
      });
    }
    // `cancelled` and `aborted`/`spawn_failed` stay on the enriching
    // screen so the user can read the terminal message + retry.
  }

  function continueWithoutAtlas() {
    if (step.kind !== "enriching") return;
    // Leave the subprocess running; the chat empty state will pick
    // up starter chips when it finishes. Toast will fire then too.
    step = { kind: "complete", stats: step.stats };
  }

  function dropToChatNow() {
    // Fires only when the host wired `onDropToChat`. Atlas keeps
    // running; chat empty state upgrades on completion via the
    // progress store.
    onDropToChat?.();
  }

  function handleStarterPick(question: StarterQuestion) {
    if (!onOpenChatWithSeed) {
      onExit();
      return;
    }
    onOpenChatWithSeed(question);
  }

  function handleChooseAgain() {
    step = { kind: "select" };
  }

  // Live look-up of the progress state for the active job, keyed
  // by whichever jobId this flow started. Kept reachable across
  // `enriching` → `complete` transitions so FolderCompletePanel
  // can note "atlas is still building" when the user skipped
  // without cancelling.
  let currentJobId = $derived.by(() => {
    if (step.kind === "enriching") return step.jobId;
    return null;
  });
  let activeEnrichJob = $derived.by(() => {
    if (!currentJobId) return null;
    return enrichProgressStore.get(currentJobId) ?? null;
  });
  /// True when a build is still streaming for this corpus on the
  /// shared progress store — works even after we've transitioned
  /// past the `enriching` screen (e.g., user hit "Continue without
  /// atlas" while the subprocess is still running).
  let atlasStillRunning = $derived.by(() => {
    let corpusId: string | undefined;
    if (step.kind === "enriching" || step.kind === "initializing_atlas") {
      corpusId = step.corpusId;
    } else if (step.kind === "atlas_complete") {
      corpusId = step.corpusId;
    } else if (step.kind === "complete") {
      corpusId = step.stats.corpus_id;
    }
    if (!corpusId) return false;
    return enrichProgressStore
      .byCorpus(corpusId)
      .some((j) => !j.terminal);
  });

  /// Human-friendly list: "A, B, and 3 more" / "A and B" / "A".
  /// Titles are cleaned through `cleanExcerptTitle` so numeric
  /// filename prefixes and trailing years don't surface in the UI.
  function formatTitleList(titles: string[]): string {
    const cleaned = titles.map(cleanExcerptTitle).filter((t) => t);
    if (cleaned.length === 0) return "";
    if (cleaned.length === 1) return cleaned[0];
    if (cleaned.length === 2) return `${cleaned[0]} and ${cleaned[1]}`;
    if (cleaned.length === 3)
      return `${cleaned[0]}, ${cleaned[1]}, and ${cleaned[2]}`;
    return `${cleaned[0]}, ${cleaned[1]}, and ${cleaned.length - 2} more`;
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
    <section class="working">
      <header class="working-head">
        <InkStamp size="md" active={true} />
        <h1 class="working-title">Scanning.</h1>
      </header>
      <p class="working-path" title={step.path}>{step.path}</p>
      <ProgressRule label="Reading folder" />
      <p class="working-foot">
        <RotatingMessage
          messages={[
            "Walking the folder tree…",
            "Classifying files…",
            "Counting readable pages…",
          ]}
        />
      </p>
    </section>
  {:else if step.kind === "confirm"}
    <PreScanPanel
      result={step.result}
      onConfirm={handleConfirmIngest}
      onChooseAgain={handleChooseAgain}
    />
  {:else if step.kind === "ingesting"}
    <IngestProgressPanel progress={step.progress} />
    <div class="working-actions">
      <button
        class="lk-btn lk-btn--quiet"
        onclick={handleCancel}
        disabled={cancelling}
      >
        {cancelling ? "Cancelling…" : "Cancel"}
      </button>
    </div>
  {:else if step.kind === "initializing_atlas"}
    <section class="working">
      <header class="working-head">
        <InkStamp size="md" active={true} />
        <h1 class="working-title">Priming the atlas.</h1>
      </header>
      <p class="working-sub">
        Indexed <strong>{step.stats.files_indexed}</strong>
        document{step.stats.files_indexed === 1 ? "" : "s"}. Selecting a
        starter sample so you can begin asking in a couple of minutes.
      </p>
      <ProgressRule label="Preparing" />
      <p class="working-foot">
        <RotatingMessage
          messages={[
            "Selecting a sample…",
            "Writing the synthetic source…",
            "Calling the pipeline…",
            "Spawning the build…",
          ]}
        />
      </p>
    </section>
  {:else if step.kind === "enriching"}
    <section class="working">
      <header class="working-head">
        <InkStamp size="md" active={true} />
        <h1 class="working-title">Building atlas.</h1>
      </header>
      <p class="working-sub">
        Extracting entities, events, and claims across
        <strong>{formatTitleList(step.sampled.titles)}</strong>.
        {#if step.sampled.total > step.sampled.titles.length}
          <span class="sub-muted">
            {step.sampled.titles.length} of {step.sampled.total}
            documents — we'll start with these so you can ask sooner.
          </span>
        {/if}
      </p>
      {#if step.spawnError}
        <p class="err-msg">{step.spawnError}</p>
      {/if}
      <EnrichmentStage
        job={activeEnrichJob}
        label="Atlas pipeline"
        hideCancel={true}
        onTerminal={(kind) => void handleAtlasTerminal(kind)}
      />
      <div class="working-actions">
        {#if onDropToChat}
          <button class="lk-btn lk-btn--mark" onclick={dropToChatNow}>
            Start chatting — atlas keeps building
          </button>
        {/if}
        <button class="lk-btn lk-btn--quiet" onclick={cancelAtlasBuild}>
          Cancel atlas
        </button>
        <button class="lk-btn lk-btn--quiet" onclick={continueWithoutAtlas}>
          Skip atlas
        </button>
      </div>
    </section>
  {:else if step.kind === "atlas_complete"}
    <section class="atlas-complete">
      <header class="head">
        <h1 class="title">Ready to ask.</h1>
        <p class="count">
          Atlas covers <strong>{formatTitleList(step.sampled.titles)}</strong>.
          {#if step.sampled.total > step.sampled.titles.length}
            <span class="sub-muted">
              ({step.sampled.titles.length} of {step.sampled.total}
              documents — rest remain searchable.)
            </span>
          {/if}
        </p>
        <p class="invitation">What connections can we make?</p>
      </header>

      {#if step.starters.length > 0}
        <section class="starter-zone">
          <StarterChips
            questions={step.starters}
            onPick={handleStarterPick}
            heading="Try asking"
          />
        </section>
      {:else if step.stats.excerpt_chunks.length > 0}
        <section class="excerpts">
          <p class="lk-label excerpts-label">A sample of what was indexed</p>
          <ol class="excerpt-list">
            {#each step.stats.excerpt_chunks.slice(0, 3) as chunk}
              <li class="excerpt">
                <p class="excerpt-body">{chunk.text}</p>
                <p class="excerpt-source">
                  — {chunk.source_name}{#if chunk.page_ref}, {chunk.page_ref}{/if}
                </p>
              </li>
            {/each}
          </ol>
        </section>
      {/if}

      <div class="actions">
        <button class="lk-btn lk-btn--mark" onclick={onExit}>Done</button>
      </div>
    </section>
  {:else if step.kind === "complete"}
    <FolderCompletePanel
      stats={step.stats}
      onDone={onExit}
      onStartChat={handleStarterPick}
      atlasStillBuilding={atlasStillRunning}
    />
  {:else if step.kind === "error"}
    <section class="error-panel">
      <p class="lk-label error-label">Failed</p>
      <p class="error-body">{step.message}</p>
      <button class="lk-btn lk-btn--quiet" onclick={handleChooseAgain}>
        Try again
      </button>
    </section>
  {/if}
</div>

<style>
  .folder-drop-flow { padding: 0; color: var(--lk-ink); }

  /* ── "Working" states: scanning, initializing, enriching ── */
  .working {
    max-width: 720px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    animation: lk-fade-in 320ms ease-out both;
    padding-top: 8px;
  }
  .working-head {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .working-title {
    margin: 0;
    font-family: var(--font-serif);
    font-style: italic;
    font-weight: 500;
    font-size: 1.75rem;
    line-height: 1.1;
    letter-spacing: -0.005em;
    color: var(--lk-ink);
  }
  .working-sub {
    margin: 0;
    font-size: 0.94rem;
    color: var(--lk-ink-soft);
    line-height: 1.55;
    max-width: 62ch;
  }
  .working-sub strong {
    color: var(--lk-ink);
    font-weight: 600;
  }
  .working-path {
    margin: -4px 0 0;
    font-family: var(--font-mono);
    font-size: 0.76rem;
    color: var(--lk-ink-faded);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 100%;
  }
  .working-foot {
    margin: -2px 0 0;
    color: var(--lk-ink-faded);
  }
  .sub-muted {
    color: var(--lk-ink-faded);
    display: inline;
  }
  .working-actions {
    display: flex;
    gap: 10px;
    justify-content: flex-end;
    flex-wrap: wrap;
    margin-top: 16px;
  }

  .error-panel {
    padding: 16px 20px;
    border: 1px solid var(--lk-err);
    background: var(--lk-err-wash);
    border-radius: var(--radius);
    color: var(--lk-ink);
  }
  .error-label {
    color: var(--lk-err);
  }
  .error-body {
    margin: 8px 0 14px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink);
    line-height: 1.5;
  }

  /* ── Atlas-complete screen ────────────────────────────── */
  .atlas-complete {
    max-width: 720px;
    animation: lk-fade-in 320ms ease-out both;
    color: var(--lk-ink);
  }
  .atlas-complete .head { margin-bottom: 22px; }
  .atlas-complete .title {
    margin: 0 0 8px;
    font-size: 2.1rem;
    font-weight: 600;
    line-height: 1.05;
    letter-spacing: -0.02em;
    color: var(--lk-ink);
  }
  .atlas-complete .count {
    margin: 0 0 14px;
    font-size: 0.96rem;
    color: var(--lk-ink-soft);
    line-height: 1.55;
    max-width: 62ch;
  }
  .atlas-complete .count strong {
    color: var(--lk-ink);
    font-weight: 600;
  }
  /* Italic Georgia — the editorial "letter from the machine" beat.
     Sovereign's voice reserves this kind of line for invitations
     (see README tone). */
  .invitation {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 1.45rem;
    font-style: italic;
    line-height: 1.15;
    color: var(--accent-light);
  }
  .starter-zone {
    margin: 20px 0;
    padding: 16px 0;
    border-top: 1px solid var(--lk-rule);
    border-bottom: 1px solid var(--lk-rule);
  }
  .excerpts {
    margin: 20px 0;
    padding-top: 16px;
    border-top: 1px solid var(--lk-rule);
  }
  .excerpts-label { margin-bottom: 10px; }
  .excerpt-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .excerpt {
    padding-bottom: 12px;
    border-bottom: 1px solid var(--lk-rule-soft);
  }
  .excerpt:last-child { border-bottom: 0; padding-bottom: 0; }
  .excerpt-body {
    margin: 0;
    font-size: var(--lk-size-body);
    color: var(--lk-ink);
    line-height: 1.5;
  }
  .excerpt-source {
    margin: 4px 0 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
  }
  .atlas-complete .actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 16px;
  }
  .err-msg {
    color: var(--lk-err, #d27979);
    font-size: var(--lk-size-meta);
    margin: 8px 0;
  }
</style>
