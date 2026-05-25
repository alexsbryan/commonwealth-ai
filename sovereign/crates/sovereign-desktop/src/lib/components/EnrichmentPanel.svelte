<!--
  Atlas enrichment panel.

  - Lists enriched corpora on disk with pipeline + age.
  - Shows a live per-job progress row while a build is streaming.
  - Exposes "Build" to kick off `enrich_build_async` and "Errors"
    to fetch the structured failure aggregate.
  - Offers a minimal SEP-ingest form so an operator can scaffold a
    new per-article philosophy corpus without touching the CLI.

  Mounted from `SettingsPanel.svelte` as the "Enrichment" tab.
  Shares state with other mounts via the `enrichProgressStore`
  singleton — two instances of this panel on-screen would stay in
  sync automatically.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import {
    enrichBuildAsync,
    enrichCancelBuild,
    enrichErrors,
    enrichListCorpora,
    enrichSepIngest,
  } from "../api";
  import type {
    EnrichedCorpusSummary,
    PhaseFailure,
  } from "../types";
  import { enrichProgressStore } from "../stores/enrichProgress.svelte";
  import EnrichmentStage from "./EnrichmentStage.svelte";

  // ── Corpora list ────────────────────────────────────────

  let corpora: EnrichedCorpusSummary[] = $state([]);
  let corporaLoading = $state(true);
  let corporaError = $state<string | null>(null);

  async function refreshCorpora() {
    corporaLoading = true;
    corporaError = null;
    try {
      corpora = await enrichListCorpora();
    } catch (e) {
      corporaError = String(e);
    } finally {
      corporaLoading = false;
    }
  }

  // ── Build kickoff ───────────────────────────────────────

  let buildingCorpus = $state<string | null>(null);
  let cancellingJobs = $state<Record<string, boolean>>({});

  async function startBuild(corpusId: string) {
    buildingCorpus = corpusId;
    try {
      const handle = await enrichBuildAsync(corpusId, null, null);
      await enrichProgressStore.track(handle);
    } catch (e) {
      // Surface as the per-corpus lastError so the row highlights
      // it. Two known failure modes: the spawn failed (CLI not in
      // PATH) or the concurrency guard rejected ("already running").
      rowErrors = { ...rowErrors, [corpusId]: String(e) };
    } finally {
      buildingCorpus = null;
    }
  }

  // Cancel is owned by `EnrichmentStage` when it's rendering the
  // active job. This helper remains only for the (rare) case where
  // the panel needs to cancel from outside the stage — currently
  // unused, kept as a seam for future toolbar actions. If it stays
  // unused after O3 lands, delete it.
  async function cancelBuild(jobId: string) {
    cancellingJobs = { ...cancellingJobs, [jobId]: true };
    try {
      await enrichCancelBuild(jobId);
    } catch (e) {
      const job = enrichProgressStore.get(jobId);
      if (job) {
        rowErrors = { ...rowErrors, [job.corpus_id]: `Cancel failed: ${e}` };
      }
    } finally {
      cancellingJobs = { ...cancellingJobs, [jobId]: false };
    }
  }

  // ── Errors fetching ─────────────────────────────────────

  let errorsByCorpus = $state<Record<string, PhaseFailure[]>>({});
  let errorsOpen = $state<Record<string, boolean>>({});
  let errorsLoading = $state<Record<string, boolean>>({});
  let rowErrors = $state<Record<string, string>>({});

  async function toggleErrors(corpusId: string) {
    if (errorsOpen[corpusId]) {
      errorsOpen = { ...errorsOpen, [corpusId]: false };
      return;
    }
    errorsLoading = { ...errorsLoading, [corpusId]: true };
    try {
      errorsByCorpus = {
        ...errorsByCorpus,
        [corpusId]: await enrichErrors(corpusId),
      };
      errorsOpen = { ...errorsOpen, [corpusId]: true };
    } catch (e) {
      rowErrors = { ...rowErrors, [corpusId]: String(e) };
    } finally {
      errorsLoading = { ...errorsLoading, [corpusId]: false };
    }
  }

  /// Group failures for rendering. `(phase, kind)` → count. Stable
  /// sort (count desc, then phase, then kind) so the UI matches
  /// the CLI aggregator's output order exactly.
  function groupFailures(
    failures: PhaseFailure[],
  ): { phase: string; kind: string; count: number; sample: PhaseFailure }[] {
    const buckets = new Map<
      string,
      { phase: string; kind: string; count: number; sample: PhaseFailure }
    >();
    for (const f of failures) {
      const key = `${f.phase}::${f.kind}`;
      const existing = buckets.get(key);
      if (existing) {
        existing.count += 1;
      } else {
        buckets.set(key, { phase: f.phase, kind: f.kind, count: 1, sample: f });
      }
    }
    return [...buckets.values()].sort((a, b) => {
      if (a.count !== b.count) return b.count - a.count;
      if (a.phase !== b.phase) return a.phase.localeCompare(b.phase);
      return a.kind.localeCompare(b.kind);
    });
  }

  // ── SEP ingest form ─────────────────────────────────────

  let sepSlug = $state("");
  let sepParagraphsPerSection = $state<number | null>(null);
  let sepIngesting = $state(false);
  let sepResult = $state<string | null>(null);
  let sepError = $state<string | null>(null);

  async function submitSepIngest() {
    const slug = sepSlug.trim();
    if (!slug) {
      sepError = "Enter a category slug (e.g. `compatibilism`).";
      return;
    }
    sepIngesting = true;
    sepError = null;
    sepResult = null;
    try {
      const result = await enrichSepIngest(slug, sepParagraphsPerSection);
      sepResult = `Scaffolded \`${result.corpus_id}\`. Switch to "Build" below to run enrichment.`;
      sepSlug = "";
      await refreshCorpora();
    } catch (e) {
      sepError = String(e);
    } finally {
      sepIngesting = false;
    }
  }

  // ── Derived: per-corpus progress rows ───────────────────

  let activeByCorpus = $derived.by(() => {
    const map: Record<string, ReturnType<typeof enrichProgressStore.byCorpus>[number]> = {};
    for (const c of corpora) {
      const jobs = enrichProgressStore.byCorpus(c.corpus_id);
      if (jobs.length > 0) map[c.corpus_id] = jobs[0]; // newest-first already
    }
    return map;
  });

  function formatAge(iso: string): string {
    if (!iso) return "";
    const then = Date.parse(iso);
    if (Number.isNaN(then)) return iso;
    const delta = (Date.now() - then) / 1000;
    if (delta < 60) return `${Math.floor(delta)}s ago`;
    if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
    if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
    return `${Math.floor(delta / 86400)}d ago`;
  }

  onMount(refreshCorpora);
