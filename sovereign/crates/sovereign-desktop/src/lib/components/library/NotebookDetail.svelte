<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  NotebookDetail — one notebook, four tabs (Phase 1 UX refactor).

  This is pure re-parenting of capabilities that already exist:
    - Ask     → a ChatView scoped to this notebook via the existing
                `outerWorkScopeStore` bridge (no ChatView change). Kept
                alive across tab switches so an in-flight conversation
                survives a hop to Explore and back.
    - Explore → <AtlasSurface startingCorpusId=…>. If the notebook has
                no map yet, a "Make explorable" CTA runs the standard
                enrich path (`recipe_enrich_init_from_corpus` +
                `enrich_build_async`) with the shared progress stage.
    - Sources → where the notebook came from + the real re-sync action
                for watched folders.
    - Settings → remove the notebook (+ a stub of the use→make bridge,
                 deepened in P3).
-->
<script lang="ts">
  import { untrack } from "svelte";
  import ChatView from "../ChatView.svelte";
  import AtlasSurface from "../atlas/AtlasSurface.svelte";
  import EnrichmentStage from "../EnrichmentStage.svelte";
  import NotebookKindIcon from "./NotebookKindIcon.svelte";
  import { kindLabel, kindTitle, normalizeKind } from "./notebookKind";
  import {
    recipeEnrichInitFromCorpus,
    enrichBuildAsync,
    lcRemove,
    removeCorpus,
    lcWatchSyncNow,
    lcList,
  } from "../../api";
  import { enrichProgressStore } from "../../stores/enrichProgress.svelte";
  import { outerWorkScopeStore } from "../../stores/outerWorkScope.svelte";
  import type {
    NotebookSummary,
    StarterQuestion,
    LocalCorpusConfig,
  } from "../../types";

  type TabId = "ask" | "explore" | "sources" | "settings";

  let {
    notebook,
    initialTab = "ask",
    onBack,
    onChanged,
    onOpenWorkshop,
  }: {
    notebook: NotebookSummary;
    initialTab?: TabId;
    onBack: () => void;
    /** Fired after a change that the shelf should reflect (a notebook
     *  removed, or freshly made explorable). */
    onChanged?: () => void;
    /** The use→make bridge: open the recipe that built this notebook in
     *  the Workshop. Provided by App; the bridge card hides without it. */
    onOpenWorkshop?: () => void;
  } = $props();

  // Name the recipe that built this notebook from what we already know
  // (Lean — no provenance backend): folder / vault / watched come from
  // the shipped "notebook" recipe; catalog / installed from their own.
  function builtByLine(): string {
    switch (normalizeKind(notebook.source_kind)) {
      case "folder":
      case "obsidian":
      case "watched":
        return "Built by the notebook recipe.";
      case "catalog":
        return "Installed from the catalog.";
      default:
        return "Built by a recipe.";
    }
  }

  // `initialTab` / `notebook.explorable` seed local state ONCE — later prop
  // changes shouldn't reactively yank the open tab or clobber a mid-session
  // explorable flip. `untrack` makes that one-time intent explicit.
  let tab = $state<TabId>(untrack(() => initialTab));

  // Explorable can flip from false→true mid-session once an enrich build
  // completes, so mirror it locally rather than reading the prop directly.
  let explorable = $state(untrack(() => notebook.explorable));

  // ── Ask: keep-alive scoped chat ───────────────────────────────────
  //
  // Mount the ChatView once the user first opens Ask, then keep it in the
  // DOM (CSS show/hide) so switching to Explore and back doesn't discard
  // an in-progress conversation. The scope MUST be set before ChatView
  // mounts — its consume-on-empty effect reads `outerWorkScopeStore`
  // when the first (empty) conversation is minted.
  let askVisited = $state(false);
  $effect(() => {
    if (tab === "ask" && !askVisited) {
      outerWorkScopeStore.set([notebook.id]);
      askVisited = true;
    }
  });

  // ── Explore: make-explorable enrich flow ──────────────────────────
  let enrichError = $state<string | null>(null);
  let enriching = $state(false);
  // The live build job for this corpus, if one is streaming. Drives the
  // shared progress stage and the false→true explorable flip.
  let enrichJob = $derived(enrichProgressStore.byCorpus(notebook.id)[0] ?? null);

  async function makeExplorable() {
    enrichError = null;
    enriching = true;
    try {
      // Scaffold the atlas config from the installed index, then build.
      // `recipe_enrich_init_from_corpus` is idempotent (`--force`).
      await recipeEnrichInitFromCorpus(notebook.id);
      const handle = await enrichBuildAsync(notebook.id, null, null);
      await enrichProgressStore.track(handle);
    } catch (e) {
      enrichError = e instanceof Error ? e.message : String(e);
      enriching = false;
    }
  }

  function onEnrichTerminal(
    kind: "complete" | "aborted" | "spawn_failed" | "cancelled",
  ) {
    enriching = false;
    if (kind === "complete") {
      explorable = true;
      onChanged?.();
    }
  }

  // ── Sources: hydrate the local-corpus config for path + sync ──────
  let localConfig = $state<LocalCorpusConfig | null>(null);
  let sourcesLoaded = $state(false);
  let syncing = $state(false);
  let syncMsg = $state<string | null>(null);

  async function loadSources() {
    if (sourcesLoaded) return;
    try {
      const all = await lcList();
      localConfig = all.find((c) => c.id === notebook.id) ?? null;
    } catch {
      localConfig = null;
    } finally {
      sourcesLoaded = true;
    }
  }
  $effect(() => {
    if (tab === "sources") void loadSources();
  });

  async function syncNow() {
    syncing = true;
    syncMsg = null;
    try {
      await lcWatchSyncNow(notebook.id);
      syncMsg = "Sync started — new and changed files are being indexed.";
    } catch (e) {
      syncMsg = `Sync failed: ${e instanceof Error ? e.message : String(e)}`;
    } finally {
      syncing = false;
    }
  }

  // ── Settings: remove ──────────────────────────────────────────────
  let confirmRemove = $state(false);
  let removing = $state(false);
  let removeError = $state<string | null>(null);

  async function removeNotebook() {
    removing = true;
    removeError = null;
    try {
      // Local corpora (folder / vault / watched) are owned by the
      // LocalCorpusManager; everything else is a catalog/recipe install.
      const isLocal = ["folder", "obsidian", "watched"].includes(
        normalizeKind(notebook.source_kind),
      );
      if (isLocal) {
        await lcRemove(notebook.id);
      } else {
        await removeCorpus(notebook.id);
      }
      onChanged?.();
      onBack();
    } catch (e) {
      removeError = e instanceof Error ? e.message : String(e);
      removing = false;
    }
  }

  const TABS: { id: TabId; label: string }[] = [
    { id: "ask", label: "Ask" },
    { id: "explore", label: "Explore" },
    { id: "sources", label: "Sources" },
    { id: "settings", label: "Settings" },
  ];

  function freshness(unix: number | null): string {
    if (!unix) return "—";
    const days = Math.floor((Date.now() / 1000 - unix) / 86400);
    if (days <= 0) return "today";
    if (days === 1) return "yesterday";
    if (days < 30) return `${days}d ago`;
    if (days < 365) return `${Math.floor(days / 30)}mo ago`;
    return `${Math.floor(days / 365)}y ago`;
  }
