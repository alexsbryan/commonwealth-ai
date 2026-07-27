<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  NotebookDetail — one notebook, four tabs (Phase 1 UX refactor).

  This is pure re-parenting of capabilities that already exist:
    - Ask     → a ChatView scoped to this notebook via the existing
                `outerWorkScopeStore` bridge (no ChatView change). Kept
                alive across tab switches so an in-flight conversation
                survives a hop to Explore and back.
    - Explore → <AtlasSurface startingCorpusId=…>. If the notebook has
                no map yet, a "Make explorable" CTA runs the in-process
                daemon enrich (`lcEnrichReset` + `lcEnrichNow`) and polls
                `enrichmentStatus` for phase/percent — no CLI subprocess.
    - Sources → where the notebook came from + the real re-sync action
                for watched folders.
    - Settings → remove the notebook (+ a stub of the use→make bridge,
                 deepened in P3).
-->
<script lang="ts">
  import { untrack } from "svelte";
  import ChatView from "../ChatView.svelte";
  import AtlasSurface from "../atlas/AtlasSurface.svelte";
  import NotebookOpenQuestions from "./NotebookOpenQuestions.svelte";
  import ConflictsPanel from "./ConflictsPanel.svelte";
  import NotebookKindIcon from "./NotebookKindIcon.svelte";
  import { cardSend, cardReceive } from "../../motion";
  import { kindLabel, kindTitle, normalizeKind } from "./notebookKind";
  import {
    lcEnrichNow,
    lcEnrichReset,
    enrichmentStatus,
    lcRemove,
    removeCorpus,
    lcWatchSyncNow,
    lcList,
    notebookConversations,
  } from "../../api";
  // `EnrichmentStatus` here is the tiered-build status the daemon returns
  // (state/is_stalled/fraction_complete), exported from `api` — distinct from
  // the watched-folder `EnrichmentStatus` union in `types`.
  import type { EnrichmentStatus } from "../../api";
  import { outerWorkScopeStore } from "../../stores/outerWorkScope.svelte";
  import { chatSeedStore } from "../../stores/chatSeed.svelte";
  import type {
    NotebookSummary,
    StarterQuestion,
    LocalCorpusConfig,
    ConversationEntry,
  } from "../../types";

  type TabId = "ask" | "explore" | "conflicts" | "sources" | "settings";

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
  // The Ask tab keeps a conversation with this notebook. On first open we
  // load the notebook's history (conversations scoped to it) and resume
  // the most recent; "+ New" starts a fresh scoped thread. The keep-alive
  // ChatView swaps its `conversationId` rather than remounting.
  let askVisited = $state(false);
  let askConversationId = $state<string | null>(null);
  let notebookConvs = $state<ConversationEntry[]>([]);
  // Header menus: the conversation switcher (Ask tab) and the ⋯ overflow
  // that holds Sources + Settings.
  let convMenuOpen = $state(false);
  let moreMenuOpen = $state(false);

  function setTab(t: TabId) {
    tab = t;
    convMenuOpen = false;
    moreMenuOpen = false;
  }

  async function openAsk() {
    try {
      notebookConvs = (await notebookConversations(notebook.id, 12)) ?? [];
    } catch {
      notebookConvs = [];
    }
    if (notebookConvs.length > 0) {
      // Resume where you left off with this notebook.
      askConversationId = notebookConvs[0].id;
    } else {
      // No history yet — mint a fresh scoped conversation. The scope MUST
      // be set before ChatView mounts (its consume-on-empty effect reads
      // outerWorkScopeStore when the first conversation is minted).
      outerWorkScopeStore.set([notebook.id]);
      askConversationId = null;
    }
    askVisited = true;
  }

  $effect(() => {
    if (tab === "ask" && !askVisited) void openAsk();
  });

  function selectConversation(id: string) {
    askConversationId = id;
  }
  function newConversation() {
    outerWorkScopeStore.set([notebook.id]);
    askConversationId = null;
  }

  // Move 4 (Map → Ask): from an atom in Explore, open a fresh scoped
  // question about it in this notebook's Ask tab — the conversation and
  // the map as two views of one notebook.
  function handleAskAbout(name: string) {
    outerWorkScopeStore.set([notebook.id]);
    chatSeedStore.set({
      text: `Tell me about ${name}.`,
      atom_id: "",
      source_section: null,
      question_type: "user",
    });
    askConversationId = null;
    askVisited = true;
    tab = "ask";
  }

  // I2-D: seed the Ask tab with a verbatim open question the sources
  // raise (the NotebookOpenQuestions panel). Same Map→Ask bridge as
  // `handleAskAbout`, but the chip text IS the question — no "Tell me
  // about …" wrapper.
  function handleAskQuestion(question: string) {
    outerWorkScopeStore.set([notebook.id]);
    chatSeedStore.set({
      text: question,
      atom_id: "",
      source_section: null,
      question_type: "user",
    });
    askConversationId = null;
    askVisited = true;
    tab = "ask";
  }
  function onAskConversationCreated(id: string) {
    askConversationId = id;
    // Surface the freshly-minted thread in the switcher.
    void notebookConversations(notebook.id, 12)
      .then((c) => (notebookConvs = c ?? []))
      .catch(() => {});
  }

  function threadTitle(c: ConversationEntry): string {
    return c.title?.trim() || "Untitled";
  }

  // ── Explore: make-explorable enrich flow ──────────────────────────
  // Enrichment runs IN-PROCESS in the daemon (tiered: RAPTOR + entity graph
  // + motifs) — no `sovereign-cli` subprocess (it isn't bundled with the
  // desktop and is redundant with the daemon's loaded models). We trigger it
  // via `lcEnrichNow` and poll `enrichmentStatus` for phase + fraction,
  // flipping explorable when the atlas lands.
  let enrichError = $state<string | null>(null);
  let enriching = $state(false);
  let enrichStatus = $state<EnrichmentStatus | null>(null);
  let enrichPollHandle: ReturnType<typeof setInterval> | null = null;

  function enrichPhaseLabel(phase?: string): string {
    switch (phase) {
      case "starting":
        return "Starting…";
      case "scanning":
        return "Scanning documents";
      case "entity_extraction":
        return "Finding people, places, and ideas";
      case "raptor_leaves":
        return "Summarizing sections";
      case "raptor_tree":
        return "Building the summary tree";
      case "motif_extraction":
        return "Finding recurring themes";
      case "atom_extraction":
        return "Extracting claims";
      case "persisting":
        return "Saving the map";
      default:
        return "Building…";
    }
  }

  function stopEnrichPoll() {
    if (enrichPollHandle) {
      clearInterval(enrichPollHandle);
      enrichPollHandle = null;
    }
  }

  async function pollEnrichOnce() {
    let s: EnrichmentStatus;
    try {
      s = await enrichmentStatus(notebook.id);
    } catch {
      return; // transient daemon hiccup — keep polling
    }
    enrichStatus = s;
    const phase = s.state?.phase;
    if (phase === "complete") {
      stopEnrichPoll();
      enriching = false;
      explorable = true;
      onChanged?.();
    } else if (phase === "failed") {
      stopEnrichPoll();
      enriching = false;
      enrichError = s.state?.error ?? "Enrichment failed.";
    } else if (s.is_stalled) {
      stopEnrichPoll();
      enriching = false;
      enrichError = "Enrichment stalled — no progress. Try again.";
    }
  }

  function startEnrichPoll() {
    stopEnrichPoll();
    enrichPollHandle = setInterval(() => void pollEnrichOnce(), 2000);
    void pollEnrichOnce();
  }

  async function makeExplorable() {
    enrichError = null;
    enriching = true;
    enrichStatus = null;
    try {
      // Self-healing: clear any zombie state (a prior build stuck at
      // "Preparing to build the map", or a sticky errored sweep) BEFORE
      // kicking a fresh build, so a wedged corpus rebuilds cleanly instead
      // of being blocked. Idempotent no-op on a healthy corpus.
      await lcEnrichReset(notebook.id);
      await lcEnrichNow(notebook.id);
      startEnrichPoll();
    } catch (e) {
      enrichError = e instanceof Error ? e.message : String(e);
      enriching = false;
    }
  }

  // Reflect an already-running build (a vault auto-enriching after ingest, or
  // a build still going from a prior session) without needing a click. The
  // component is re-keyed per notebook, so a one-shot probe on mount is
  // enough; it attaches the poll only when a non-terminal build is in flight.
  $effect(() => {
    if (explorable) return;
    let cancelled = false;
    void (async () => {
      try {
        const s = await enrichmentStatus(notebook.id);
        if (cancelled) return;
        const phase = s.state?.phase;
        if (
          s.state &&
          phase !== "complete" &&
          phase !== "failed" &&
          !s.is_stalled
        ) {
          enriching = true;
          enrichStatus = s;
          startEnrichPoll();
        } else if (s.state && (phase === "failed" || s.is_stalled)) {
          // A prior build died or stalled (daemon restart, crash). Surface
          // it so the CTA reads as an explicit rebuild, not a silent
          // first-run. "Make explorable" self-heals the zombie on click.
          enrichError = s.state?.error
            ? `The last build didn't finish: ${s.state.error}`
            : "The last build stopped before finishing — rebuild to try again.";
        }
      } catch {
        // No status file yet → leave the "No map yet" CTA in place.
      }
    })();
    return () => {
      cancelled = true;
      stopEnrichPoll();
    };
  });

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
  <header class="nb-header page-header" in:cardReceive={{ key: notebook.id }} out:cardSend={{ key: notebook.id }}>
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

    <div class="nb-nav">
      <!-- The two things you do with a notebook. Sources / Settings are
           rarer, so they live in the ⋯ overflow. -->
      <div class="seg">
        <button
          class="seg-btn"
          class:active={tab === "ask"}
          data-testid="notebook-tab-ask"
          onclick={() => setTab("ask")}
        >Ask</button>
        <button
          class="seg-btn"
          class:active={tab === "explore"}
          data-testid="notebook-tab-explore"
          onclick={() => setTab("explore")}
        >Explore</button>
        <!-- Conflicts appears only for a governance corpus (one with a
             governance oplog); `open_conflicts` is null otherwise. -->
        {#if notebook.open_conflicts != null}
          <button
            class="seg-btn"
            class:active={tab === "conflicts"}
            data-testid="notebook-tab-conflicts"
            onclick={() => setTab("conflicts")}
          >Conflicts{#if notebook.open_conflicts > 0}<span class="seg-count">{notebook.open_conflicts}</span>{/if}</button>
        {/if}
      </div>

      {#if tab === "ask" && notebookConvs.length > 0}
        <div class="menu-anchor">
          <button
            class="menu-trigger"
            data-testid="notebook-conv-menu"
            aria-expanded={convMenuOpen}
            onclick={() => (convMenuOpen = !convMenuOpen)}
          >
            Conversations
            <svg class="chev" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m6 9 6 6 6-6" /></svg>
          </button>
          {#if convMenuOpen}
            <button class="menu-backdrop" aria-label="Close menu" onclick={() => (convMenuOpen = false)}></button>
            <div class="menu-pop right" data-testid="notebook-ask-history">
              <button
                class="menu-item fresh"
                data-testid="notebook-ask-new"
                onclick={() => { newConversation(); convMenuOpen = false; }}
              >+ New conversation</button>
              {#each notebookConvs as c (c.id)}
                <button
                  class="menu-item"
                  class:active={c.id === askConversationId}
                  data-testid="notebook-ask-thread"
                  title={threadTitle(c)}
                  onclick={() => { selectConversation(c.id); convMenuOpen = false; }}
                >{threadTitle(c)}</button>
              {/each}
            </div>
          {/if}
        </div>
      {/if}

      <div class="menu-anchor">
        <button
          class="menu-trigger icon"
          data-testid="notebook-more"
          aria-label="More"
          aria-expanded={moreMenuOpen}
          onclick={() => (moreMenuOpen = !moreMenuOpen)}
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="1" /><circle cx="19" cy="12" r="1" /><circle cx="5" cy="12" r="1" /></svg>
        </button>
        {#if moreMenuOpen}
          <button class="menu-backdrop" aria-label="Close menu" onclick={() => (moreMenuOpen = false)}></button>
          <div class="menu-pop right">
            <button
              class="menu-item"
              class:active={tab === "sources"}
              data-testid="notebook-tab-sources"
              onclick={() => setTab("sources")}
            >Sources</button>
            <button
              class="menu-item"
              class:active={tab === "settings"}
              data-testid="notebook-tab-settings"
              onclick={() => setTab("settings")}
            >Settings</button>
          </div>
        {/if}
      </div>
    </div>
  </header>

  <div class="nb-body">
    <!-- Ask: keep-alive layer so the conversation survives tab hops. -->
    {#if askVisited}
      <div class="ask-layer" class:hidden={tab !== "ask"} aria-hidden={tab !== "ask"}>
        <div class="ask-chat">
          <ChatView
            conversationId={askConversationId}
            taskSteps={[]}
            onClearTask={() => {}}
            onConversationCreated={onAskConversationCreated}
            hideScope={true}
          />
        </div>
      </div>
    {/if}

    {#if tab === "explore"}
      {#if explorable}
        <div class="explore-surface">
          <NotebookOpenQuestions corpusId={notebook.id} onAsk={handleAskQuestion} />
          <AtlasSurface startingCorpusId={notebook.id} onAskAbout={handleAskAbout} />
        </div>
      {:else if enriching}
        {@const frac = enrichStatus?.fraction_complete ?? 0}
        <div class="pad page-body page-measure">
          <h2>Building the map…</h2>
          <p class="lede">
            Reading {notebook.name} to extract its entities, claims, and
            connections. You can keep using the rest of the app — this runs
            in the background.
          </p>
          <div class="enrich-progress">
            <div class="enrich-phase">
              {enrichPhaseLabel(enrichStatus?.state?.phase)}
              {#if frac > 0}
                <span class="enrich-pct">{Math.round(frac * 100)}%</span>
              {/if}
            </div>
            <div class="enrich-bar">
              <div
                class="enrich-fill"
                style:width={`${Math.max(frac * 100, 2)}%`}
              ></div>
            </div>
            {#if enrichStatus?.state?.message}
              <p class="enrich-msg">{enrichStatus.state.message}</p>
            {/if}
          </div>
        </div>
      {:else}
        <div class="pad empty page-body page-measure">
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
            {#if enriching}Starting…{:else if enrichError}Rebuild the map{:else}Make
              explorable{/if}
          </button>
        </div>
      {/if}
    {:else if tab === "conflicts"}
      <ConflictsPanel
        corpusId={notebook.id}
        notebookName={notebook.name}
        onChanged={onChanged}
      />
    {:else if tab === "sources"}
      <div class="pad page-body page-measure">
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
      <div class="pad page-body page-measure">
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
    padding-block: 12px 10px;
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
    flex: 1;
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

  /* The notebook's nav lives in the header now: a segmented Ask|Explore
     toggle + a ⋯ overflow, instead of a separate tab bar. */
  .nb-nav {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }
  .seg {
    display: inline-flex;
    padding: 2px;
    gap: 2px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 999px;
  }
  .seg-btn {
    font: inherit;
    cursor: pointer;
    padding: 4px 14px;
    border-radius: 999px;
    border: 1px solid transparent;
    background: transparent;
    color: var(--text-secondary);
    font-weight: 550;
    font-size: 0.82rem;
  }
  .seg-btn:hover { color: var(--text-primary); }
  .seg-btn.active {
    background: var(--bg-elevated);
    color: var(--text-primary);
    border-color: color-mix(in oklch, var(--accent) 35%, var(--border));
    box-shadow: 0 1px 2px rgb(0 0 0 / 0.06);
  }
  /* Open-conflict count on the Conflicts tab — a small warning-tinted pill. */
  .seg-count {
    margin-left: 6px;
    padding: 0 6px;
    border-radius: 999px;
    font-size: 0.72rem;
    font-weight: 650;
    background: color-mix(in oklch, var(--error) 20%, transparent);
    color: color-mix(in oklch, var(--error) 80%, var(--text-primary));
  }

  .menu-anchor { position: relative; display: inline-flex; }
  .menu-trigger {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font: inherit;
    font-size: 0.82rem;
    font-weight: 500;
    color: var(--text-secondary);
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius);
    padding: 5px 10px;
    cursor: pointer;
  }
  .menu-trigger:hover { color: var(--text-primary); background: var(--bg-elevated); }
  .menu-trigger.icon { padding: 5px 7px; }
  .menu-trigger .chev { opacity: 0.7; }

  /* A full-viewport click-catcher so clicking anywhere dismisses the
     menu (the popover idiom the inner-work drawer uses). */
  .menu-backdrop {
    position: fixed;
    inset: 0;
    z-index: 40;
    background: transparent;
    border: none;
    cursor: default;
  }
  .menu-pop {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: 41;
    min-width: 200px;
    max-width: 280px;
    max-height: 60vh;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    padding: 5px;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: 0 12px 32px rgb(0 0 0 / 0.18);
  }
  .menu-pop.right { left: auto; right: 0; }
  .menu-item {
    text-align: left;
    font: inherit;
    font-size: 0.82rem;
    color: var(--text-secondary);
    background: transparent;
    border: none;
    border-radius: 6px;
    padding: 7px 10px;
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .menu-item:hover { background: var(--bg-secondary); color: var(--text-primary); }
  .menu-item.active {
    color: var(--text-primary);
    background: color-mix(in oklch, var(--accent) 12%, transparent);
  }
  .menu-item.fresh {
    color: var(--accent);
    font-weight: 550;
    border-bottom: 1px solid var(--border);
    border-radius: 6px 6px 0 0;
    margin-bottom: 3px;
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

  /* The Ask tab's conversation fills the layer; the thread switcher
     lives in the header dropdown, not a band here. */
  .ask-chat {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
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

  /* `.pad` carries `.page-body .page-measure` (app.css). It used to be
     left-aligned at max-width 760 while the Conflicts tab was centred at
     860 and Explore centred at 920, so the content column jumped
     horizontally every time you switched tabs of the same notebook. */
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

  .enrich-progress {
    margin-top: 20px;
    max-width: 520px;
  }
  .enrich-phase {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    font-size: 0.88rem;
    color: var(--text-secondary);
    margin-bottom: 8px;
  }
  .enrich-pct {
    font-size: 0.8rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
  .enrich-bar {
    height: 6px;
    background: var(--border);
    border-radius: 3px;
    overflow: hidden;
  }
  .enrich-fill {
    height: 100%;
    background: var(--accent);
    border-radius: 3px;
    transition: width 0.4s ease;
  }
  .enrich-msg {
    margin: 8px 0 0;
    font-size: 0.78rem;
    color: var(--text-muted);
  }
</style>