</script>

<section class="enrich-panel">
  <p class="intro">
    Builds a graph of people, events, claims, and open questions across
    one library at a time. Every LLM call stays local — through the
    daemon at <code>localhost:9741</code>.
  </p>

  <!-- ── SEP ingest ──────────────────────────────────── -->

  <p class="section-label">New SEP article</p>
  <p class="slot-desc" style="margin-bottom: 10px;">
    Build a per-article library from the Stanford Encyclopedia of
    Philosophy parquet. Cache the parquet first with
    <code>sovereign corpus acquire sep</code>.
  </p>

  <form class="sep-form" onsubmit={(e) => { e.preventDefault(); submitSepIngest(); }}>
    <input
      class="sep-slug"
      type="text"
      placeholder="category slug (e.g. compatibilism)"
      bind:value={sepSlug}
      disabled={sepIngesting}
    />
    <input
      class="sep-pps"
      type="number"
      placeholder="paragraphs/section"
      min="1"
      bind:value={sepParagraphsPerSection}
      disabled={sepIngesting}
    />
    <button class="primary" type="submit" disabled={sepIngesting || !sepSlug.trim()}>
      {sepIngesting ? "Scaffolding…" : "Ingest"}
    </button>
  </form>
  {#if sepResult}
    <p class="ok-msg">{sepResult}</p>
  {/if}
  {#if sepError}
    <p class="err-msg">{sepError}</p>
  {/if}

  <!-- ── Corpora list ────────────────────────────────── -->

  <p class="section-label" style="margin-top: 28px;">Enriched libraries</p>

  {#if corporaLoading}
    <p class="muted">Loading…</p>
  {:else if corporaError}
    <p class="err-msg">Could not list libraries: {corporaError}</p>
  {:else if corpora.length === 0}
    <p class="muted">
      Nothing enriched yet. Scaffold an SEP article above or use the CLI:
      <code>sovereign enrich init &lt;corpus&gt; --source &lt;file&gt; --pipeline philosophy_atlas</code>.
    </p>
  {:else}
    <div class="corpora">
      {#each corpora as corpus (corpus.corpus_id)}
        {@const job = activeByCorpus[corpus.corpus_id]}
        <article class="corpus-row" class:is-building={job && !job.terminal}>
          <header class="corpus-head">
            <div class="corpus-idblock">
              <h3 class="corpus-id">{corpus.corpus_id}</h3>
              <span class="corpus-meta">
                {corpus.pipeline_id} · {formatAge(corpus.created_at)}
              </span>
            </div>
            <div class="corpus-actions">
              {#if job && !job.terminal}
                <button
                  class="secondary danger"
                  onclick={() => cancelBuild(job.job_id)}
                  disabled={cancellingJobs[job.job_id]}
                >
                  {cancellingJobs[job.job_id] ? "Cancelling…" : "Cancel"}
                </button>
              {:else}
                <button
                  class="secondary"
                  onclick={() => startBuild(corpus.corpus_id)}
                  disabled={buildingCorpus === corpus.corpus_id}
                >
                  Build
                </button>
              {/if}
              <button
                class="secondary"
                onclick={() => toggleErrors(corpus.corpus_id)}
                disabled={errorsLoading[corpus.corpus_id]}
              >
                {errorsLoading[corpus.corpus_id] ? "…" : errorsOpen[corpus.corpus_id] ? "Hide errors" : "Errors"}
              </button>
            </div>
          </header>

          {#if rowErrors[corpus.corpus_id]}
            <p class="err-msg">{rowErrors[corpus.corpus_id]}</p>
          {/if}

          <!-- Live progress inline on the row. Cancel is hidden here
               because the row header already renders a Cancel button
               while the job is non-terminal. -->
          <EnrichmentStage job={job ?? null} hideCancel={true} />

          <!-- Structured failures panel. -->
          {#if errorsOpen[corpus.corpus_id]}
            {@const failures = errorsByCorpus[corpus.corpus_id] ?? []}
            <div class="errors-block">
              {#if failures.length === 0}
                <p class="muted">No structured failures across any phase. ✓</p>
              {:else}
                <p class="errors-summary">
                  {failures.length} failure(s) across {groupFailures(failures).length} group(s)
                </p>
                {#each groupFailures(failures) as g}
                  <div class="err-group">
                    <header class="err-group-head">
                      <code class="err-phase">[{g.phase} / {g.kind}]</code>
                      <span class="err-count">{g.count}</span>
                    </header>
                    <p class="err-sample">
                      <code>{g.sample.subject}</code> — {g.sample.reason}
                    </p>
                    {#if g.sample.remediation}
                      <p class="err-remediation">
                        <span class="err-remediation-label">Remediate:</span>
                        {g.sample.remediation}
                      </p>
                    {/if}
                  </div>
                {/each}
              {/if}
            </div>
          {/if}
        </article>
      {/each}
    </div>
  {/if}
</section>

<style>
  .enrich-panel {
    padding: 4px 0;
  }
  .intro {
    color: var(--text-secondary, var(--text-primary));
    margin: 0 0 20px 0;
    line-height: 1.5;
  }
  .section-label {
    /* Matches the SettingsPanel tab section-label rule. */
    margin: 0 0 6px 0;
    font-size: 0.85em;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-secondary, var(--text-primary));
  }
  .slot-desc {
    margin: 0;
    color: var(--text-secondary, var(--text-primary));
    font-size: 0.92em;
  }
  .muted {
    color: var(--text-muted, var(--text-secondary));
    font-size: 0.92em;
  }

  /* ── SEP form ─────────────────────────── */
  .sep-form {
    display: flex;
    gap: 8px;
    align-items: center;
    margin-bottom: 6px;
  }
  .sep-slug {
    flex: 1 1 auto;
    min-width: 180px;
  }
  .sep-pps {
    flex: 0 0 150px;
    text-align: right;
  }
  .sep-form input {
    background: var(--bg-secondary, var(--bg-primary));
    border: 1px solid var(--border, #333);
    border-radius: var(--radius, 6px);
    padding: 6px 10px;
    color: var(--text-primary, #eee);
    font-size: 0.9em;
  }
  .sep-form input:disabled {
    opacity: 0.5;
  }

  .primary,
  .secondary {
    font-size: 0.88em;
    padding: 6px 12px;
    border-radius: var(--radius, 6px);
    border: 1px solid var(--border, #333);
    cursor: pointer;
    font-family: inherit;
  }
  .primary {
    background: var(--accent, #c4a46a);
    color: var(--text-on-accent, #111);
    border-color: var(--accent, #c4a46a);
  }
  .secondary {
    background: var(--bg-secondary, transparent);
    color: var(--text-primary, #eee);
  }
  .primary:disabled,
  .secondary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  /* Danger variant for destructive actions (Cancel). Subtle —
     hints at intent without stealing attention from the progress
     bar while a build is streaming. */
  .secondary.danger {
    color: var(--error, #d27979);
    border-color: var(--error, #d27979);
  }
  .secondary.danger:hover:not(:disabled) {
    background: var(--error, #d27979);
    color: var(--text-on-accent, #111);
  }

  .ok-msg {
    margin: 6px 0 0;
    color: var(--success, #7fb86a);
    font-size: 0.9em;
  }
  .err-msg {
    margin: 6px 0 0;
    color: var(--error, #d27979);
    font-size: 0.9em;
  }

  /* ── Corpus rows ──────────────────────── */
  .corpora {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .corpus-row {
    border: 1px solid var(--border, #333);
    border-radius: var(--radius, 6px);
    padding: 12px 14px;
    background: var(--bg-secondary, transparent);
  }
  .corpus-row.is-building {
    border-color: var(--accent, #c4a46a);
  }
  .corpus-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
  }
  .corpus-id {
    margin: 0;
    font-size: 1em;
    font-weight: 600;
    color: var(--text-primary, #eee);
  }
  .corpus-meta {
    display: block;
    margin-top: 2px;
    color: var(--text-muted, var(--text-secondary));
    font-size: 0.82em;
  }
  .corpus-actions {
    display: flex;
    gap: 6px;
  }

  /* Progress styles moved to EnrichmentStage.svelte (O2). */

  /* ── Errors panel ─────────────────────── */
  .errors-block {
    margin-top: 12px;
    padding-top: 10px;
    border-top: 1px solid var(--border, #333);
  }
  .errors-summary {
    margin: 0 0 8px;
    font-size: 0.9em;
    color: var(--text-primary, #eee);
  }
  .err-group {
    padding: 8px 0;
    border-bottom: 1px solid var(--border, #333);
  }
  .err-group:last-child {
    border-bottom: 0;
  }
  .err-group-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 10px;
  }
  .err-phase {
    font-size: 0.85em;
    color: var(--text-secondary, var(--text-primary));
  }
  .err-count {
    font-variant-numeric: tabular-nums;
    color: var(--error, #d27979);
    font-weight: 600;
  }
  .err-sample {
    margin: 4px 0 0;
    font-size: 0.82em;
    color: var(--text-muted, var(--text-secondary));
  }
  .err-remediation {
    margin: 6px 0 0;
    padding: 6px 8px;
    background: var(--bg-primary, rgba(0, 0, 0, 0.2));
    border-left: 2px solid var(--accent, #c4a46a);
    border-radius: 2px;
    font-size: 0.82em;
    color: var(--text-primary, #eee);
    line-height: 1.45;
  }
  .err-remediation-label {
    color: var(--accent, #c4a46a);
    font-weight: 600;
    margin-right: 4px;
  }
</style>
