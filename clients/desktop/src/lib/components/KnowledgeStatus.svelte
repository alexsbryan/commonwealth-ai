<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { listCorpora, installCorpus, removeCorpus, pauseCorpus, buildCorpusIndex, getCorpusHealth, retryEnrichmentFailures, expandCorpus, canExpandCorpus, startLayeredSetup, newsworthyStatus, newsworthyTickNow, meshAssistStart, corpusGetRecipeParameters, type NewsworthyStatus } from "../api";
  import RecipeParameterForm from "./library/RecipeParameterForm.svelte";
  import { corpusProgressStore, isTerminalPhase } from "../stores/corpusProgress.svelte";
  import { assistProgressStore } from "../stores/assistProgress.svelte";
  import PeerAssistOffer from "./mesh/PeerAssistOffer.svelte";
  import AssistProgressPanel from "./mesh/AssistProgressPanel.svelte";
  import type { CorpusEntry, CorpusHealthDetail } from "../types";
  import {
    formatRelativeAgo,
    catalogTier,
    formatDate,
    phaseLabel,
  } from "./knowledgeStatusFormat";

  let corpora: CorpusEntry[] = $state([]);
  // Progress payloads come from the singleton `corpusProgressStore` —
  // don't attach a second listener here. The store's `byId` record
  // is reactive; `$derived` picks up every progress event.
  let progress = $derived(corpusProgressStore.byId);
  let expanded: Set<string> = $state(new Set());
  let health: Record<string, CorpusHealthDetail> = $state({});
  let repairing: Set<string> = $state(new Set());
  let building: Set<string> = $state(new Set());
  let unlistenBuildComplete: UnlistenFn | null = null;
  let unlistenBuildError: UnlistenFn | null = null;

  /// Watcher state for `wikipedia-newsworthy`. Polled when a corpus
  /// has Newsworthy as a child layer, so the chip can render a
  /// glassbox status line (role, last tick, tracked count) instead
  /// of leaving the user guessing whether the daemon is doing
  /// anything.
  let newsworthy: NewsworthyStatus | null = $state(null);
  let newsworthyPollHandle: number | null = null;

  async function refreshNewsworthy() {
    try {
      newsworthy = await newsworthyStatus();
    } catch (e) {
      console.warn("newsworthyStatus failed:", e);
    }
  }

  let tickInFlight = $state(false);
  async function runNewsworthyTickNow() {
    if (tickInFlight) return;
    tickInFlight = true;
    try {
      await newsworthyTickNow();
      // Poll for snapshot refresh — tick runs async. Watcher
      // publishes once after portal ingest (fast) and again after
      // step-B fetches (can take minutes at the 1 req/s MediaWiki
      // rate). Watch both: stop polling when observed_at advances
      // OR five minutes pass — whichever first. The per-poll cost
      // is one cheap KV scan, so 1.5s cadence over 5 min is fine.
      const before = newsworthy?.last_tick?.observed_at ?? 0;
      const start = Date.now();
      while (Date.now() - start < 5 * 60_000) {
        await new Promise((r) => setTimeout(r, 1500));
        await refreshNewsworthy();
        if ((newsworthy?.last_tick?.observed_at ?? 0) > before) break;
      }
    } catch (e) {
      console.warn("newsworthyTickNow failed:", e);
    } finally {
      tickInFlight = false;
    }
  }

  // The backend (`list_corpora` Tauri command) returns the full catalog
  // from `corpus_engine::builtin_corpora()` — there's no longer a fallback
  // path because the catalog ships in Rust source, not a sidecar TOML.

  let installedCount = $derived(
    corpora.filter((c) => c.status === "installed").length,
  );
  let anyInstalling = $derived(
    corpora.some(
      (c) =>
        c.status === "installing" ||
        (progress[c.id] && !isTerminalPhase(progress[c.id].phase)),
    ),
  );

  // Top-level picker rows: drop layers (children with parent_corpus_id —
  // rendered under their parent's row) and internal collaborative-ingest
  // partitions (`<corpus>-partition-<self>` directories that flow through
  // to listInstalledIndexes by accident). Keeps the user-facing list to
  // one row per logical corpus family.
  let isPartition = (id: string): boolean =>
    /^.+-partition-(?:node-[0-9a-f]+|self)$/.test(id);
  let topLevelCorpora = $derived(
    corpora.filter(
      (c) =>
        !c.parent_corpus_id &&
        !isPartition(c.id) &&
        catalogTier(c.catalog_status) !== "hidden",
    ),
  );
  let featuredCorpora = $derived(
    topLevelCorpora.filter((c) => catalogTier(c.catalog_status) === "featured"),
  );
  // Coming-soon rail: every other top-level recipe, sorted by name.
  // Install actions render as disabled so users can see what's on the
  // roadmap without crashing into half-built ingest pipelines.
  let comingSoonCorpora = $derived(
    topLevelCorpora
      .filter((c) => catalogTier(c.catalog_status) === "preview")
      .slice()
      .sort((a, b) => a.name.localeCompare(b.name)),
  );
  // Group children by parent_corpus_id so each parent row can render
  // its add-on toggles in a sub-panel. Excludes:
  //  - `wikipedia-fetched`: a byproduct of the Catalog add-on (filled
  //    by on-demand fetches), not something the user toggles. Its count
  //    is surfaced as status under the Catalog chip instead.
  //  - hidden corpora (e.g. the on-demand `wikipedia-article` recipe).
  let childrenByParent: Record<string, typeof corpora> = $derived.by(() => {
    const map: Record<string, typeof corpora> = {};
    for (const c of corpora) {
      if (
        c.parent_corpus_id &&
        c.id !== "wikipedia-fetched" &&
        catalogTier(c.catalog_status) !== "hidden"
      ) {
        (map[c.parent_corpus_id] ||= []).push(c);
      }
    }
    return map;
  });

  // The on-demand fetch corpus the Catalog add-on populates. Surfaced
  // as a count under the Catalog chip ("N articles fetched").
  let wikipediaFetched = $derived(
    corpora.find((c) => c.id === "wikipedia-fetched"),
  );

  onMount(async () => {
    await refresh();
    await corpusProgressStore.init();
    // Newsworthy is the only watcher-driven corpus today; poll its
    // status modestly so the chip's secondary line stays fresh.
    // Cheap read — daemon route is a pair of KV scans.
    await refreshNewsworthy();
    newsworthyPollHandle = window.setInterval(refreshNewsworthy, 30_000);
    // The store handles incoming progress events; we still want to
    // refetch the full corpus list on terminal transitions so
    // `corpora.status` reflects the new installed/not_installed state.
    // Watch the store for those and debounce-refresh.
    $effect.root(() => {
      $effect(() => {
        const entries = Object.values(corpusProgressStore.byId);
        if (entries.some((p) => isTerminalPhase(p.phase))) {
          refresh();
        }
      });
      return () => {};
    });
    unlistenBuildComplete = await listen<{ corpus_id: string }>(
      "index-build-complete",
      (event) => {
        const { corpus_id } = event.payload;
        building = new Set([...building].filter((i) => i !== corpus_id));
        refresh();
      },
    );
    unlistenBuildError = await listen<{ corpus_id: string; error: string }>(
      "index-build-error",
      (event) => {
        const { corpus_id } = event.payload;
        building = new Set([...building].filter((i) => i !== corpus_id));
        console.error("Index build failed for", corpus_id, event.payload.error);
      },
    );
  });

  onDestroy(() => {
    if (unlistenBuildComplete) unlistenBuildComplete();
    if (unlistenBuildError) unlistenBuildError();
    if (newsworthyPollHandle !== null) window.clearInterval(newsworthyPollHandle);
  });

  async function refresh() {
    try {
      corpora = await listCorpora();
      // Probe expand-affordance after corpus list refresh — cheap
      // local file reads, runs in parallel with the next render.
      refreshExpandable();
    } catch (e) {
      console.error("Failed to list corpora:", e);
      corpora = [];
    }
  }

  /// The corpus whose install-time parameter form is open, if any.
  /// A recipe that declares `[parameters]` cannot be installed by
  /// clicking Install alone — the acquirer would receive the literal
  /// `{ticker}` — so the click opens the form and the FORM installs.
  let parameterForm: string | null = $state(null);

  async function handleInstall(id: string) {
    try {
      // Installing Wikipedia gives the curated Core (Vital Articles).
      // Newsworthy and Catalog are opt-in add-on toggles, not part of
      // the base install; Simple English is parked in "Coming soon".
      // `startLayeredSetup` is the (now Core-only) "install Wikipedia"
      // entry point — idempotent on the daemon side.
      if (id === "wikipedia") {
        await startLayeredSetup();
      } else {
        // Ask the recipe what it needs before assuming it needs
        // nothing. Local registry read, no daemon round trip, and it
        // is what makes the form GENERIC: the catalog knows nothing
        // about tickers, only about parameters.
        //
        // The form opens when a parameter is REQUIRED and carries NO
        // default — that is exactly the condition under which the
        // install cannot proceed without the user, and the plain
        // path would reach the acquirer with an un-interpolated
        // `{ticker}`. Recipes whose parameters all carry defaults
        // (us-code, scotus-opinions, …) keep installing on one click,
        // unchanged. Once the form IS open it renders every declared
        // parameter, so a defaulted one like sec-filings-company's
        // `contact` stays visible and editable rather than being sent
        // on the user's behalf without their seeing it.
        //
        // An unreadable schema falls through to the plain install:
        // it means the registry cannot resolve this recipe at all, and
        // that install fails with a message the `install-failed` card
        // renders. Warned here so the cause is greppable rather than
        // presenting as a mysterious install failure.
        let needsInput = false;
        try {
          const schema = await corpusGetRecipeParameters(id);
          needsInput = schema.parameters.some(
            (p) => p.required && (p.default === null || p.default === undefined),
          );
        } catch (e) {
          console.warn(`recipe parameters unreadable for '${id}':`, e);
        }
        if (needsInput) {
          parameterForm = id;
          return;
        }
        await installCorpus(id);
      }
      corpora = corpora.map((c) =>
        c.id === id ? { ...c, status: "installing" as const } : c,
      );
    } catch (e) {
      console.error("Install failed:", e);
    }
  }

  /// The daemon accepted a parameterized install: close the form and
  /// flip the row the same way the plain path does, so progress
  /// renders through the shared `corpus-progress` store.
  function handleParameterizedInstalled(id: string) {
    parameterForm = null;
    corpora = corpora.map((c) =>
      c.id === id ? { ...c, status: "installing" as const } : c,
    );
  }

  async function handleRemove(id: string) {
    try {
      await removeCorpus(id);
      await refresh();
    } catch (e) {
      console.error("Remove failed:", e);
    }
  }

  async function handlePause(id: string) {
    try {
      await pauseCorpus(id);
      await refresh();
    } catch (e) {
      console.error("Pause failed:", e);
    }
  }

  /// Tracks per-corpus "this corpus has a relaxable filter scope" so
  /// we render the "Expand to full" affordance. Populated lazily
  /// after `refresh()` resolves; the probe reads `_corpus_meta.json`
  /// directly so it's cheap.
  let expandable: Set<string> = $state(new Set());

  async function refreshExpandable() {
    const next = new Set<string>();
    for (const c of corpora) {
      if (c.status !== "installed") continue;
      try {
        if (await canExpandCorpus(c.id)) next.add(c.id);
      } catch (_) {
        // probe failure shouldn't block the rest of the UI
      }
    }
    expandable = next;
  }

  async function handleExpand(id: string) {
    try {
      await expandCorpus(id);
      // Optimistic flip — the corpus-progress poller will refresh
      // status as the expansion runs.
      corpora = corpora.map((c) =>
        c.id === id ? { ...c, status: "installing" as const } : c,
      );
    } catch (e) {
      console.error("Expand failed:", e);
    }
  }


  async function toggleHealth(id: string) {
    if (expanded.has(id)) {
      expanded.delete(id);
      expanded = new Set(expanded);
    } else {
      expanded.add(id);
      expanded = new Set(expanded);
      if (!health[id]) {
        try {
          const detail = await getCorpusHealth(id);
          if (detail) health = { ...health, [id]: detail };
        } catch (e) {
          console.error("Failed to load corpus health:", e);
        }
      }
    }
  }

  async function handleRepair(id: string) {
    repairing = new Set([...repairing, id]);
    try {
      await retryEnrichmentFailures(id);
      // Refresh health so the failure count and claims count update.
      const detail = await getCorpusHealth(id);
      if (detail) health = { ...health, [id]: detail };
    } catch (e) {
      console.error("Repair failed:", e);
    } finally {
      repairing = new Set([...repairing].filter((x) => x !== id));
    }
  }

  async function handleBuildIndex(id: string) {
    building = new Set([...building, id]);
    try {
      await buildCorpusIndex(id);
    } catch (e) {
      console.error("Build index failed:", e);
      building = new Set([...building].filter((i) => i !== id));
    }
  }

  // Peer-assist ("Blanket") on installed recipe corpora. The offer
  // self-guards — it renders nothing unless the corpus is grantable AND a
  // compatible peer is online — so mounting it per row is safe; it only
  // surfaces for user-file recipes that opted in. Decisions are keyed by
  // corpus id since several rows can be expanded at once.
  const STANDING_ASSIST_TTL_SECS = 24 * 60 * 60; // backend caps at 24h
  let assistDecisions: Record<
    string,
    { enabled: boolean; peerNodeIds: string[] }
  > = $state({});
  let assistStarting: Set<string> = $state(new Set());
  let assistErrors: Record<string, string> = $state({});

  async function startAssist(id: string) {
    const decision = assistDecisions[id];
    if (!decision?.enabled || decision.peerNodeIds.length === 0) return;
    assistStarting = new Set([...assistStarting, id]);
    assistErrors = { ...assistErrors, [id]: "" };
    try {
      const handle = await meshAssistStart(
        id,
        decision.peerNodeIds,
        STANDING_ASSIST_TTL_SECS,
      );
      assistProgressStore.track({
        corpus_id: handle.corpus_id,
        handoff_id: handle.handoff_id,
        grant_expires_at_ms: handle.grant_expires_at_ms,
      });
    } catch (e) {
      assistErrors = { ...assistErrors, [id]: String(e) };
    }
    assistStarting = new Set([...assistStarting].filter((x) => x !== id));
  }

