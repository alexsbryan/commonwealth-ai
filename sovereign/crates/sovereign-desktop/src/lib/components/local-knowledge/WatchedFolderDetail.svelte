<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import {
    lcWatchAddRoot,
    lcWatchDetails,
    lcWatchEnrichDisable,
    lcWatchEnrichEnable,
    lcWatchEnrichRebuild,
    lcWatchRemoveRoot,
    lcWatchState,
  } from "../../api";
  import type {
    WatchedFolderDetailsResponse,
    WatchedFailedFile,
  } from "../../types";
  import DocumentInspector from "./DocumentInspector.svelte";

  interface Props {
    corpusId: string;
    onClose: () => void;
  }

  let { corpusId, onClose }: Props = $props();

  let details: WatchedFolderDetailsResponse | null = $state(null);
  let docIds: string[] = $state([]);
  let loadError: string | null = $state(null);
  let busy = $state(true);
  let inspectingDocId: string | null = $state(null);
  let rootActionInflight = $state(false);
  let rootActionError: string | null = $state(null);

  // Folder-ingest v1 §3.7 — formats + skipped + failed are all on
  // the details digest. The doc list comes off the lighter `/state`
  // endpoint via `entries`, fetched in parallel so the panel
  // doesn't gate on the larger payload.
  async function reload() {
    busy = true;
    loadError = null;
    try {
      const [d, s] = await Promise.all([
        lcWatchDetails(corpusId),
        lcWatchState(corpusId),
      ]);
      details = d;
      // The state response carries `entries` keyed by doc_id when
      // requested with `?with_entries=1`; today it doesn't, so we
      // reconstruct doc ids from the live count + a small follow-up
      // server call once the `/state` route exposes it. For Phase
      // C we display the doc-count metric and leave the per-doc
      // list to a follow-up — the inspector still works when
      // `inspectingDocId` is set programmatically by other UI.
      docIds = [];
      void s;
    } catch (e) {
      loadError = String(e);
    }
    busy = false;
  }

  onMount(() => {
    void reload();
  });

  function formatRelative(unix: number): string {
    if (unix === 0) return "never";
    const now = Math.floor(Date.now() / 1000);
    const delta = Math.max(0, now - unix);
    if (delta < 60) return `${delta}s ago`;
    if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
    if (delta < 86_400) return `${Math.floor(delta / 3600)}h ago`;
    return `${Math.floor(delta / 86_400)}d ago`;
  }

  function formatSyncMode(mode: string): string {
    return mode === "manual" ? "Manual — sweeps on request" : "Continuous";
  }

  // §3.7 "What I don't have" surface: failed files grouped by
  // reason kind, surfaced as one row per group with the per-file
  // detail in a hover tooltip. We pre-aggregate so the rendering
  // doesn't have to re-walk the list per render.
  let failedByKind = $derived.by(() => {
    if (!details) return [];
    const groups = new Map<string, WatchedFailedFile[]>();
    for (const f of details.failed_files) {
      const list = groups.get(f.kind) ?? [];
      list.push(f);
      groups.set(f.kind, list);
    }
    return Array.from(groups.entries())
      .map(([kind, files]) => ({ kind, files }))
      .sort((a, b) => b.files.length - a.files.length);
  });

  let skippedSorted = $derived.by(() => {
    if (!details) return [];
    return Object.entries(details.skipped_by_extension).sort(
      (a, b) => b[1] - a[1],
    );
  });

  let formatsSorted = $derived.by(() => {
    if (!details) return [];
    return Object.entries(details.formats).sort((a, b) => b[1] - a[1]);
  });

  function reasonLabel(kind: string): string {
    switch (kind) {
      case "corrupt":
        return "Couldn't be parsed (corrupt or unreadable)";
      case "password_protected":
        return "Password-protected";
      case "scanned_no_text":
        return "Scanned PDFs with no text layer";
      case "encrypted":
        return "Encrypted";
      default:
        return kind.replace(/_/g, " ");
    }
  }

  // Folder-ingest v1 §3.1 — multi-root edit operations from the
  // detail panel. Add picks a folder via the OS dialog and posts;
  // remove deletes by additional_roots index (= idx - 1 because
  // idx 0 is the primary). Both reload the full digest on
  // success so per-root counts re-derive.
  async function addRoot() {
    rootActionInflight = true;
    rootActionError = null;
    try {
      const picked = await openDialog({ multiple: false, directory: true });
      if (typeof picked !== "string") {
        rootActionInflight = false;
        return;
      }
      await lcWatchAddRoot(corpusId, picked);
      await reload();
    } catch (e) {
      rootActionError = String(e);
    }
    rootActionInflight = false;
  }

  // Folder-ingest v1 §3.3 — enrichment lifecycle.
  //
  // Default path: in-process tiered driver (RAPTOR clusters +
  // TF-IDF motif index + GliNER chunk_entities feeding the PPR
  // multi-hop retrieval surface). Universal across corpus shapes
  // — the pipeline_id is accepted but ignored. AssetState
  // (PartiallyReady → MultiHopReady → Ready) streams back as
  // EnrichmentRuntimeStatus::Tiered events the details digest
  // projects into the existing Building/Complete/Failed shapes.
  //
  // Legacy fallback default: daemons without tiered deps installed
  // still spawn `sovereign-cli enrich build` and that path requires
  // a pipeline_id from {philosophy_atlas, referential_atlas,
  // literary_atlas}. The tiered path accepts but ignores this value.
  // Picker UI removed (universal under tiered) — hard-code the
  // legacy default here so back-compat installs still work.
  const DEFAULT_LEGACY_PIPELINE = "philosophy_atlas";

  let enrichActionInflight = $state(false);
  let enrichError: string | null = $state(null);
  let pollHandle: ReturnType<typeof setInterval> | null = null;

  // Honest cost-estimate framing per spec §3.3. The driver's
  // `CostEstimate::from_doc_count` heuristic lives in Rust; we
  // mirror the same coefficients here so the UI can render the
  // estimate before kicking off a build (no round-trip needed).
  function costEstimate(docCount: number): { lowMin: number; highMin: number } {
    if (docCount === 0) return { lowMin: 0, highMin: 0 };
    const chunksPerDoc = 5;
    const lowSecs = 30 + (docCount * chunksPerDoc * 500) / 1000;
    const highSecs = 30 + (docCount * chunksPerDoc * 1500) / 1000;
    return {
      lowMin: Math.max(1, Math.round(lowSecs / 60)),
      highMin: Math.max(1, Math.round(highSecs / 60)),
    };
  }

  async function enableEnrichment() {
    if (!details) return;
    enrichActionInflight = true;
    enrichError = null;
    try {
      await lcWatchEnrichEnable(details.corpus_id, DEFAULT_LEGACY_PIPELINE);
      await reload();
    } catch (e) {
      enrichError = String(e);
    }
    enrichActionInflight = false;
  }

  async function disableEnrichment() {
    if (!details) return;
    if (
      !window.confirm(
        "Disable atlas enrichment? The atlas data will be removed; the " +
          "folder stays searchable through standard retrieval. You can " +
          "re-enable later — the build will start from scratch.",
      )
    ) {
      return;
    }
    enrichActionInflight = true;
    enrichError = null;
    try {
      await lcWatchEnrichDisable(details.corpus_id);
      await reload();
    } catch (e) {
      enrichError = String(e);
    }
    enrichActionInflight = false;
  }

  async function rebuildEnrichment() {
    if (!details) return;
    enrichActionInflight = true;
    enrichError = null;
    try {
      await lcWatchEnrichRebuild(details.corpus_id);
      await reload();
    } catch (e) {
      enrichError = String(e);
    }
    enrichActionInflight = false;
  }

  // Live polling: while a build is in flight we refresh the
  // details digest every 2s so the progress bar advances. The
  // alternative would be subscribing to the daemon's enrich
  // progress channel directly via `listen()`, but the digest
  // already carries enough to render — and a single re-fetch is
  // cheap because the daemon serves it from in-memory state.
  $effect(() => {
    const isBuilding = details?.enrichment.kind === "building";
    if (isBuilding && !pollHandle) {
      pollHandle = setInterval(() => {
        void reload();
      }, 2000);
    } else if (!isBuilding && pollHandle) {
      clearInterval(pollHandle);
      pollHandle = null;
    }
  });

  onDestroy(() => {
    if (pollHandle) clearInterval(pollHandle);
  });

  async function removeRoot(idx: number) {
    if (idx === 0) return; // Primary root can't be removed individually
    if (
      !window.confirm(
        "Detach this folder from the corpus? Files indexed only from " +
          "this root will be soft-deleted by the next sweep (revivable " +
          "within the grace window if you re-attach).",
      )
    ) {
      return;
    }
    rootActionInflight = true;
    rootActionError = null;
    try {
      // additional_roots is 0-indexed, so subtract 1 from the
      // root-entry idx (which counts the primary as 0).
      await lcWatchRemoveRoot(corpusId, idx - 1);
      await reload();
    } catch (e) {
      rootActionError = String(e);
    }
    rootActionInflight = false;
  }
