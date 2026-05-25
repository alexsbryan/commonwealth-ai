<script lang="ts">
  // Settings → Imports
  //
  // Thin view over `importsStore` (module-level singleton). The
  // store survives unmount, listens to `corpus-progress` globally,
  // and auto-fires `enrich_build_async` when ingest completes — so
  // navigating away from this tab and back never resets the user's
  // import.
  //
  // v1 ships the Anthropic path. ChatGPT + Gemini show "Coming soon"
  // pills (SYSTEM_OVERVIEW §10.1).

  import { onMount } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { listen } from "@tauri-apps/api/event";
  import {
    atlasCheckGlinerModel,
    atlasDownloadGlinerModel,
    atlasListAtoms,
    importAnthropicZip,
  } from "../../api";
  import type { GlinerModelStatus } from "../../types";
  import { atlasNavigation } from "../../stores/atlasNavigation.svelte";
  import { importsStore, type ImportState } from "../../stores/importsStore.svelte";
  import {
    deriveEta,
    formatPreflightBand,
    formatRefinedTotal,
  } from "../../util/etaFromProgress";

  // 1Hz tick so the live ETA + "last update" indicator refresh
  // between events. Without this, the chip can read 30+ seconds
  // stale during long enrichment phases.
  let _nowTick = $state(performance.now());
  $effect(() => {
    const handle = setInterval(() => {
      _nowTick = performance.now();
    }, 1000);
    return () => clearInterval(handle);
  });

  onMount(() => {
    void importsStore.init();
    void refreshGlinerStatus();
    const unlisten = listen<{ file: string; downloaded: number; total: number }>(
      "gliner-download-progress",
      (e) => {
        const { file, downloaded, total } = e.payload;
        if (file === "__complete__") {
          glinerDownloadFile = null;
          glinerDownloadPct = 0;
          void refreshGlinerStatus();
          return;
        }
        glinerDownloadFile = file;
        if (total > 0) {
          glinerDownloadPct = Math.min(100, Math.round((downloaded / total) * 100));
        }
      },
    );
    return () => {
      void unlisten.then((u) => u());
    };
  });

  let importState = $derived<ImportState>(importsStore.state);
  let progress = $derived(importState.ingestProgress);

  // ─── GliNER per-chunk entity extraction (Phase 1 model UX) ───
  //
  // Local NER model for the conv-tiered retrieval surface. Without
  // it, ingest still works but falls back to RAPTOR-derived
  // entities only (~5/leaf instead of ~24/chunk). The toggle is a
  // one-time install — once the model is on disk, every future
  // import auto-runs entity extraction via the daemon hook.
  let glinerStatus: GlinerModelStatus | null = $state(null);
  let glinerDownloading = $state(false);
  let glinerDownloadFile: string | null = $state(null);
  let glinerDownloadPct = $state(0);
  let glinerError: string | null = $state(null);

  async function refreshGlinerStatus() {
    try {
      glinerStatus = await atlasCheckGlinerModel();
    } catch (e) {
      glinerError = e instanceof Error ? e.message : String(e);
    }
  }

  async function downloadGlinerModel() {
    glinerDownloading = true;
    glinerError = null;
    glinerDownloadPct = 0;
    glinerDownloadFile = null;
    try {
      await atlasDownloadGlinerModel();
    } catch (e) {
      glinerError = e instanceof Error ? e.message : String(e);
    } finally {
      glinerDownloading = false;
      await refreshGlinerStatus();
    }
  }

  // Active stages collapse the picker UI in favour of the progress
  // card so the user doesn't see a "Import Claude export" button
  // dangling next to an ingest the daemon is already running. Only
  // `idle` and `complete` / `failed` (terminal) bring the picker
  // back — the latter as the recovery path when the user wants to
  // retry a different export.
  let pickerVisible = $derived(
    importState.stage === "idle" ||
      importState.stage === "complete" ||
      importState.stage === "failed",
  );

  let etaResult = $derived.by(() => {
    if (
      importState.stage !== "ingesting" ||
      importState.startedAtMs === null ||
      !progress
    ) {
      return { label: "", secondsRemaining: null };
    }
    void _nowTick;
    return deriveEta(progress, importState.startedAtMs);
  });

  // Total-time display layers two sources:
  //   - Refined total once live progress is past warmup (real
  //     chunks/sec → real total).
  //   - Baked pre-flight band before that.
  // Refined wins because it's grounded in observed throughput; the
  // band is a 0.4s/msg guess that's wrong on any non-default model
  // or content shape.
  let totalEstimate = $derived.by(() => {
    if (importState.startedAtMs !== null) {
      void _nowTick;
      const refined = formatRefinedTotal(progress ?? undefined, importState.startedAtMs);
      if (refined) return refined;
    }
    if (importState.startResponse?.estimated_minutes) {
      return formatPreflightBand(importState.startResponse.estimated_minutes);
    }
    return "";
  });

  // Phase labels for the ingest-side `corpus-progress` enum.
  const INGEST_PHASE_LABELS: Record<string, string> = {
    downloading: "Starting…",
    extracting: "Extracting conversations",
    chunking: "Building chunks",
    embedding: "Embedding chunks",
    indexing: "Indexing",
    extracting_claims: "Reading every conversation",
    finding_relationships: "Finding connections",
    extracting_relationships: "Mapping relationships",
    building_link_graph: "Building the atlas",
    computing_profiles: "Surfacing people and topics",
    complete: "Ingest complete — starting enrichment",
    failed: "Failed",
  };

  // Friendly labels for the enrich-subprocess `BuildStep` enum.
  const ENRICH_STEP_LABELS: Record<string, string> = {
    seed: "Seeding topics",
    extract: "Reading every conversation",
    cluster: "Grouping by theme",
    name: "Naming clusters",
    resolve: "Resolving entities",
    tensions: "Mapping tensions",
    gaps: "Finding open questions",
    configure: "Composing the atlas",
    report: "Writing the report",
  };

  let stageLabel = $derived.by(() => {
    if (importState.stage === "starting") return "Starting…";
    if (importState.stage === "ingesting") {
      const phase = progress?.phase;
      if (!phase) return "Starting ingest…";
      return INGEST_PHASE_LABELS[phase] ?? phase;
    }
    if (importState.stage === "enriching") {
      const step = importState.enrichStep;
      if (!step) return "Starting enrichment…";
      const label = ENRICH_STEP_LABELS[step.step] ?? step.step;
      return `${label} (${step.ordinal} of ${step.total})`;
    }
    if (importState.stage === "complete") return "Done";
    if (importState.stage === "failed") return "Failed";
    return "";
  });

  let progressPercent = $derived.by(() => {
    if (importState.stage === "ingesting") {
      // Ingest is the first half of the bar. Cap at 50% so the user
      // sees the bar move into the second half once enrichment fires.
      return Math.min(50, (progress?.percent ?? 0) / 2);
    }
    if (importState.stage === "enriching") {
      // Enrichment is the second half. Steps + chapter progress feed
      // a coarse linear bar 50 → 100%.
      const step = importState.enrichStep;
      if (!step) return 50;
      const stepFraction = (step.ordinal - 1) / step.total;
      let intraStep = 0;
      const ep = importState.enrichProgress;
      if (ep && ep.kind === "chapter_progress" && ep.total > 0) {
        intraStep = ep.index / ep.total / step.total;
      }
      return 50 + (stepFraction + intraStep) * 50;
    }
    if (importState.stage === "complete") return 100;
    return 0;
  });

  async function pickAndStart() {
    if (
      importState.stage === "starting" ||
      importState.stage === "ingesting" ||
      importState.stage === "enriching" ||
      importState.stage === "needs_reset_confirm"
    ) {
      return;
    }
    importsStore.reset();
    let picked: string | string[] | null;
    try {
      picked = await open({
        multiple: false,
        directory: false,
        filters: [
          {
            name: "Claude export (.zip)",
            extensions: ["zip"],
          },
        ],
      });
    } catch (e) {
      importsStore.setError(e instanceof Error ? e.message : String(e));
      return;
    }
    if (typeof picked !== "string") return;

    importsStore.beginImport();
    try {
      const resp = await importAnthropicZip(picked, false);
      importsStore.setStartResponse(resp, picked);
    } catch (e) {
      importsStore.setError(e instanceof Error ? e.message : String(e));
    }
  }

  // User clicked through the destructive-confirm banner. Re-invoke
  // `import_anthropic_zip` with `resetPartial: true` against the
  // same zip path the store remembered. The daemon-side handler
  // wipes the partial index dir, then proceeds with the install.
  async function confirmResetAndStart() {
    const pending = importState.pendingReset;
    if (!pending) return;
    importsStore.beginImport();
    try {
      const resp = await importAnthropicZip(pending.zipPath, true);
      importsStore.setStartResponse(resp, pending.zipPath);
    } catch (e) {
      importsStore.setError(e instanceof Error ? e.message : String(e));
    }
  }

  function cancelReset() {
    importsStore.reset();
  }

  async function openInAtlas() {
    try {
      const page = await atlasListAtoms(importsStore.corpusId, undefined, {
        limit: 1,
        offset: 0,
      });
      const firstAtomId = page.items[0]?.atom_id;
      if (!firstAtomId) {
        importsStore.setError(
          "Atlas reports zero atoms — enrichment may have produced an empty result.",
        );
        return;
      }
      atlasNavigation.requestAtom(importsStore.corpusId, firstAtomId);
    } catch (e) {
      importsStore.setError(e instanceof Error ? e.message : String(e));
    }
  }

  function retry() {
    importsStore.reset();
    void pickAndStart();
  }