</script>

<div class="knowledge-status">
  {#each featuredCorpora as corpus}
    {@const inProgress =
      corpus.status === "installing" ||
      (progress[corpus.id] && !isTerminalPhase(progress[corpus.id].phase))}
    <div class="corpus-row">
      <div class="corpus-info">
        <div class="corpus-name">
          {#if corpus.status === "installed" && corpus.vector_index_ready}
            <span class="dot installed" title="Semantic search ready"></span>
          {:else if corpus.status === "installed"}
            <span class="dot fts-only" title="Keyword search only — semantic index not built"></span>
          {:else if inProgress}
            <span class="dot installing"></span>
          {:else}
            <span class="dot"></span>
          {/if}
          {corpus.name}
          {#if corpus.enrichment_enabled}
            <span class="enrichment-pill" title="Can be explored as a map of people, claims, and connections">✦ explorable</span>
          {/if}
        </div>
        <div class="corpus-detail">
          {#if corpus.status === "installed"}
            <!-- An installed corpus can still have a FAILED operation
                 against it — most often "Expand to full". That failure
                 has nowhere else to appear: this branch wins over the
                 not-installed one below, and the progress banner only
                 renders non-terminal phases. Without this the expand
                 button just silently does nothing. -->
            {#if progress[corpus.id]?.phase === "failed"}
              <div class="install-failed" data-testid="operation-failed">
                <span class="failed-title">Last operation failed</span>
                {#if progress[corpus.id].message}
                  <p class="failed-reason">{progress[corpus.id].message}</p>
                {/if}
                <div class="failed-actions">
                  <button
                    class="btn-dismiss"
                    onclick={() => corpusProgressStore.dismiss(corpus.id)}
                  >
                    Dismiss
                  </button>
                </div>
              </div>
            {/if}
            {#if !corpus.vector_index_ready}
              <div class="fts-only-notice">
                <span>Keyword search only</span>
                <button
                  class="btn-build-index"
                  onclick={() => handleBuildIndex(corpus.id)}
                  disabled={building.has(corpus.id)}
                >
                  {building.has(corpus.id) ? "Building…" : "Improve search"}
                </button>
              </div>
            {/if}
            <button
              class="detail-toggle"
              onclick={() => toggleHealth(corpus.id)}
              title="Show index details"
            >
              {expanded.has(corpus.id) ? "▾" : "▸"}
            </button>
            Indexed
            {#if corpus.indexed_at}
              &middot; {formatDate(corpus.indexed_at)}
            {/if}
            {#if corpus.chunks_count}
              &middot; {corpus.chunks_count.toLocaleString()} passages
            {/if}
            {#if expanded.has(corpus.id)}
              <div class="health-panel">
                {#if corpus.embedding_model}
                  <span class="health-chip">{corpus.embedding_model}{corpus.embedding_dimensions ? ` (${corpus.embedding_dimensions}-dim)` : ""}</span>
                {/if}
                {#if health[corpus.id]}
                  {#if health[corpus.id].claims_count > 0}
                    <span class="health-chip enriched">✦ {health[corpus.id].claims_count.toLocaleString()} questions</span>
                  {:else if corpus.enrichment_enabled}
                    <span class="health-chip">✦ explorable (no questions yet)</span>
                  {:else}
                    <span class="health-chip muted">Not yet explorable</span>
                  {/if}
                  {#if health[corpus.id].parse_failure_count > 0}
                    <button
                      class="health-chip repair-btn"
                      disabled={repairing.has(corpus.id)}
                      onclick={() => handleRepair(corpus.id)}
                      title="{health[corpus.id].parse_failure_count.toLocaleString()} batches failed to parse during skeleton extraction — click to reprocess with improved parser"
                    >
                      {repairing.has(corpus.id)
                        ? "Reprocessing…"
                        : `Reprocess ${health[corpus.id].parse_failure_count.toLocaleString()} failures`}
                    </button>
                  {/if}
                  {#if health[corpus.id].has_article_profiles}
                    <span class="health-chip">Field skeleton</span>
                  {/if}
                {:else}
                  <span class="health-chip muted">Loading…</span>
                {/if}
              </div>
              <!-- Peer-assist offer. Self-hides unless this corpus is
                   grantable + a compatible peer is online, so it only shows
                   for user-file recipes that opted in. -->
              {#if assistProgressStore.get(corpus.id)}
                <div class="assist-slot">
                  <AssistProgressPanel
                    job={assistProgressStore.get(corpus.id)!}
                    onRevoke={(c) => assistProgressStore.revoke(c)}
                  />
                </div>
              {:else}
                <div class="assist-slot">
                  <PeerAssistOffer
                    corpusId={corpus.id}
                    surface="recipe"
                    onChange={(d) =>
                      (assistDecisions = { ...assistDecisions, [corpus.id]: d })}
                  />
                  {#if assistDecisions[corpus.id]?.enabled && assistDecisions[corpus.id].peerNodeIds.length > 0}
                    <button
                      class="action-btn install assist-start"
                      onclick={() => startAssist(corpus.id)}
                      disabled={assistStarting.has(corpus.id)}
                    >
                      {assistStarting.has(corpus.id)
                        ? "Starting…"
                        : "Get mesh help"}
                    </button>
                  {/if}
                  {#if assistErrors[corpus.id]}
                    <p class="assist-error">{assistErrors[corpus.id]}</p>
                  {/if}
                </div>
              {/if}
            {/if}
          {:else if inProgress}
            {#if progress[corpus.id]}
              {phaseLabel(progress[corpus.id].phase)}
              {#if progress[corpus.id].percent > 0}
                · {progress[corpus.id].percent.toFixed(0)}%
              {/if}
              {#if progress[corpus.id].message}
                · {progress[corpus.id].message}
              {/if}
            {:else}
              Starting…
            {/if}
          {:else if progress[corpus.id]?.phase === "failed"}
            <!-- A failed install used to be invisible here: the daemon
                 only logged it, the corpus dropped out of the status
                 snapshot, and the poller reported the disappearance as
                 "Done". Now the reason is shown in place, with the
                 remedy when the engine could name one. -->
            <div class="install-failed" data-testid="install-failed">
              <span class="failed-title">Install failed</span>
              {#if progress[corpus.id].message}
                <p class="failed-reason">{progress[corpus.id].message}</p>
              {/if}
              <div class="failed-actions">
                <button
                  class="action-btn install"
                  onclick={() => handleInstall(corpus.id)}
                >
                  Try again
                </button>
                <button
                  class="btn-dismiss"
                  onclick={() => corpusProgressStore.dismiss(corpus.id)}
                >
                  Dismiss
                </button>
              </div>
            </div>
          {:else}
            <span title={corpus.description}>
              ~{corpus.size_compressed_gb} GB download · ~{corpus.size_indexed_gb} GB indexed
            </span>
          {/if}
        </div>
        {#if childrenByParent[corpus.id]?.length}
          <div class="corpus-layers" data-testid="corpus-layers">
            <span class="layers-label">Add-ons:</span>
            {#each childrenByParent[corpus.id] as layer}
              {@const layerInProgress =
                layer.status === "installing" ||
                (progress[layer.id] && !isTerminalPhase(progress[layer.id].phase))}
              {@const layerInstalled = layer.status === "installed"}
              <button
                type="button"
                class="layer-chip"
                class:installed={layerInstalled}
                class:installing={layerInProgress}
                class:available={!layerInstalled && !layerInProgress}
                data-testid="layer-chip"
                data-layer-id={layer.id}
                data-layer-status={layerInProgress ? "installing" : layer.status}
                disabled={layerInProgress}
                aria-pressed={layerInstalled}
                aria-label="{layerInstalled
                  ? `Remove ${layer.name} layer`
                  : layerInProgress
                    ? `${layer.name} layer is installing`
                    : `Add ${layer.name} layer`}"
                title={layer.description}
                onclick={() =>
                  layerInstalled ? handleRemove(layer.id) : handleInstall(layer.id)}
              >
                <span class="layer-dot" aria-hidden="true"></span>
                <span class="layer-name">{layer.name}</span>
                <span class="layer-action" aria-hidden="true">
                  {#if layerInstalled}
                    Remove
                  {:else if layerInProgress}
                    Installing…
                  {:else}
                    Add
                  {/if}
                </span>
              </button>
            {/each}
          </div>

          <!-- Per-layer status detail. Lives outside `.corpus-layers`
               so the chips row stays uniform horizontally — pre-fix,
               an installed newsworthy chip was a tall stacked column
               (chip + status block) while siblings stayed single-line
               and the `align-items: center` row centred them against
               the taller wrap, pushing them visually lower. Status
               now drops underneath the row at its own rhythm. -->
          {#each childrenByParent[corpus.id] as layer}
            {#if layer.id === "wikipedia-newsworthy" && layer.status === "installed" && newsworthy}
              {@const lt = newsworthy.last_tick}
              {@const selfIsLeader =
                newsworthy.self_in_pool &&
                newsworthy.leader_node_id === lt?.node_id_str}
              {@const installWarnLive =
                !newsworthy.local_corpus_installed &&
                newsworthy.installed_peer_count === 0}
              <div class="layer-status" data-testid="newsworthy-status">
                <span class="layer-status-label">Newsworthy</span>
                {#if lt}
                  {#if selfIsLeader}
                    <span class="status-role status-leader" title="This node fetches Portal:Current_events on each tick and writes the daily page into the wikipedia-newsworthy corpus. Followers refresh tracked articles into the parent `wikipedia` corpus.">
                      you are leader
                    </span>
                  {:else if newsworthy.leader_node_id}
                    <span class="status-role" title="Election picks the lowest NodeId among peers that have wikipedia-newsworthy installed. The leader writes the daily portal page; this node is a follower and will refresh tracked articles into the parent wikipedia corpus when there are any.">
                      follower · leader {newsworthy.leader_node_id.slice(0, 16)}…
                    </span>
                  {:else}
                    <span class="status-role status-warn" title="No online peer has wikipedia-newsworthy installed. Daily portal ingest is paused mesh-wide until at least one node installs it.">
                      no leader — no peer has it installed
                    </span>
                  {/if}
                  <span class="status-sep">·</span>
                  <span title="Time since the watcher's last completed tick. Default interval is 24h; use 'Run tick now' to bypass.">
                    last tick {formatRelativeAgo(lt.observed_at)}
                  </span>
                  <span class="status-sep">·</span>
                  <span title="Articles tracked under the 30-day rolling window. Leader writes this set after each portal ingest.">
                    {lt.tracked_total} tracked
                  </span>
                  {#if installWarnLive}
                    <span class="status-warn">
                      · install incomplete locally — click Add again to repair
                    </span>
                  {/if}
                  {#if selfIsLeader && !lt.portal_ingested && lt.tracked_total === 0}
                    <button
                      type="button"
                      class="tick-now-btn"
                      disabled={tickInFlight}
                      onclick={runNewsworthyTickNow}
                      title="Fire one watcher tick now. Fetches yesterday's Portal:Current_events, writes it to wikipedia-newsworthy, seeds the tracked-article set."
                    >
                      {tickInFlight ? "Running tick…" : "Run tick now →"}
                    </button>
                  {/if}
                {:else}
                  <span class="status-pending">
                    watcher starting — first tick lands within ~15 min
                  </span>
                {/if}
              </div>
            {/if}
            {#if layer.id === "wikipedia-catalog" && layer.status === "installed"}
              <div class="layer-status" data-testid="catalog-status">
                <span class="layer-status-label">Catalog</span>
                <span title="Indexes article titles + abstracts so any of ~6.8M Wikipedia articles can be fetched in full on demand. Fetched articles accumulate in the shared wikipedia-fetched corpus and are reused after the first fetch.">
                  fetch any article on demand
                </span>
                {#if wikipediaFetched && (wikipediaFetched.chunks_count ?? 0) > 0}
                  <span class="status-sep">·</span>
                  <span title="Full-text fetched on demand so far, stored in the wikipedia-fetched corpus.">
                    {(wikipediaFetched.chunks_count ?? 0).toLocaleString()} chunks fetched
                  </span>
                {/if}
              </div>
            {/if}
          {/each}
        {/if}
      </div>

      <div class="corpus-action">
        {#if corpus.status === "installed"}
          {#if expandable.has(corpus.id)}
            <button
              class="action-btn expand"
              title="Expand to the full corpus by relaxing the active filter scope. The existing index is preserved; only newly-accepted documents are embedded."
              onclick={() => handleExpand(corpus.id)}
            >
              Expand to full →
            </button>
          {/if}
          <button class="action-btn remove" onclick={() => handleRemove(corpus.id)}>
            Remove
          </button>
        {:else if inProgress}
          <div class="inprogress-controls">
            {#if progress[corpus.id]?.percent > 0}
              <div class="progress-bar">
                <div
                  class="progress-fill"
                  style="width: {progress[corpus.id].percent}%"
                ></div>
              </div>
            {:else}
              <span class="status-text">Working…</span>
            {/if}
            <!--
              In-progress "Pause" calls `pauseCorpus` → daemon
              `/internal/corpus/pause`, which signals the ingest's
              cancellation flag and waits for the task to exit cleanly
              but does NOT wipe on-disk state. Committed chunks and
              `_corpus_meta.json` are preserved so clicking Install
              again resumes from the last flush. The destructive
              variant ("Remove") only appears once a corpus is
              installed; that path goes through `removeCorpus` →
              `/internal/corpus/cancel` with `confirm_wipe: true`.
            -->
            <button
              class="action-btn cancel"
              onclick={() => handlePause(corpus.id)}
              title="Stop this ingest. Committed data is kept so you can resume by clicking Install again."
            >
              Pause
            </button>
          </div>
        {:else}
          <button
            class="action-btn install"
            data-testid="corpus-install"
            data-corpus-id={corpus.id}
            onclick={() => handleInstall(corpus.id)}
          >
            Install
          </button>
        {/if}
      </div>
    </div>
    {#if parameterForm === corpus.id}
      <!-- The install-time form for a recipe that cannot be installed
           without user input. Rendered from the recipe's own
           `[parameters]` schema; see RecipeParameterForm.svelte. -->
      <RecipeParameterForm
        corpusId={corpus.id}
        corpusName={corpus.name}
        onInstalled={() => handleParameterizedInstalled(corpus.id)}
        onCancel={() => (parameterForm = null)}
      />
    {/if}
  {/each}

  {#if comingSoonCorpora.length > 0}
    <div class="coming-soon-section" data-testid="coming-soon-section">
      <h4 class="coming-soon-title">Coming soon</h4>
      <p class="coming-soon-blurb">Recipes on the bench. Hover any one for details.</p>
      <div class="coming-soon-grid">
        {#each comingSoonCorpora as corpus}
          <div
            class="cs-card"
            aria-disabled="true"
            title={corpus.description}
          >
            <div class="cs-card-head">
              <span class="dot"></span>
              <span class="cs-card-name">{corpus.name}</span>
              {#if corpus.enrichment_enabled}
                <span class="cs-enrich" title="Includes claim/relationship enrichment when ready">✦</span>
              {/if}
            </div>
            <div class="cs-card-meta">
              {corpus.size_indexed_gb < 1
                ? `${Math.round(corpus.size_indexed_gb * 1024)} MB`
                : `${corpus.size_indexed_gb} GB`} indexed
            </div>
          </div>
        {/each}
      </div>
    </div>
  {/if}

</div>

<style>
  .knowledge-status {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .coming-soon-section {
    margin-top: 20px;
    padding-top: 14px;
    border-top: 1px dashed var(--border);
  }
  .coming-soon-title {
    font-size: 0.66rem;
    font-weight: 600;
    color: var(--lavender);
    text-transform: uppercase;
    letter-spacing: 0.12em;
    margin: 0 0 4px;
  }
  .coming-soon-blurb {
    font-size: 0.78rem;
    color: var(--text-muted);
    margin: 0 0 10px;
    line-height: 1.5;
  }
  /* Two-column grid of compact cards. Replaces the dense full-width
     vertical stack that listed every preview recipe with its full
     description — at 11 entries that scrolled forever. Cards show
     name + size only; full description lives in the tooltip. */
  .coming-soon-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 6px;
  }
  .cs-card {
    padding: 8px 10px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    opacity: 0.7;
    transition: opacity 120ms ease, border-color 120ms ease;
    cursor: help;
    min-width: 0;
  }
  .cs-card:hover {
    opacity: 1;
    border-color: var(--border-mid);
  }
  .cs-card-head {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 0.8rem;
    font-weight: 500;
    color: var(--text-secondary);
    min-width: 0;
  }
  .cs-card-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cs-enrich {
    color: var(--accent-light);
    font-size: 0.78rem;
    flex-shrink: 0;
  }
  .cs-card-meta {
    margin-top: 2px;
    font-size: 0.7rem;
    color: var(--text-muted);
    font-family: var(--font-mono);
  }
  .action-btn:disabled {
    cursor: not-allowed;
    opacity: 0.7;
  }
  .corpus-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 0;
    border-bottom: 1px solid var(--border);
  }
  .corpus-row:last-child {
    border-bottom: none;
  }
  .corpus-info {
    flex: 1;
    min-width: 0;
  }
  .corpus-name {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.9rem;
    font-weight: 500;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-muted);
    flex-shrink: 0;
  }
  .dot.installed {
    background: var(--success, #22c55e);
  }
  .dot.fts-only {
    background: var(--warning, #e6a817);
  }
  .dot.installing {
    background: var(--accent);
    animation: pulse 1.5s infinite;
  }
  .fts-only-notice {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.75rem;
    color: var(--warning, #e6a817);
    margin-bottom: 2px;
  }
  .btn-build-index {
    padding: 1px 8px;
    font-size: 0.72rem;
    font-weight: 500;
    border: 1px solid var(--warning, #e6a817);
    color: var(--warning, #e6a817);
    border-radius: var(--radius);
    background: transparent;
    cursor: pointer;
    font-family: inherit;
    transition: opacity 0.15s;
  }
  .btn-build-index:hover:not(:disabled) {
    opacity: 0.75;
  }
  .btn-build-index:disabled {
    cursor: default;
    opacity: 0.5;
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.4;
    }
  }
  .corpus-detail {
    font-size: 0.8rem;
    color: var(--text-muted);
    margin-left: 16px;
    margin-top: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .corpus-action {
    flex-shrink: 0;
    min-width: 80px;
    text-align: right;
  }
  .action-btn {
    padding: 4px 12px;
    border-radius: var(--radius);
    font-size: 0.8rem;
    font-weight: 500;
  }
  .action-btn.install {
    background: var(--accent);
    color: var(--text-on-accent);
  }
  .action-btn.install:hover {
    background: var(--accent-hover);
  }
  .action-btn.remove {
    background: var(--bg-surface);
    color: var(--text-secondary);
    border: 1px solid var(--border);
  }
  .action-btn.remove:hover {
    border-color: var(--error, #ef4444);
    color: var(--error, #ef4444);
  }
  .action-btn.cancel {
    background: var(--bg-surface);
    color: var(--text-secondary);
    border: 1px solid var(--border);
  }
  .action-btn.cancel:hover {
    border-color: var(--error, #ef4444);
    color: var(--error, #ef4444);
  }
  .inprogress-controls {
    display: flex;
    align-items: center;
    gap: 8px;
    justify-content: flex-end;
  }
  .progress-bar {
    width: 80px;
    height: 6px;
    background: var(--bg-surface);
    border-radius: 3px;
    overflow: hidden;
  }
  .progress-fill {
    height: 100%;
    background: var(--accent);
    border-radius: 3px;
    transition: width 0.3s;
  }
  .status-text {
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .detail-toggle {
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    color: var(--text-muted);
    font-size: 0.7rem;
    line-height: 1;
    margin-right: 2px;
  }
  .detail-toggle:hover {
    color: var(--text-secondary);
  }
  .health-panel {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 4px;
    margin-left: 0;
  }
  .assist-slot {
    margin-top: 6px;
  }
  .assist-start {
    margin-top: 6px;
  }
  .assist-error {
    margin: 6px 0 0;
    font-size: 0.75rem;
    color: var(--error, #ef4444);
  }
  .install-failed {
    margin-top: 6px;
  }
  .failed-title {
    font-size: 0.75rem;
    font-weight: 500;
    color: var(--error, #ef4444);
  }
  /* The remedy text is the point of this block — it can be a couple of
     sentences (request access here, then set this env var), so it wraps
     rather than truncating. */
  .failed-reason {
    margin: 3px 0 0;
    font-size: 0.75rem;
    line-height: 1.4;
    color: var(--text-secondary);
    white-space: normal;
  }
  .failed-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 6px;
  }
  .btn-dismiss {
    background: none;
    border: none;
    padding: 0;
    font-size: 0.75rem;
    color: var(--text-muted);
    cursor: pointer;
    text-decoration: underline;
  }
  .btn-dismiss:hover {
    color: var(--text-secondary);
  }
  .health-chip {
    display: inline-block;
    font-size: 0.7rem;
    color: var(--text-secondary);
    background: var(--bg-surface);
    border: 1px solid var(--border);
    padding: 1px 7px;
    border-radius: 10px;
    white-space: nowrap;
  }
  .health-chip.enriched {
    color: var(--accent-light);
    background: var(--accent-dim);
    border-color: rgba(201, 168, 76, 0.3);
  }
  .health-chip.muted {
    color: var(--text-muted);
  }
  .health-chip.repair-btn {
    cursor: pointer;
    color: var(--warning, #e6a817);
    background: transparent;
    border-color: var(--warning, #e6a817);
    font-family: inherit;
    transition: opacity 0.15s;
  }
  .health-chip.repair-btn:hover:not(:disabled) {
    opacity: 0.8;
  }
  .health-chip.repair-btn:disabled {
    cursor: default;
    opacity: 0.6;
  }
  .enrichment-pill {
    font-size: 0.65rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--accent-light);
    background: var(--accent-dim);
    border: 1px solid rgba(201, 168, 76, 0.3);
    padding: 1px 6px;
    border-radius: 10px;
    margin-left: 6px;
    white-space: nowrap;
  }
  /* ── Layers panel — sub-corpora rendered under their parent's row.
        e.g. Wikipedia carries Simple English (Layer 0) and Newsworthy
        (Layer 2) as toggleable chips. The whole chip is the
        click target (button); the trailing label tells the user
        what will happen. ── */
  .corpus-layers {
    margin-top: 6px;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    font-size: 0.72rem;
  }
  .layers-label {
    color: var(--text-muted);
    font-weight: 500;
  }
  .layer-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 10px 3px 8px;
    border: 1px solid var(--border-muted, rgba(255, 255, 255, 0.14));
    border-radius: 999px;
    background: var(--bg-subtle, rgba(255, 255, 255, 0.04));
    color: inherit;
    font: inherit;
    cursor: pointer;
    transition: background 120ms ease, border-color 120ms ease,
      color 120ms ease, transform 80ms ease;
  }
  .layer-chip:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.08);
    border-color: rgba(255, 255, 255, 0.28);
  }
  .layer-chip:active:not(:disabled) {
    transform: translateY(1px);
  }
  .layer-chip:focus-visible {
    outline: 2px solid var(--focus-ring, #5aa9ff);
    outline-offset: 2px;
  }
  .layer-chip:disabled {
    cursor: progress;
  }
  /* Status block sits on its own row beneath the layer chips so the
     chips row stays uniform regardless of which layer is installed.
     Subtle muted indent communicates "this is detail about a chip
     above" without nesting visually inside it. */
  .layer-status {
    margin: 6px 0 0 12px;
    padding: 6px 10px;
    font-size: 0.7rem;
    color: var(--text-muted);
    background: var(--bg-secondary);
    border-left: 2px solid var(--border-mid);
    border-radius: 4px;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
    line-height: 1.4;
  }
  .layer-status-label {
    font-size: 0.62rem;
    font-weight: 600;
    color: var(--text-muted);
    letter-spacing: 0.1em;
    text-transform: uppercase;
    margin-right: 2px;
  }
  .layer-status .status-role {
    color: var(--text-secondary);
    font-weight: 500;
  }
  .layer-status .status-sep {
    opacity: 0.6;
  }
  .layer-status .status-warn {
    color: var(--warning, #e6a817);
  }
  .layer-status .status-pending {
    color: var(--text-muted);
    font-style: italic;
  }
  .layer-status .status-leader {
    color: var(--accent-light);
  }
  .tick-now-btn {
    font: inherit;
    font-size: 0.68rem;
    color: var(--accent-light);
    background: transparent;
    border: 1px solid rgba(201, 168, 76, 0.4);
    border-radius: 999px;
    padding: 1px 8px;
    margin-left: 4px;
    cursor: pointer;
  }
  .tick-now-btn:hover:not(:disabled) {
    background: rgba(201, 168, 76, 0.12);
  }
  .tick-now-btn:disabled {
    cursor: progress;
    opacity: 0.6;
  }

  /* States */
  .layer-chip.installed {
    border-color: rgba(120, 220, 140, 0.5);
    background: rgba(120, 220, 140, 0.12);
  }
  .layer-chip.installing {
    border-color: rgba(255, 200, 80, 0.55);
    background: rgba(255, 200, 80, 0.10);
  }
  .layer-chip.available {
    border-style: dashed;
  }
  .layer-chip.available:hover {
    border-style: solid;
  }

  .layer-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--text-muted);
    flex: 0 0 auto;
  }
  .layer-chip.installed .layer-dot {
    background: var(--growth);
  }
  .layer-chip.installing .layer-dot {
    background: var(--accent);
    animation: layer-pulse 1.4s ease-in-out infinite;
  }
  @keyframes layer-pulse {
    0%, 100% { opacity: 0.5; }
    50% { opacity: 1; }
  }

  .layer-name {
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .layer-action {
    color: var(--text-muted);
    font-size: 0.66rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    border-left: 1px solid var(--border-muted, rgba(255, 255, 255, 0.14));
    padding-left: 6px;
  }
  .layer-chip.installed .layer-action {
    color: rgba(120, 220, 140, 0.95);
  }
  .layer-chip.installing .layer-action {
    color: rgba(255, 200, 80, 0.95);
  }
  .layer-chip:hover:not(:disabled) .layer-action {
    color: var(--text-strong, #fff);
  }
</style>
