<script lang="ts">
  // Settings → Imports
  //
  // v1 ships the Anthropic path: pick the export `.zip`, Sovereign
  // unzips `conversations.json` into the canonical landing path,
  // POSTs the install, and the existing `corpus-progress` stream
  // drives the live progress card. On `phase = complete`, the
  // "Open in Atlas" button switches the rail via
  // `atlasNavigation.requestAtom(corpus_id, firstAtomId)` — the
  // imported corpus appears under the new "Conversations" header
  // alongside `conversation-history` (the user's Sovereign-internal
  // chats).
  //
  // ChatGPT + Gemini land as additional source rows in a follow-up
  // PR (SYSTEM_OVERVIEW §10.1). The "Coming soon" pills are visible
  // today so the product surface is legible even though only one
  // source is wired.

  import { open } from "@tauri-apps/plugin-dialog";
  import {
    atlasListAtoms,
    importAnthropicZip,
    type ImportStartResponse,
  } from "../../api";
  import { corpusProgressStore } from "../../stores/corpusProgress.svelte";
  import { atlasNavigation } from "../../stores/atlasNavigation.svelte";
  import {
    deriveEta,
    formatPreflightBand,
  } from "../../util/etaFromProgress";

  type Stage = "idle" | "starting" | "in-progress" | "complete" | "failed";

  let stage = $state<Stage>("idle");
  let startResponse = $state<ImportStartResponse | null>(null);
  let errorMessage = $state<string | null>(null);
  let startedAtMs = $state<number | null>(null);
  // 1Hz tick so the live ETA refreshes between corpus-progress events
  // — those land at maybe 0.3-1 Hz; without a local tick the ETA
  // chip can read stale by ~30s during the long phases.
  let _nowTick = $state(performance.now());
  $effect(() => {
    if (stage !== "in-progress") return;
    const handle = setInterval(() => {
      _nowTick = performance.now();
    }, 1000);
    return () => clearInterval(handle);
  });

  const corpusId = "conversations-anthropic";

  let progress = $derived(corpusProgressStore.byId[corpusId]);

  // Roll the stage state machine forward off the streamed phase.
  $effect(() => {
    if (!progress) return;
    if (progress.phase === "complete") {
      stage = "complete";
    } else if (progress.phase === "failed") {
      stage = "failed";
      errorMessage = progress.message ?? "Import failed";
    } else if (stage === "starting" || stage === "in-progress") {
      stage = "in-progress";
    }
  });

  let etaResult = $derived.by(() => {
    if (stage !== "in-progress" || startedAtMs === null) {
      return { label: "", secondsRemaining: null };
    }
    // Touch _nowTick so the $derived re-runs on the 1Hz tick even
    // when no new progress event has arrived.
    void _nowTick;
    return deriveEta(progress, startedAtMs);
  });

  let preflightBand = $derived(
    startResponse?.estimated_minutes
      ? formatPreflightBand(startResponse.estimated_minutes)
      : "",
  );

  // Friendly phase labels — `corpus-progress` enum is more granular
  // than the UI needs. Map to the four stages the user cares about.
  const PHASE_LABELS: Record<string, string> = {
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
    complete: "Done",
    failed: "Failed",
  };

  async function pickAndStart() {
    if (stage === "starting" || stage === "in-progress") return;
    errorMessage = null;
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
      errorMessage = e instanceof Error ? e.message : String(e);
      return;
    }
    if (typeof picked !== "string") return;

    stage = "starting";
    startedAtMs = performance.now();
    startResponse = null;
    try {
      startResponse = await importAnthropicZip(picked);
      stage = "in-progress";
    } catch (e) {
      stage = "failed";
      errorMessage = e instanceof Error ? e.message : String(e);
    }
  }

  async function openInAtlas() {
    try {
      const page = await atlasListAtoms(corpusId, undefined, {
        limit: 1,
        offset: 0,
      });
      const firstAtomId = page.items[0]?.atom_id;
      if (!firstAtomId) {
        errorMessage = "Atlas reports zero atoms — enrichment may have produced an empty result.";
        return;
      }
      atlasNavigation.requestAtom(corpusId, firstAtomId);
    } catch (e) {
      errorMessage = e instanceof Error ? e.message : String(e);
    }
  }

  function retry() {
    stage = "idle";
    startResponse = null;
    errorMessage = null;
    startedAtMs = null;
    void pickAndStart();
  }