</script>

<section class="detail">
  <header class="head">
    <button class="back" onclick={onClose}>← Back to folders</button>
    {#if details}
      <h2 class="title">{details.display_name}</h2>
      <p class="path" title={details.root_path}>{details.root_path}</p>
    {/if}
  </header>

  {#if busy && !details}
    <p class="muted">Loading…</p>
  {/if}
  {#if loadError}
    <p class="error">{loadError}</p>
  {/if}

  {#if details}
    <!-- Top-line summary: how Sovereign currently sees the folder. -->
    <div class="summary">
      <div class="metric">
        <span class="metric-label">Indexed</span>
        <span class="metric-value">{details.live_entries}</span>
        <span class="metric-suffix">documents</span>
      </div>
      <div class="metric">
        <span class="metric-label">Last synced</span>
        <span class="metric-value">{formatRelative(details.last_sweep_unix)}</span>
      </div>
      <div class="metric">
        <span class="metric-label">Sync mode</span>
        <span class="metric-value subtle">{formatSyncMode(details.sync_mode)}</span>
      </div>
      <div class="metric">
        <span class="metric-label">Tombstones</span>
        <span class="metric-value subtle">{details.tombstones}</span>
        <span class="metric-suffix">in grace window</span>
      </div>
      {#if details.sensitive}
        <div class="metric sensitive-tag">
          <span class="metric-label">Sensitivity</span>
          <span class="metric-value">Sensitive</span>
          <span class="metric-suffix">
            excluded from ambient situated context
          </span>
        </div>
      {/if}
    </div>

    <!-- Folder-ingest v1 §3.1 multi-root: per-root subsection.
         Always rendered (every corpus has at least the primary
         root) so the user has one stable place to see "what
         folders feed this corpus" + add/remove additional ones. -->
    <section class="section">
      <div class="section-head">
        <h3 class="section-title">Folders</h3>
        <button
          class="ghost"
          onclick={addRoot}
          disabled={rootActionInflight}
          title="Layer another folder onto this corpus. Queries draw from all roots; identical files across roots are deduplicated."
        >
          + Add folder
        </button>
      </div>
      <ul class="root-list">
        {#each details.roots as root (root.idx)}
          <li class="root">
            <div class="root-meta">
              <code class="root-path" title={root.path}>{root.path}</code>
              <span class="root-detail">
                {root.doc_count} {root.doc_count === 1 ? "doc" : "docs"}
                {#if root.primary}
                  <span class="primary-tag" title="Primary root — this is the corpus's anchor path. Add or remove additional folders, but the primary stays.">
                    primary
                  </span>
                {:else if root.added_at_unix > 0}
                  <span class="added-when">
                    · added {formatRelative(root.added_at_unix)}
                  </span>
                {/if}
              </span>
            </div>
            {#if !root.primary}
              <button
                class="ghost mini"
                onclick={() => removeRoot(root.idx)}
                disabled={rootActionInflight}
                aria-label={`Detach ${root.path}`}
              >
                Detach
              </button>
            {/if}
          </li>
        {/each}
      </ul>
      {#if rootActionError}
        <p class="error">{rootActionError}</p>
      {/if}
    </section>

    <!-- Indexed formats: what Sovereign DOES have. -->
    {#if formatsSorted.length > 0}
      <section class="section">
        <h3 class="section-title">Indexed formats</h3>
        <ul class="bucket-list">
          {#each formatsSorted as [ext, count] (ext)}
            <li class="bucket">
              <span class="bucket-key">.{ext}</span>
              <span class="bucket-count">{count}</span>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    <!-- §3.7 "What I don't have" surface — explicitly named so
         users know the system is honest about its negative
         space, not silently dropping things. -->
    {#if failedByKind.length > 0 || skippedSorted.length > 0}
      <section class="section negative">
        <h3 class="section-title">What I don't have</h3>
        <p class="section-lede">
          Files Sovereign noticed in this folder but couldn't index.
          The system is explicit about gaps so you can decide what to
          do — convert, OCR, or accept the omission.
        </p>

        {#each failedByKind as group (group.kind)}
          <details class="group">
            <summary>
              <span class="group-count">{group.files.length}</span>
              {reasonLabel(group.kind)}
            </summary>
            <ul class="files">
              {#each group.files as file (file.absolute_path)}
                <li class="file" title={file.reason}>
                  <span class="file-path">{file.absolute_path}</span>
                  <span class="file-reason">{file.reason}</span>
                </li>
              {/each}
            </ul>
          </details>
        {/each}

        {#if skippedSorted.length > 0}
          <details class="group">
            <summary>
              <span class="group-count">
                {skippedSorted.reduce((acc, [, n]) => acc + n, 0)}
              </span>
              Unsupported file formats
            </summary>
            <ul class="bucket-list">
              {#each skippedSorted as [ext, count] (ext)}
                <li class="bucket">
                  <span class="bucket-key">.{ext}</span>
                  <span class="bucket-count">{count}</span>
                </li>
              {/each}
            </ul>
          </details>
        {/if}
      </section>
    {/if}

    <!-- Tiered enrichment lifecycle. Off / Building / Complete /
         Failed render distinct affordances. Honest cost framing
         per §3.3 — the user reads what enrichment does, when it
         works well/poorly, what it costs, and what's recoverable
         BEFORE clicking Enable.

         As of the watched-folder tiered port: enable invokes the
         in-process tiered driver (RAPTOR + motifs + GliNER entity
         graph). The pipeline picker below is vestigial under the
         tiered path — only the legacy subprocess (daemons without
         FolderTieredProvider installed) honours it. Kept so the
         picker still works on those installs without UI changes. -->
    <section class="section enrichment">
      <h3 class="section-title">Atlas enrichment</h3>

      {#if details.enrichment.kind === "off"}
        <p class="section-lede">
          Turn this on and Sovereign reads across the folder to build
          a richer map of what's in it — section-level summaries, the
          words and phrases that recur and matter, and the people,
          places, and ideas that connect files to each other. Answers
          can then cite scene-level context, not just the nearest
          paragraph.
        </p>
        <ul class="honest-list">
          <li>
            <strong>Worth it for</strong> — notes, papers, transcripts,
            anything where files refer to each other. The more your
            files share — recurring names, repeated concepts, the same
            cast of characters — the more this lifts answers.
          </li>
          <li>
            <strong>Skip it for</strong> — small folders (a handful of
            files), grab-bags of unrelated topics, or pure data dumps
            (CSVs, exports). Nothing breaks, but the extra map adds
            little.
          </li>
          <li>
            <strong>Easy to undo</strong> — disable any time. The
            folder keeps working with plain search; the enrichment
            data drops cleanly.
          </li>
        </ul>

        {#if details.live_entries > 0}
          {@const est = costEstimate(details.live_entries)}
          <p class="cost">
            Estimated build time: <strong>{est.lowMin}–{est.highMin}
            minutes</strong> for {details.live_entries}
            {details.live_entries === 1 ? "document" : "documents"}.
            Folder stays searchable throughout the build.
          </p>
        {/if}

        <div class="actions">
          <button
            class="primary"
            onclick={enableEnrichment}
            disabled={enrichActionInflight || details.live_entries === 0}
            title={details.live_entries === 0
              ? "Folder is empty — wait for the initial sweep, then enable enrichment."
              : "Start the atlas build. The folder remains searchable while it runs."}
          >
            Enable enrichment
          </button>
        </div>
      {:else if details.enrichment.kind === "building"}
        {@const eb = details.enrichment}
        <p class="section-lede">
          Building atlas using <code>{eb.pipeline_id}</code>. The
          folder stays searchable while this runs. You can cancel
          by disabling — partial state is cleaned up.
        </p>
        <div class="progress">
          <div class="progress-meta">
            <span class="phase">{eb.phase}</span>
            {#if eb.total > 0}
              <span class="counter">{eb.current} / {eb.total}</span>
            {/if}
          </div>
          {#if eb.total > 0}
            <div
              class="progress-bar"
              role="progressbar"
              aria-valuenow={eb.current}
              aria-valuemin="0"
              aria-valuemax={eb.total}
            >
              <div
                class="progress-fill"
                style="width: {Math.min(100, (eb.current / eb.total) * 100)}%"
              ></div>
            </div>
          {:else}
            <div class="progress-bar indeterminate">
              <div class="progress-fill"></div>
            </div>
          {/if}
        </div>
        <div class="actions">
          <button
            class="ghost danger"
            onclick={disableEnrichment}
            disabled={enrichActionInflight}
            title="Cancel the build and clear partial atlas state."
          >
            Cancel & disable
          </button>
        </div>
      {:else if details.enrichment.kind === "complete"}
        {@const ec = details.enrichment}
        <p class="section-lede">
          Atlas built using <code>{ec.pipeline_id}</code>
          {ec.built_at_unix > 0
            ? `, ${formatRelative(ec.built_at_unix)}`
            : ""}.
          {#if ec.current_doc_count > ec.doc_count}
            <span class="stale">
              {ec.current_doc_count - ec.doc_count} new
              {ec.current_doc_count - ec.doc_count === 1
                ? "document has"
                : "documents have"}
              been added since — the atlas may be stale.
            </span>
          {/if}
        </p>
        <div class="actions">
          <button
            class="ghost"
            onclick={rebuildEnrichment}
            disabled={enrichActionInflight}
            title="Re-run the same pipeline against the current folder contents."
          >
            Rebuild
          </button>
          <button
            class="ghost danger"
            onclick={disableEnrichment}
            disabled={enrichActionInflight}
          >
            Disable
          </button>
        </div>
      {:else if details.enrichment.kind === "failed"}
        {@const ef = details.enrichment}
        <p class="error">
          Build failed: <code>{ef.reason}</code>
        </p>
        <div class="actions">
          <button
            class="ghost"
            onclick={rebuildEnrichment}
            disabled={enrichActionInflight}
          >
            Retry
          </button>
          <button
            class="ghost danger"
            onclick={disableEnrichment}
            disabled={enrichActionInflight}
          >
            Disable
          </button>
        </div>
      {/if}

      {#if enrichError}
        <p class="error">{enrichError}</p>
      {/if}
    </section>
  {/if}
</section>

{#if inspectingDocId && details}
  <DocumentInspector
    corpusId={details.corpus_id}
    docId={inspectingDocId}
    onClose={() => (inspectingDocId = null)}
  />
{/if}

<style>
  .detail {
    padding: 28px 32px 44px;
    max-width: 920px;
    animation: lk-fade-in 200ms ease-out both;
  }
  .head {
    margin-bottom: 24px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .back {
    align-self: flex-start;
    margin-bottom: 8px;
    padding: 4px 8px;
    background: none;
    border: none;
    color: var(--lk-ink-soft);
    font-size: var(--lk-size-meta);
    cursor: pointer;
  }
  .back:hover { color: var(--lk-ink); }
  .title {
    margin: 0;
    font-family: var(--lk-font-display);
    font-size: var(--lk-size-hero);
    font-weight: 600;
    color: var(--lk-ink);
  }
  .path {
    margin: 0;
    font-family: var(--lk-font-mono, monospace);
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
    overflow-wrap: anywhere;
  }
  .muted { color: var(--lk-ink-faded); font-size: var(--lk-size-meta); }
  .error {
    padding: 8px 12px;
    border-left: 3px solid var(--lk-err);
    background: var(--lk-err-wash);
    color: var(--lk-ink);
    font-size: var(--lk-size-meta);
  }

  .summary {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    gap: 16px;
    margin-bottom: 28px;
    padding: 18px 20px;
    background: var(--lk-paper-deep);
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
  }
  .metric {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .metric-label {
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .metric-value {
    font-size: var(--lk-size-lead);
    font-weight: 500;
    color: var(--lk-ink);
  }
  .metric-value.subtle { font-weight: 400; color: var(--lk-ink-soft); }
  .metric-suffix { font-size: var(--lk-size-meta); color: var(--lk-ink-faded); }
  .sensitive-tag {
    grid-column: 1 / -1;
    padding-left: 12px;
    border-left: 2px solid var(--lk-ink-faded);
    font-style: italic;
  }

  .section { margin-top: 24px; }
  .section-title {
    margin: 0 0 8px;
    font-size: var(--lk-size-lead);
    color: var(--lk-ink);
    font-weight: 500;
  }
  .section-lede {
    margin: 0 0 12px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    line-height: 1.4;
    max-width: 60ch;
  }

  .bucket-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .bucket {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    padding: 4px 10px;
    background: var(--lk-paper-deep);
    border: 1px solid var(--lk-rule);
    border-radius: 999px;
    font-size: var(--lk-size-meta);
  }
  .bucket-key {
    font-family: var(--lk-font-mono, monospace);
    color: var(--lk-ink-soft);
  }
  .bucket-count { color: var(--lk-ink); font-weight: 500; }

  .negative .section-title { color: var(--lk-ink); }
  .group {
    margin-top: 8px;
    padding: 10px 14px;
    background: var(--lk-paper-deep);
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
  }
  .group summary {
    cursor: pointer;
    color: var(--lk-ink-soft);
    font-size: var(--lk-size-meta);
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .group summary:hover { color: var(--lk-ink); }
  .group-count {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 1.5em;
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--lk-rule);
    font-size: var(--lk-size-meta);
    color: var(--lk-ink);
    font-weight: 500;
  }
  .files {
    list-style: none;
    margin: 10px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .file {
    display: flex;
    flex-direction: column;
    padding: 6px 8px;
    border-radius: 4px;
    font-size: var(--lk-size-meta);
  }
  .file:hover { background: var(--lk-rule); }
  .file-path {
    font-family: var(--lk-font-mono, monospace);
    color: var(--lk-ink);
    overflow-wrap: anywhere;
  }
  .file-reason { color: var(--lk-ink-faded); margin-top: 2px; }

  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 8px;
  }
  .root-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .root {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 12px;
    background: var(--lk-paper-deep);
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
  }
  .root-meta {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .root-path {
    font-family: var(--lk-font-mono, monospace);
    font-size: var(--lk-size-meta);
    color: var(--lk-ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .root-detail {
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
  }
  .primary-tag {
    margin-left: 6px;
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--lk-paper);
    border: 1px solid var(--lk-rule);
    font-size: 11px;
    color: var(--lk-ink-soft);
  }
  .added-when { margin-left: 4px; }
  .ghost {
    padding: 6px 12px;
    background: transparent;
    border: 1px solid var(--lk-rule);
    border-radius: 6px;
    color: var(--lk-ink);
    cursor: pointer;
    font-size: var(--lk-size-meta);
  }
  .ghost:hover { border-color: var(--lk-crown); }
  .ghost:disabled { opacity: 0.5; cursor: not-allowed; }
  .ghost.mini { padding: 2px 8px; font-size: 11px; }
  .ghost.danger { color: var(--lk-err); }
  .ghost.danger:hover:not(:disabled) { border-color: var(--lk-err); }
  .primary {
    padding: 6px 14px;
    background: var(--lk-warn);
    border: 1px solid var(--lk-warn);
    border-radius: 6px;
    color: white;
    font-weight: 500;
    cursor: pointer;
    font-size: var(--lk-size-meta);
  }
  .primary:disabled { opacity: 0.5; cursor: not-allowed; }
  .actions { margin-top: 12px; display: flex; gap: 8px; flex-wrap: wrap; }

  .enrichment .honest-list {
    list-style: none;
    margin: 8px 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
  }
  .enrichment .honest-list li {
    padding: 6px 10px;
    border-left: 2px solid var(--lk-rule);
    line-height: 1.4;
  }
  .cost {
    margin: 8px 0 0;
    padding: 8px 12px;
    background: var(--lk-paper-deep);
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
  }
  .stale {
    color: var(--lk-warn);
    font-style: italic;
  }
  .progress {
    margin: 12px 0;
    padding: 12px 14px;
    background: var(--lk-paper-deep);
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
  }
  .progress-meta {
    display: flex;
    justify-content: space-between;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    margin-bottom: 6px;
  }
  .phase { font-family: var(--lk-font-mono, monospace); }
  .progress-bar {
    height: 6px;
    background: var(--lk-rule);
    border-radius: 3px;
    overflow: hidden;
  }
  .progress-fill {
    height: 100%;
    background: var(--lk-crown, var(--lk-warn));
    transition: width 0.3s ease;
  }
  .progress-bar.indeterminate .progress-fill {
    width: 30%;
    animation: indeterminate 1.4s linear infinite;
  }
  @keyframes indeterminate {
    0% { transform: translateX(-100%); }
    100% { transform: translateX(330%); }
  }
</style>
