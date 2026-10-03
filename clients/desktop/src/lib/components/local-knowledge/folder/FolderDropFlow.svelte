<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, untrack } from "svelte";

  import {
    enrichGetStarterQuestions,
    governanceWriteRecipe,
    governancePostBuildSeed,
    lcCancel,
    lcEnrichNow,
    lcEnrichReset,
    lcIngest,
    lcOcrAvailable,
    lcPreScan,
    meshAssistStart,
  } from "../../../api";
  import type {
    IngestStats,
    LocalCorpusProgress,
    PreScanResult,
    SampledDocuments,
    StarterQuestion,
  } from "../../../types";
  import { assistProgressStore } from "../../../stores/assistProgress.svelte";
  import { chatSeedStore } from "../../../stores/chatSeed.svelte";
  import { notifyReadyToAsk } from "../../../stores/toast.svelte";
  import PeerAssistOffer from "../../mesh/PeerAssistOffer.svelte";
  import AssistProgressPanel from "../../mesh/AssistProgressPanel.svelte";

  import FolderSelectPanel from "./FolderSelectPanel.svelte";
  import PreScanPanel from "./PreScanPanel.svelte";
  import FolderCompletePanel from "./FolderCompletePanel.svelte";
  import IngestProgressPanel from "../IngestProgressPanel.svelte";
  import EnrichPollProgress from "../../EnrichPollProgress.svelte";
  import StarterChips from "../../StarterChips.svelte";
  import InkStamp from "../../onboarding/InkStamp.svelte";
  import ProgressRule from "../../onboarding/ProgressRule.svelte";
  import RotatingMessage from "../../onboarding/RotatingMessage.svelte";
  import { cleanExcerptTitle } from "../../../onboarding/excerpt_helpers";

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
        /// Ingest has just completed; brief transient while we write
        /// any governance recipe and kick the in-process daemon build
        /// (`lcEnrichReset` + `lcEnrichNow`). Usually sub-second; if
        /// kicking fails we transition to `complete` so the user keeps
        /// the ingest-only surface.
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
        /// Fatal-ish error from kicking the in-process build. Non-null
        /// falls through to a "continue without atlas" affordance.
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

  // `initialPath` seeds `step` once. `untrack` documents the
  // intent and silences the `state_referenced_locally` warning,
  // which fires whenever a prop is read inside `$state(...)` —
  // a case where an unsuspecting reader might expect reactive
  // sync that doesn't actually happen.
  let step: Step = $state(
    untrack(() => ({ kind: "select", initialPath: initialPath ?? undefined })),
  );
  let unlisten: UnlistenFn | null = null;
  let cancelling = $state(false);
  /** Peer-assist decision captured from the confirm-step offer. When
   *  `enabled`, we kick off a grant-scoped collaborative ingest across the
   *  selected peers after the local ingest starts. Reset per confirm. */
  let assistDecision = $state<{ enabled: boolean; peerNodeIds: string[] }>({
    enabled: false,
    peerNodeIds: [],
  });
  /** corpus_id of the step currently in flight, for the assist-progress
   *  lookup. Present on confirm / ingesting / enriching / initializing. */
  // `$derived.by` (closure form) so TS control-flow analysis resets `step`
  // to the full `Step` union — the bare-expression form inherits the
  // top-level narrowing to `{kind:"select"}` from the `$state` initializer
  // above and would reject every other-variant comparison.
  let currentCorpusId = $derived.by(() =>
    step.kind === "confirm" ||
    step.kind === "ingesting" ||
    step.kind === "enriching" ||
    step.kind === "initializing_atlas"
      ? step.corpusId
      : null,
  );
  let activeAssistJob = $derived(
    currentCorpusId ? assistProgressStore.get(currentCorpusId) : undefined,
  );
  /** An Obsidian vault and a plain folder read differently to a user; the
   *  offer's copy keys off this. Previously hardcoded `"folder"` even for
   *  vaults. */
  let assistSurface = $derived<"vault" | "folder">(
    sourceType === "obsidian" ? "vault" : "folder",
  );
  let assistStarting = $state(false);
  let assistStartError = $state("");

  /**
   * Issue the grant + start the collaborative ingest for the selected peers.
   *
   * Callable BEFORE ingest (from the confirm step, fire-and-forget: a failure
   * must never block the local ingest) and DURING it (from the ingesting step,
   * where the user clicked a button and is owed a visible error). `surfaceError`
   * distinguishes the two — silently swallowing a failure the user explicitly
   * asked for would be the same glassbox mistake as the offer hiding itself.
   */
  async function startAssist(corpusId: string, surfaceError: boolean) {
    if (!assistDecision.enabled || assistDecision.peerNodeIds.length === 0) {
      return;
    }
    try {
      const handle = await meshAssistStart(
        corpusId,
        assistDecision.peerNodeIds,
      );
      assistProgressStore.track({
        corpus_id: handle.corpus_id,
        handoff_id: handle.handoff_id,
        grant_expires_at_ms: handle.grant_expires_at_ms,
      });
    } catch (e) {
      if (surfaceError) throw e;
      console.warn("peer-assist failed to start; continuing local-only", e);
    }
  }

  async function startAssistNow(corpusId: string) {
    if (!corpusId) return;
    assistStarting = true;
    assistStartError = "";
    try {
      await startAssist(corpusId, true);
    } catch (e) {
      assistStartError = `Couldn't start mesh help: ${e}`;
    }
    assistStarting = false;
  }
  /** The corpus template the user picked on the confirm step. `"notes"`
   *  = the default sample-atlas path; `"governance"` = attach the
   *  community-governance recipe and build the full corpus so the
   *  Conflicts panel lights up. Captured at confirm, read at
   *  ingest-complete (the linear flow guarantees one in flight). */
  let ingestTemplate: "notes" | "governance" = "notes";
  /** Source folder path, captured at confirm — the governance recipe
   *  records it as provenance. */
  let ingestPath = "";
  /** Whether the desktop has a working OCR pipeline. Probed once at
   *  flow start; passed into PreScanPanel so the OCR offer only
   *  surfaces when accepting it would actually do something. */
  let ocrAvailable = $state(false);

  // Probe OCR availability once when the flow mounts. Failure is
  // non-fatal — defaults to "not available" and the UI hides the
  // affordance, exactly like a build without bundled binaries.
  $effect(() => {
    let cancelled = false;
    void lcOcrAvailable()
      .then((avail) => {
        if (!cancelled) ocrAvailable = avail;
      })
      .catch(() => {
        if (!cancelled) ocrAvailable = false;
      });
    return () => {
      cancelled = true;
    };
  });

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

  async function handleConfirmIngest(
    useOcr: boolean,
    template: "notes" | "governance" = "notes",
  ) {
    if (step.kind !== "confirm") return;
    const { corpusId, displayName, path } = step;
    ingestTemplate = template;
    ingestPath = path;
    step = {
      kind: "ingesting",
      corpusId,
      displayName,
      progress: null,
    };
    await kickOffIngest(corpusId, useOcr);

    // If the user opted into mesh help, issue the grant + start the
    // collaborative ingest scoped to the selected peers. Failure here never
    // blocks the local ingest — it just finishes on this machine.
    await startAssist(corpusId, false);
  }

  async function kickOffIngest(corpusId: string, useOcr = false) {
    try {
      const jobId = await lcIngest(corpusId, useOcr);
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
      // Governance corpus: attach the community-governance recipe so
      // enrichment takes the custom-ontology path. The rule baseline is
      // seeded server-side on build completion (the enrich hook).
      if (ingestTemplate === "governance") {
        await governanceWriteRecipe(corpusId, displayName, ingestPath);
      }
      // In-process tiered enrichment: clear any zombie status, then kick
      // the daemon build (POST /internal/corpus/enrich-once). No
      // `sovereign-cli` subprocess — that binary isn't bundled with the
      // desktop. Progress is polled by <EnrichPollProgress> below. Tiered
      // RAPTOR is per-document, so the whole corpus is enriched (no
      // client-side sampling step).
      await lcEnrichReset(corpusId);
      await lcEnrichNow(corpusId);
      step = {
        kind: "enriching",
        corpusId,
        displayName,
        stats,
        sampled: { titles: [], total: stats.files_indexed },
        spawnError: null,
      };
    } catch (e: unknown) {
      // Kicking the build failed. Fall through to the ingest-only
      // completion screen — search still works, just without atlas.
      console.warn("auto enrich kick failed:", e);
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

  async function handleAtlasComplete() {
    if (step.kind !== "enriching") return;
    const { corpusId, displayName, stats, sampled } = step;
    // Governance template: atoms.json now exists, so migrate atom ids to
    // content-hash + seed the rule baseline (replaces the old
    // `enrich_build_async` completion hook). Non-governance folders skip
    // this. Best-effort — a seed failure shouldn't block the celebration.
    if (ingestTemplate === "governance") {
      try {
        await governancePostBuildSeed(corpusId);
      } catch (e) {
        console.warn("governancePostBuildSeed failed:", e);
      }
    }
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

  function handleAtlasFailed(reason: string) {
    if (step.kind !== "enriching") return;
    // Stay on the enriching screen so the user can read the failure and
    // choose to continue without the atlas.
    step = { ...step, spawnError: reason };
  }

  function continueWithoutAtlas() {
    if (step.kind !== "enriching") return;
    // Leave the daemon build running; the toast + Library reflect
    // completion. Search already works without the atlas.
    atlasBackgrounded = true;
    step = { kind: "complete", stats: step.stats };
  }

  function dropToChatNow() {
    // Fires only when the host wired `onDropToChat`. Atlas keeps
    // running in the daemon; the chat empty state upgrades when the
    // build finishes.
    atlasBackgrounded = true;
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

  /// Set when the user leaves the build to run in the background
  /// ("Skip atlas" / "Start chatting"). Tells FolderCompletePanel the
  /// daemon is still building the atlas for this corpus.
  let atlasBackgrounded = $state(false);

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
      {ocrAvailable}
      onConfirm={handleConfirmIngest}
      onChooseAgain={handleChooseAgain}
    />
    <PeerAssistOffer
      corpusId={step.corpusId}
      surface={assistSurface}
      defaultExpanded={step.result.readable.length >= 500}
      explainWhenUnavailable={true}
      onChange={(d) => (assistDecision = d)}
    />
  {:else if step.kind === "ingesting"}
    <IngestProgressPanel progress={step.progress} />
    {#if activeAssistJob}
      <AssistProgressPanel
        job={activeAssistJob}
        onRevoke={(c) => assistProgressStore.revoke(c)}
      />
    {:else}
      <!-- Peer help mid-flight. The pre-scan offer above is not enough: the
           resume-on-relaunch path jumps straight to `ingesting` and never
           renders `confirm`, so a long vault re-sync previously had NO way to
           ask for help (reported 2026-07-27). This is also where the decision
           is most informed — the user can see the actual rate first. -->
      <PeerAssistOffer
        corpusId={step.corpusId}
        surface={assistSurface}
        explainWhenUnavailable={true}
        onChange={(d) => (assistDecision = d)}
      />
      {#if assistDecision.enabled && assistDecision.peerNodeIds.length > 0}
        <button
          class="lk-btn"
          onclick={() => startAssistNow(step.kind === "ingesting" ? step.corpusId : "")}
          disabled={assistStarting}
        >
          {assistStarting ? "Starting…" : "Get mesh help"}
        </button>
      {/if}
      {#if assistStartError}
        <p class="assist-error">{assistStartError}</p>
      {/if}
    {/if}
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
      <EnrichPollProgress
        corpusId={step.corpusId}
        label="Atlas pipeline"
        onComplete={() => void handleAtlasComplete()}
        onFailed={(r) => handleAtlasFailed(r)}
      />
      {#if activeAssistJob}
        <AssistProgressPanel
          job={activeAssistJob}
          onRevoke={(c) => assistProgressStore.revoke(c)}
        />
      {/if}
      <div class="working-actions">
        {#if onDropToChat}
          <button class="lk-btn lk-btn--mark" onclick={dropToChatNow}>
            Start chatting — atlas keeps building
          </button>
        {/if}
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
      atlasStillBuilding={atlasBackgrounded}
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

  .assist-error {
    font-size: 0.85rem;
    color: var(--neg-text, #a33);
    margin: 0.35rem 0 0;
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
     svrnmesh's voice reserves this kind of line for invitations
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