</script>

<div class="imports-tab">
  <div class="sources">
    <article class="source-card source-card--active">
      <header class="source-card-header">
        <div class="source-icon">💬</div>
        <div class="source-meta">
          <h3 class="source-name">Claude (Anthropic)</h3>
          <p class="source-help">
            Go to <strong>claude.ai → Settings → Privacy → Export data</strong>.
            Anthropic emails you a download link. The file is a <code>.zip</code> named
            <code>data-&lt;uuid&gt;-&lt;batch&gt;.zip</code>.
          </p>
        </div>
      </header>
      <button
        type="button"
        class="primary"
        onclick={pickAndStart}
        disabled={stage === "starting" || stage === "in-progress"}
        data-testid="imports-pick-claude"
      >
        {#if stage === "idle" || stage === "complete" || stage === "failed"}
          Import Claude export
        {:else if stage === "starting"}
          Unpacking…
        {:else}
          Import in progress
        {/if}
      </button>
    </article>

    <article class="source-card source-card--disabled">
      <header class="source-card-header">
        <div class="source-icon">💬</div>
        <div class="source-meta">
          <h3 class="source-name">ChatGPT (OpenAI)</h3>
          <p class="source-help">Export your data from OpenAI's privacy portal. We'll add support shortly.</p>
        </div>
      </header>
      <span class="badge">Coming soon</span>
    </article>

    <article class="source-card source-card--disabled">
      <header class="source-card-header">
        <div class="source-icon">💬</div>
        <div class="source-meta">
          <h3 class="source-name">Gemini (Google)</h3>
          <p class="source-help">Export Gemini Apps via Google Takeout. We'll add support shortly.</p>
        </div>
      </header>
      <span class="badge">Coming soon</span>
    </article>
  </div>

  {#if startResponse}
    <section class="progress-card" data-testid="imports-progress-card">
      <header class="progress-card-header">
        <div>
          <p class="progress-corpus">Claude conversations</p>
          <p class="progress-detail">
            {startResponse.total_messages.toLocaleString()} messages
            {#if preflightBand && stage !== "complete" && stage !== "failed"}
              · {preflightBand}
            {/if}
          </p>
        </div>
        {#if stage === "in-progress" && etaResult.label}
          <span class="eta-chip">{etaResult.label}</span>
        {/if}
      </header>

      {#if progress}
        <div class="progress-bar" role="progressbar" aria-valuenow={Math.round(progress.percent)} aria-valuemin={0} aria-valuemax={100}>
          <div class="progress-bar-fill" style:width={`${Math.min(100, Math.max(0, progress.percent))}%`}></div>
        </div>
        <p class="phase-label">{PHASE_LABELS[progress.phase] ?? progress.phase}</p>
      {/if}

      {#if stage === "complete"}
        <div class="actions">
          <button type="button" class="primary" onclick={openInAtlas} data-testid="imports-open-in-atlas">
            Open in Atlas
          </button>
        </div>
      {/if}
      {#if stage === "failed"}
        <div class="actions">
          {#if errorMessage}
            <p class="error" role="alert">{errorMessage}</p>
          {/if}
          <button type="button" class="primary" onclick={retry} data-testid="imports-retry">
            Retry
          </button>
        </div>
      {/if}
    </section>
  {/if}

  {#if errorMessage && !startResponse}
    <p class="error" role="alert">{errorMessage}</p>
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
  }

  .progress-bar-fill {
    height: 100%;
    background: var(--accent, #3a6ad0);
    transition: width 220ms ease;
  }

  .phase-label {
    margin: 0;
    font-size: 0.82rem;
    color: var(--text-muted);
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
</style>