</script>

<div class="notebook-detail" data-testid="notebook-detail">
  <header class="nb-header">
    <button class="back" onclick={onBack} data-testid="notebook-detail-back" aria-label="Back to Library">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m15 18-6-6 6-6" />
      </svg>
      Library
    </button>
    <div class="nb-title">
      <span class="nb-kind" title={kindTitle(notebook.source_kind)}>
        <NotebookKindIcon kind={notebook.source_kind} size={17} />
      </span>
      <h1>{notebook.name}</h1>
      {#if explorable}
        <span class="nb-explorable" title="Has an explorable map">✦</span>
      {/if}
    </div>
  </header>

  <nav class="nb-tabs" aria-label="Notebook sections">
    {#each TABS as t}
      <button
        class:active={tab === t.id}
        data-testid={`notebook-tab-${t.id}`}
        onclick={() => (tab = t.id)}
      >
        {t.label}
      </button>
    {/each}
  </nav>

  <div class="nb-body">
    <!-- Ask: keep-alive layer so the conversation survives tab hops. -->
    {#if askVisited}
      <div class="ask-layer" class:hidden={tab !== "ask"} aria-hidden={tab !== "ask"}>
        <ChatView conversationId={null} taskSteps={[]} onClearTask={() => {}} />
      </div>
    {/if}

    {#if tab === "explore"}
      {#if explorable}
        <div class="explore-surface">
          <AtlasSurface startingCorpusId={notebook.id} />
        </div>
      {:else if enrichJob}
        <div class="pad">
          <h2>Building the map…</h2>
          <p class="lede">
            Reading {notebook.name} to extract its entities, claims, and
            connections. You can keep using the rest of the app — this runs
            in the background.
          </p>
          <EnrichmentStage job={enrichJob} onTerminal={onEnrichTerminal} />
        </div>
      {:else}
        <div class="pad empty">
          <div class="empty-glyph" aria-hidden="true">✦</div>
          <h2>No map yet</h2>
          <p class="lede">
            Explore turns this notebook into a browsable graph of the people,
            ideas, and claims inside it. Building one reads every document with
            your local model, so it can take a while for a large notebook.
          </p>
          {#if enrichError}
            <p class="error">{enrichError}</p>
          {/if}
          <button
            class="primary"
            onclick={makeExplorable}
            disabled={enriching}
            data-testid="notebook-make-explorable"
          >
            {enriching ? "Starting…" : "Make explorable"}
          </button>
        </div>
      {/if}
    {:else if tab === "sources"}
      <div class="pad">
        <h2>Where this came from</h2>
        <div class="source-card">
          <span class="src-icon"><NotebookKindIcon kind={notebook.source_kind} size={20} /></span>
          <div class="src-meta">
            <div class="src-kind">{kindLabel(notebook.source_kind)}</div>
            <div class="src-sub">{kindTitle(notebook.source_kind)}</div>
            {#if localConfig}
              <div class="src-path" title={localConfig.root_path}>{localConfig.root_path}</div>
            {/if}
            <div class="src-stats">
              {notebook.doc_count.toLocaleString()} chunks · indexed {freshness(notebook.updated_unix)} · {notebook.scope}
            </div>
          </div>
        </div>

        {#if normalizeKind(notebook.source_kind) === "watched"}
          <div class="source-action">
            <button class="secondary" onclick={syncNow} disabled={syncing} data-testid="notebook-sync-now">
              {syncing ? "Syncing…" : "Sync now"}
            </button>
            {#if syncMsg}<span class="src-msg">{syncMsg}</span>{/if}
          </div>
        {:else if !sourcesLoaded}
          <p class="muted">Loading source details…</p>
        {/if}
      </div>
    {:else if tab === "settings"}
      <div class="pad">
        <h2>Notebook settings</h2>
        <p class="lede">
          {notebook.name} was added as a {kindLabel(notebook.source_kind).toLowerCase()}
          notebook. Deeper per-notebook configuration arrives in a later pass.
        </p>

        {#if onOpenWorkshop}
          <!-- The use→make bridge (D9): the graduation hinge from a notebook
               to the Workshop where its recipe can be changed. -->
          <div class="setting-row bridge">
            <div>
              <div class="setting-title">{builtByLine()}</div>
              <div class="setting-sub">
                Want it to work differently — add summaries, OCR, a web step? Open
                the recipe in the Workshop to change how notebooks like this are built.
              </div>
            </div>
            <button class="secondary" onclick={onOpenWorkshop} data-testid="notebook-open-workshop">
              Open in Workshop →
            </button>
          </div>
        {/if}

        <div class="setting-row danger">
          <div>
            <div class="setting-title">Remove this notebook</div>
            <div class="setting-sub">
              Deletes the index from this machine. The original files
              {#if normalizeKind(notebook.source_kind) === "watched" || normalizeKind(notebook.source_kind) === "folder" || normalizeKind(notebook.source_kind) === "obsidian"}
                on disk are never touched.
              {:else}
                can be re-installed from the catalog later.
              {/if}
            </div>
          </div>
          {#if confirmRemove}
            <div class="confirm">
              <button class="danger-btn" onclick={removeNotebook} disabled={removing} data-testid="notebook-remove-confirm">
                {removing ? "Removing…" : "Confirm remove"}
              </button>
              <button class="secondary" onclick={() => (confirmRemove = false)} disabled={removing}>Cancel</button>
            </div>
          {:else}
            <button class="secondary" onclick={() => (confirmRemove = true)} data-testid="notebook-remove">Remove</button>
          {/if}
        </div>
        {#if removeError}<p class="error">{removeError}</p>{/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .notebook-detail {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
    background: var(--bg-primary);
  }

  .nb-header {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 12px 18px 10px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .back {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font: inherit;
    font-size: 0.82rem;
    font-weight: 500;
    color: var(--text-secondary);
    background: none;
    border: none;
    cursor: pointer;
    padding: 4px 6px;
    border-radius: var(--radius);
  }
  .back:hover { color: var(--text-primary); background: var(--bg-elevated); }

  .nb-title {
    display: flex;
    align-items: center;
    gap: 9px;
    min-width: 0;
  }
  .nb-kind { color: var(--text-muted); display: inline-flex; }
  .nb-title h1 {
    font-size: 1.02rem;
    font-weight: 650;
    color: var(--text-primary);
    margin: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .nb-explorable { color: var(--accent); font-size: 0.95rem; }

  .nb-tabs {
    display: flex;
    gap: 4px;
    padding: 8px 14px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-secondary);
    flex-shrink: 0;
  }
  .nb-tabs button {
    font: inherit;
    cursor: pointer;
    padding: 5px 13px;
    border-radius: var(--radius);
    border: 1px solid transparent;
    background: transparent;
    color: var(--text-secondary);
    font-weight: 500;
    font-size: 0.85rem;
  }
  .nb-tabs button:hover { color: var(--text-primary); }
  .nb-tabs button.active {
    background: var(--bg-elevated);
    color: var(--text-primary);
    border-color: color-mix(in oklch, var(--accent) 35%, var(--border));
  }

  .nb-body {
    flex: 1;
    min-height: 0;
    position: relative;
    overflow: hidden;
  }

  /* Ask keep-alive layer — absolute fill, CSS show/hide. */
  .ask-layer {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
  }
  .ask-layer.hidden { display: none; }

  /* Mirror App.svelte's standalone `.atlas-surface` exactly — a flex
     column with a bounded height — so AtlasCorpusView's windowed
     `.atom-scroll` gets a real viewport to virtualize against (a plain
     block here let it render every loaded row). */
  .explore-surface {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow-y: auto;
  }

  .pad {
    height: 100%;
    overflow-y: auto;
    padding: 24px 28px;
    max-width: 760px;
  }
  .pad h2 {
    font-size: 1.05rem;
    font-weight: 600;
    color: var(--text-primary);
    margin: 0 0 8px;
  }
  .lede {
    color: var(--text-secondary);
    font-size: 0.9rem;
    line-height: 1.55;
    margin: 0 0 18px;
  }
  .muted { color: var(--text-muted); font-size: 0.85rem; }

  .empty {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
  }
  .empty-glyph {
    font-size: 1.8rem;
    color: color-mix(in oklch, var(--accent) 70%, var(--text-muted));
    margin-bottom: 6px;
  }

  .source-card {
    display: flex;
    gap: 14px;
    align-items: flex-start;
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--bg-secondary);
  }
  .src-icon { color: var(--text-secondary); margin-top: 1px; }
  .src-meta { min-width: 0; }
  .src-kind { font-weight: 600; color: var(--text-primary); font-size: 0.92rem; }
  .src-sub { color: var(--text-secondary); font-size: 0.84rem; margin-top: 2px; }
  .src-path {
    font-family: var(--font-mono);
    font-size: 0.78rem;
    color: var(--text-muted);
    margin-top: 8px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .src-stats { color: var(--text-muted); font-size: 0.78rem; margin-top: 8px; }
  .source-action { display: flex; align-items: center; gap: 12px; margin-top: 16px; }
  .src-msg { color: var(--text-secondary); font-size: 0.82rem; }

  .setting-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--bg-secondary);
  }
  .setting-row + .setting-row { margin-top: 12px; }
  .setting-row.bridge { border-color: color-mix(in oklch, var(--accent) 30%, var(--border)); }
  .setting-row.danger { border-color: color-mix(in oklch, var(--error) 30%, var(--border)); }
  .setting-title { font-weight: 600; color: var(--text-primary); font-size: 0.9rem; }
  .setting-sub { color: var(--text-secondary); font-size: 0.82rem; margin-top: 3px; max-width: 46ch; }
  .confirm { display: flex; gap: 8px; }

  button.primary {
    font: inherit;
    font-weight: 600;
    font-size: 0.88rem;
    padding: 9px 18px;
    border-radius: var(--radius);
    border: none;
    background: var(--accent);
    color: var(--accent-contrast, #fff);
    cursor: pointer;
  }
  button.primary:disabled { opacity: 0.6; cursor: default; }
  button.secondary {
    font: inherit;
    font-weight: 500;
    font-size: 0.84rem;
    padding: 7px 14px;
    border-radius: var(--radius);
    border: 1px solid var(--border-mid);
    background: var(--bg-elevated);
    color: var(--text-primary);
    cursor: pointer;
  }
  button.secondary:disabled { opacity: 0.6; cursor: default; }
  button.danger-btn {
    font: inherit;
    font-weight: 600;
    font-size: 0.84rem;
    padding: 7px 14px;
    border-radius: var(--radius);
    border: 1px solid var(--error);
    background: color-mix(in oklch, var(--error) 14%, transparent);
    color: var(--error);
    cursor: pointer;
  }
  .error { color: var(--error); font-size: 0.84rem; margin-top: 12px; }
</style>