</script>

<div class="imports-tab">
  {#if pickerVisible}
  <div class="sources" data-testid="imports-sources">
    <article class="source-card source-card--active">
      <header class="source-card-header">
        <div class="source-icon">💬</div>
        <div class="source-meta">
          <h3 class="source-name">
            Claude (Anthropic)
            {#if importState.alreadyInstalled}
              <span class="imported-badge" title="A Claude export has already been ingested on this machine.">Imported</span>
            {/if}
          </h3>
          <p class="source-help">
            Go to <strong>claude.ai → Settings → Privacy → Export data</strong>.
            Anthropic emails a download link. It's a <code>.zip</code> named
            <code>data-&lt;uuid&gt;-&lt;batch&gt;.zip</code>.
          </p>
        </div>
      </header>
      <button
        type="button"
        class="primary"
        onclick={pickAndStart}
        disabled={importState.stage === "starting" || importState.stage === "ingesting" || importState.stage === "enriching"}
        data-testid="imports-pick-claude"
      >
        {#if importState.stage === "starting"}
          Unpacking…
        {:else if importState.stage === "ingesting" || importState.stage === "enriching"}
          Import in progress
        {:else if importState.alreadyInstalled}
          Re-import a fresh export
        {:else}
          Import Claude export
        {/if}
      </button>
    </article>

    <article class="source-card source-card--disabled">
      <header class="source-card-header">
        <div class="source-icon">💬</div>
        <div class="source-meta">
          <h3 class="source-name">ChatGPT (OpenAI)</h3>
          <p class="source-help">Export your data from OpenAI's privacy portal. Support coming soon.</p>
        </div>
      </header>
      <span class="badge">Coming soon</span>
    </article>

    <article class="source-card source-card--disabled">
      <header class="source-card-header">
        <div class="source-icon">💬</div>
        <div class="source-meta">
          <h3 class="source-name">Gemini (Google)</h3>
          <p class="source-help">Export Gemini Apps via Google Takeout. Support coming soon.</p>
        </div>
      </header>
      <span class="badge">Coming soon</span>
    </article>
  </div>

  <!-- Smart highlights (GliNER per-chunk NER). One-time model
       install; thereafter every imported conversation gets
       automatic entity tagging used by search + Atlas. -->
  <article class="gliner-card">
    <header class="source-header">
      <div class="source-icon">🔍</div>
      <div class="source-meta">
        <h3 class="source-name">Smart highlights for your chats</h3>
        <p class="source-help">
          Tags the people, places, works, and organizations across every
          conversation you import. Runs in the background once installed —
          and search starts finding related threads across topics you
          didn't think to link.
        </p>
      </div>
    </header>
    <div class="gliner-controls" data-testid="gliner-controls">
      {#if glinerStatus === null}
        <span class="badge">Checking…</span>
      {:else if glinerStatus.installed}
        <span class="badge installed">✓ On</span>
        <span class="path-hint">runs locally · nothing leaves your machine</span>
        <button
          type="button"
          class="redownload-btn"
          onclick={downloadGlinerModel}
          disabled={glinerDownloading}
          title="Re-download the model files (skip if already present)"
        >
          {glinerDownloading ? "Updating…" : "Re-download"}
        </button>
      {:else if glinerDownloading}
        <span class="badge running">
          {#if glinerDownloadFile === "tokenizer.json"}
            Preparing… {glinerDownloadPct}%
          {:else}
            Downloading… {glinerDownloadPct}%
          {/if}
        </span>
      {:else}
        <span class="badge not-installed">Off</span>
        <button
          type="button"
          class="install-btn"
          onclick={downloadGlinerModel}
        >
          Turn on (one-time {glinerStatus.size_estimate_mb} MB download)
        </button>
      {/if}
    </div>
    {#if glinerError}
      <p class="gliner-error" role="alert">
        Something went wrong: {glinerError}
      </p>
    {/if}
  </article>
  {/if}

  {#if !pickerVisible && importState.stage !== "needs_reset_confirm"}
    <p class="resume-banner" data-testid="imports-resume-banner">
      Your Claude import is already running. Progress below.
    </p>
  {/if}

  {#if importState.stage === "needs_reset_confirm" && importState.pendingReset}
    <section
      class="confirm-card"
      data-testid="imports-reset-confirm"
      aria-labelledby="imports-reset-title"
    >
      <h3 id="imports-reset-title" class="confirm-title">Start fresh for the best results</h3>
      <p class="confirm-body">
        A previous Claude import didn't finish. Sovereign reads conversations
        better now than it did then — picking up where you left off would mix
        old and new results.
      </p>
      <p class="confirm-body">
        Better to start over with all {importState.pendingReset.totalMessages.toLocaleString()} messages.
        Your export file stays put; only the search data Sovereign built so far
        gets cleared.
      </p>
      <div class="actions confirm-actions">
        <button
          type="button"
          class="primary destructive"
          onclick={confirmResetAndStart}
          data-testid="imports-reset-confirm-yes"
        >
          Start fresh
        </button>
        <button
          type="button"
          class="secondary"
          onclick={cancelReset}
          data-testid="imports-reset-confirm-cancel"
        >
          Not now
        </button>
      </div>
    </section>
  {/if}

  {#if importState.stage !== "idle" && importState.stage !== "needs_reset_confirm"}
    <section class="progress-card" data-testid="imports-progress-card">
      <header class="progress-card-header">
        <div>
          <p class="progress-corpus">Claude conversations</p>
          {#if importState.startResponse}
            <p class="progress-detail">
              {importState.startResponse.total_messages.toLocaleString()} messages
              {#if totalEstimate && importState.stage !== "complete" && importState.stage !== "failed"}
                · {totalEstimate}
              {/if}
            </p>
          {/if}
        </div>
        {#if importState.stage === "ingesting" && etaResult.label}
          <span class="eta-chip" data-testid="imports-eta">{etaResult.label}</span>
        {/if}
      </header>

      <div
        class="progress-bar"
        class:progress-bar--active={importState.stage === "starting" || importState.stage === "ingesting" || importState.stage === "enriching"}
        role="progressbar"
        aria-valuenow={Math.round(progressPercent)}
        aria-valuemin={0}
        aria-valuemax={100}
      >
        <div class="progress-bar-fill" style:width={`${Math.min(100, Math.max(0, progressPercent))}%`}></div>
      </div>
      <p
        class="phase-label"
        class:phase-label--active={importState.stage === "starting" || importState.stage === "ingesting" || importState.stage === "enriching"}
        data-testid="imports-phase-label"
      >{stageLabel}</p>

      {#if importState.stage === "complete"}
        <div class="actions">
          <button
            type="button"
            class="primary"
            onclick={openInAtlas}
            data-testid="imports-open-in-atlas"
          >
            Open in Atlas
          </button>
        </div>
      {/if}
      {#if importState.stage === "failed"}
        <div class="actions">
          {#if importState.errorMessage}
            <p class="error" role="alert" data-testid="imports-error">{importState.errorMessage}</p>
          {/if}
          <button type="button" class="primary" onclick={retry} data-testid="imports-retry">
            Retry
          </button>
        </div>
      {/if}
    </section>
  {/if}

  {#if importState.errorMessage && importState.stage === "idle"}
    <p class="error" role="alert">{importState.errorMessage}</p>
  {/if}
</div>

<style>
  .imports-tab {
    display: flex;
    flex-direction: column;
    gap: 24px;
    max-width: 720px;
  }

  .sources {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .source-card {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 18px 20px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .source-card--disabled {
    opacity: 0.6;
  }

  .source-card-header {
    display: flex;
    align-items: flex-start;
    gap: 14px;
    flex: 1;
  }

  .source-icon {
    font-size: 1.6rem;
    line-height: 1;
    margin-top: 2px;
  }

  .source-meta {
    flex: 1;
  }

  .source-name {
    font-size: 1rem;
    font-weight: 600;
    margin: 0 0 4px;
    letter-spacing: -0.01em;
  }

  .source-help {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.85rem;
    line-height: 1.5;
  }

  .source-help code {
    background: var(--bg-elevated, var(--bg-primary));
    padding: 1px 5px;
    border-radius: 4px;
    font-size: 0.78rem;
  }

  button.primary {
    padding: 9px 16px;
    background: var(--accent, #3a6ad0);
    color: white;
    border: none;
    border-radius: var(--radius-small, 6px);
    font: inherit;
    font-weight: 500;
    cursor: pointer;
    transition: background 150ms ease;
  }

  button.primary:hover:not(:disabled) {
    background: var(--accent-hover, #2f5fbe);
  }

  button.primary:disabled {
    opacity: 0.55;
    cursor: default;
  }

  .badge {
    padding: 3px 9px;
    background: var(--bg-elevated, var(--bg-primary));
    border: 1px solid var(--border-mid, var(--border));
    border-radius: 10px;
    font-size: 0.74rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .imported-badge {
    display: inline-block;
    margin-left: 8px;
    padding: 1px 8px;
    font-size: 0.68rem;
    font-weight: 600;
    color: var(--success, #6bbf6b);
    background: rgba(120, 220, 140, 0.12);
    border: 1px solid rgba(120, 220, 140, 0.5);
    border-radius: 10px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    vertical-align: middle;
  }

  .progress-card {
    padding: 20px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  .progress-card-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
  }

  .progress-corpus {
    margin: 0;
    font-weight: 600;
    font-size: 0.95rem;
  }

  .progress-detail {
    margin: 4px 0 0;
    color: var(--text-muted);
    font-size: 0.82rem;
  }

  .eta-chip {
    padding: 3px 10px;
    background: var(--bg-elevated, var(--bg-primary));
    border: 1px solid var(--border-mid, var(--border));
    border-radius: 10px;
    font-size: 0.76rem;
    color: var(--text-secondary);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .progress-bar {
    height: 6px;
    background: var(--bg-elevated, var(--bg-primary));
    border-radius: 3px;
    overflow: hidden;
    position: relative;
  }

  .progress-bar-fill {
    height: 100%;
    background: var(--accent, #3a6ad0);
    transition: width 220ms ease;
  }

  /* Subtle barber-pole shimmer overlays the filled portion during
     active phases. Indeterminate-feel motion so a long phase (Phase 1
     enrichment, ~minutes per chapter on a cold slot) doesn't read as
     frozen between corpus-progress ticks. Pure CSS — no extra JS, no
     repaint cost on idle. */
  .progress-bar--active .progress-bar-fill {
    background-image: linear-gradient(
      90deg,
      rgba(255, 255, 255, 0.08) 0%,
      rgba(255, 255, 255, 0.22) 50%,
      rgba(255, 255, 255, 0.08) 100%
    );
    background-color: var(--accent, #3a6ad0);
    background-size: 36px 100%;
    background-repeat: repeat-x;
    animation: imports-shimmer 1.4s linear infinite;
  }

  @keyframes imports-shimmer {
    from {
      background-position: 0 0;
    }
    to {
      background-position: 36px 0;
    }
  }

  .phase-label {
    margin: 0;
    font-size: 0.82rem;
    color: var(--text-muted);
  }

  /* Cycling ellipsis after the phase text — visible heartbeat for the
     user that the import hasn't stalled even when the percent stays
     flat for minutes (cold-slot LLM phases). 1, 2, 3 dots → blank →
     repeat. Same `prefers-reduced-motion` posture as the rest of the
     app: respect the OS preference and freeze the indicator. */
  .phase-label--active::after {
    content: "";
    display: inline-block;
    width: 1.4em;
    text-align: left;
    animation: imports-ellipsis 1.4s steps(4, end) infinite;
  }

  @keyframes imports-ellipsis {
    0% { content: ""; }
    25% { content: "."; }
    50% { content: ".."; }
    75% { content: "..."; }
    100% { content: ""; }
  }

  @media (prefers-reduced-motion: reduce) {
    .progress-bar--active .progress-bar-fill {
      animation: none;
    }
    .phase-label--active::after {
      animation: none;
      content: " …";
    }
  }

  .actions {
    display: flex;
    flex-direction: column;
    gap: 10px;
    align-items: flex-start;
  }

  .error {
    margin: 0;
    color: var(--danger, #c33);
    font-size: 0.84rem;
  }

  .resume-banner {
    margin: 0;
    padding: 10px 14px;
    background: var(--bg-elevated, var(--bg-secondary));
    border: 1px solid var(--border-mid, var(--border));
    border-radius: var(--radius);
    color: var(--text-secondary);
    font-size: 0.85rem;
  }

  /* Destructive-reset confirmation banner. Visible only on the
     `needs_reset_confirm` stage. Same border + radius as the
     progress card so the panel reads as one surface; warning
     accent on the left edge signals "read me before clicking." */
  .confirm-card {
    padding: 20px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-left: 3px solid var(--warn, #c97a2b);
    border-radius: var(--radius);
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .confirm-title {
    margin: 0;
    font-size: 0.98rem;
    font-weight: 600;
  }

  .confirm-body {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.85rem;
    line-height: 1.55;
  }

  .confirm-actions {
    flex-direction: row;
    gap: 10px;
    align-items: center;
  }

  button.primary.destructive {
    background: var(--danger, #c33);
  }

  button.primary.destructive:hover:not(:disabled) {
    background: var(--danger-hover, #a82a2a);
  }

  button.secondary {
    padding: 9px 16px;
    background: transparent;
    color: var(--text-secondary);
    border: 1px solid var(--border-mid, var(--border));
    border-radius: var(--radius-small, 6px);
    font: inherit;
    font-weight: 500;
    cursor: pointer;
    transition: background 150ms ease, border-color 150ms ease;
  }

  button.secondary:hover {
    background: var(--bg-elevated, var(--bg-primary));
    border-color: var(--border, currentColor);
  }

  /* GliNER per-chunk entity extraction (Phase 1 install card) */
  .gliner-card {
    margin-top: 16px;
    padding: 16px;
    background: var(--bg-surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 8px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .gliner-controls {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .gliner-controls .badge.installed {
    background: var(--growth-dim);
    color: var(--growth);
  }
  .gliner-controls .badge.not-installed {
    background: var(--bg-elevated);
    color: var(--text-muted);
  }
  .gliner-controls .badge.running {
    background: var(--lavender-dim);
    color: var(--lavender-light);
    font-variant-numeric: tabular-nums;
  }
  .install-btn,
  .redownload-btn {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: 6px;
    padding: 6px 14px;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .install-btn:hover:not(:disabled),
  .redownload-btn:hover:not(:disabled) {
    background: var(--accent-strong, #6989f0);
  }
  .install-btn:disabled,
  .redownload-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .path-hint {
    font-size: 0.72rem;
    color: var(--text-muted, #888);
    font-family: ui-monospace, monospace;
  }
  .gliner-error {
    margin: 0;
    color: var(--error, #d44);
    font-size: 0.85rem;
  }
</style>
