<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Deep research — scene 1 (order deep-research-t3b). The desktop is a
  DRIVER over the CLI verb's contract (`svrn deep-research`, order
  deep-research-t3a): this view forwards the operator's question + budget +
  typed consent grant to the verb as flags, then renders ONLY what the
  driver read back from the run-dir artifacts the verb wrote. No loop logic
  here, no second state source — the run-dir/manifest is the single source,
  and the checked report is the verb's own artifact, never re-invented.

  Three faces:
    • Ask  — the composer: question, budget, typed consent (default-deny),
             estate corpora for the next ask (a prior run's estate corpus
             selects here — the compounding handoff).
    • Run  — the live view: round N, the gate's named gap list, the budget
             ledger, the consent-grant status.
    • Report — the checked report: report.md + the verdict dimensions
             (corroboration / residue / reframe / alignment) + the
             constitution position (zero untraced figures in [passed]).
-->
<script lang="ts">
  import { onMount } from "svelte";
  import {
    drCapabilities,
    drStart,
    drAbort,
    drListRuns,
    drOpenReport,
    listCorpora,
  } from "../../api";
  import type {
    CorpusEntry,
    DrBudget,
    DrCapabilities,
    DrReport,
    DrRunSummary,
  } from "../../types";
  import {
    deepResearchStore,
    formatElapsed,
    runStateLabel,
  } from "../../stores/deepResearch.svelte";
  import { renderMarkdown } from "../../utils/markdown";

  let {
    onExit,
    onOpenLibrary,
  }: { onExit: () => void; onOpenLibrary: () => void } = $props();

  // ── State tiers ─────────────────────────────────────────────
  // THE RUN IS NOT HELD HERE. It lives in `stores/deepResearch.svelte.ts`,
  // at module scope, because this component is mounted under an `{#if}` and
  // vanishes the moment the operator clicks anything else — which used to
  // take the listener and every fact about the run with it. What is local
  // here is what is genuinely local: which face is showing, and the
  // composer's unsent input.

  type Face = "ask" | "run" | "report";
  let face = $state<Face>("ask");

  // Composer
  let question = $state("");
  let maxRounds = $state(3);
  let searchBudget = $state(4);
  let fetchBudget = $state(4);
  /// "" = default-deny (no `--consent` flag — the web leg refuses
  /// non-public-web payloads). The typed set is the verb's closed set.
  let consent = $state<"" | "public-web" | "peer" | "personal">("");
  let selectedCorpora = $state<string[]>([]);
  let corpora = $state<CorpusEntry[]>([]);
  let caps = $state<DrCapabilities | null>(null);
  let capsError = $state<string | null>(null);
  let startError = $state<string | null>(null);
  let stopError = $state<string | null>(null);
  let stopConfirming = $state(false);

  // The run in flight, read from the store.
  let active = $derived(deepResearchStore.active);
  let running = $derived(active !== null);
  let liveness = $derived(deepResearchStore.liveness);
  let signalAge = $derived(deepResearchStore.signalAgeSecs);
  let finished = $derived(deepResearchStore.finished);

  // Report view
  let report = $state<DrReport | null>(null);
  let reportError = $state<string | null>(null);

  // Shelf
  let runs = $state<DrRunSummary[]>([]);
  let runsError = $state<string | null>(null);

  const hasResume = () => caps?.flags.includes("--resume") ?? false;

  onMount(() => {
    void loadCapabilities();
    void loadCorpora();
    void loadRuns();
    // A run may already be turning — started before this view existed, or
    // still going from the last time it was closed. Find it rather than
    // laying an empty composer over the top of it.
    void adoptRunInFlight();
  });

  async function adoptRunInFlight() {
    await deepResearchStore.recover();
    if (deepResearchStore.active) face = "run";
  }

  /// A run that lands while this view is open shows its report at once; one
  /// that lands while the view is closed is held by the store and shown on
  /// the next open (the banner announces it meanwhile). Either way the
  /// finished work is surfaced — it used to be dropped on the floor,
  /// because the terminal event arrived at a listener this component had
  /// already torn down.
  $effect(() => {
    const f = deepResearchStore.finished;
    if (!f || f.seen) return;
    deepResearchStore.markFinishedSeen();
    stopConfirming = false;
    if (f.report) {
      report = f.report;
      face = "report";
    } else {
      face = "run";
    }
    void loadRuns();
  });

  async function loadCapabilities() {
    try {
      caps = await drCapabilities();
      capsError = caps.error;
    } catch (e) {
      capsError = typeof e === "string" ? e : String(e);
    }
  }

  async function loadCorpora() {
    try {
      corpora = (await listCorpora()).filter((c) => c.status === "installed");
    } catch {
      corpora = [];
    }
  }

  async function loadRuns() {
    try {
      runs = await drListRuns();
      runsError = null;
    } catch (e) {
      runsError = typeof e === "string" ? e : String(e);
    }
  }

  // ── Ask → run ───────────────────────────────────────────────

  async function startRun(resumeRunId?: string) {
    startError = null;
    stopError = null;
    report = null;
    deepResearchStore.clearFinished();
    const asked = resumeRunId
      ? (runs.find((r) => r.run_id === resumeRunId)?.question ?? resumeRunId)
      : question.trim();
    try {
      const handle = await drStart(question, {
        maxRounds,
        search: searchBudget,
        fetch: fetchBudget,
        corpora: selectedCorpora,
        consent: consent === "" ? null : consent,
        resumeRunId: resumeRunId ?? null,
      });
      // Hand the run to the store immediately: from here it belongs to the
      // app, not to this component's lifetime.
      await deepResearchStore.attach(handle, asked);
      face = "run";
      void loadRuns();
    } catch (e) {
      startError = typeof e === "string" ? e : String(e);
    }
  }

  // ── Stopping — the operator's call, and only theirs ──────────

  /// Stopping is confirmed, because a run may be twenty minutes deep and
  /// the click is next to nothing else. It is also not a kill: the backend
  /// polls the flag at every state entry and lands a truncated report with
  /// the truncation declared, so the honest promise is "keep what we have",
  /// not "abort".
  function requestStop() {
    stopConfirming = true;
  }

  async function confirmStop() {
    stopConfirming = false;
    const a = deepResearchStore.active;
    if (!a) return;
    deepResearchStore.markStopRequested();
    try {
      await drAbort(a.jobId);
      void loadRuns();
    } catch (e) {
      stopError = typeof e === "string" ? e : String(e);
    }
  }

  // ── Report ──────────────────────────────────────────────────

  async function openReport(runIdToOpen: string) {
    reportError = null;
    try {
      report = await drOpenReport(runIdToOpen);
      face = "report";
    } catch (e) {
      reportError = typeof e === "string" ? e : String(e);
    }
  }

  /// The completed run's estate corpus (t3a's ingest names it after the
  /// run) selects into the next ask's `--corpora` — the compounding
  /// handoff. When it is not on the shelf yet, absence is reported, never
  /// defaulted.
  function askAgainOnEstate() {
    const matching = corpora.filter(
      (c) => report && (c.id.includes(report.run_id) || report.run_id.includes(c.id)),
    );
    selectedCorpora = matching.map((c) => c.id);
    face = "ask";
  }

  // ── Rendering helpers ───────────────────────────────────────

  /// The stage vocabulary is the backend's closed set (`planning` /
  /// `rounding` / `checking` / `done`, derived from which artifacts exist).
  /// This maps it to something a person can act on. An unknown stage is
  /// shown VERBATIM rather than flattened to a generic "working" — a label
  /// we cannot explain is still information, and hiding it would be the
  /// silent substitution this whole surface is trying to stop making.
  const STAGE_LABEL: Record<string, string> = {
    planning: "Planning the search",
    rounding: "Gathering and checking evidence",
    checking: "Checking the draft against its evidence",
    done: "Finishing up",
  };

  function stageLabel(stage: string): string {
    if (!stage) return "Starting up";
    return STAGE_LABEL[stage] ?? stage;
  }

  function meterRows(
    budget: DrBudget,
  ): { key: string; spent: number; remaining: number; total: number; pct: number }[] {
    const keys = new Set([...Object.keys(budget.spent), ...Object.keys(budget.remaining)]);
    return [...keys].sort().map((key) => {
      const spent = budget.spent[key] ?? 0;
      const remaining = budget.remaining[key] ?? 0;
      const total = spent + remaining;
      return { key, spent, remaining, total, pct: total > 0 ? (spent / total) * 100 : 0 };
    });
  }
</script>

<div class="dr-view" data-testid="deep-research-view">
  <header class="dr-header">
    <button type="button" class="dr-back" onclick={onExit} data-testid="dr-back">
      ← Back to chat
    </button>
    <h1>Deep research</h1>
    <p class="dr-sub">
      A question, a budget, and a typed release. The run drives the
      deep-research verb — rounds of gap-driven acquisition, a checked
      report, and an estate corpus you keep.
    </p>
    {#if capsError}
      <p class="dr-caps-error" data-testid="dr-caps-error">{capsError}</p>
    {/if}
  </header>

  {#if face === "ask"}
    <section class="dr-composer" data-testid="dr-composer">
      {#if active}
        <!-- Reached the composer with a run in flight (via "Leave it
             running", or the shelf). Say so, and offer the way back —
             an empty composer over a live run is what made people think
             their research had been thrown away. -->
        <div class="dr-running-notice" data-testid="dr-running-notice">
          <span>
            A run is still going{active.round !== null
              ? ` — round ${active.round}${active.maxRounds ? ` of ${active.maxRounds}` : ""}`
              : ""}{active.lastBeatMs !== null
              ? `, ${formatElapsed(active.elapsedSecs)} in`
              : ""}. One runs at a time, so this asks again once it
            finishes — or stop it from the run view.
          </span>
          <button
            type="button"
            class="dr-linkish"
            onclick={() => (face = "run")}
            data-testid="dr-back-to-run"
          >
            Back to the run
          </button>
        </div>
      {/if}
      <label class="dr-field dr-question">
        <span class="dr-label">The question</span>
        <textarea
          bind:value={question}
          rows="3"
          placeholder="What do you need to know, and what does an answer look like?"
          data-testid="dr-question"
        ></textarea>
      </label>

      <div class="dr-row">
        <label class="dr-field dr-number">
          <span class="dr-label">Rounds</span>
          <input type="number" min="1" max="6" bind:value={maxRounds} data-testid="dr-rounds" />
        </label>
        <label class="dr-field dr-number">
          <span class="dr-label">Searches / round</span>
          <input
            type="number"
            min="0"
            max="12"
            bind:value={searchBudget}
            data-testid="dr-search"
          />
        </label>
        <label class="dr-field dr-number">
          <span class="dr-label">Fetches / round</span>
          <input
            type="number"
            min="0"
            max="12"
            bind:value={fetchBudget}
            data-testid="dr-fetch"
          />
        </label>
      </div>

      <fieldset class="dr-consent" data-testid="dr-consent">
        <legend class="dr-label">
          Consent — what this run may release outside your estate
        </legend>
        <label class="dr-consent-option">
          <input
            type="radio"
            name="dr-consent"
            value=""
            bind:group={consent}
            checked={consent === ""}
            data-testid="dr-consent-deny"
          />
          <span>
            <strong>Default-deny</strong>
            <em>No release. The run uses your estate only; anything the web
            leg would need is refused.</em>
          </span>
        </label>
        <label class="dr-consent-option">
          <input
            type="radio"
            name="dr-consent"
            value="public-web"
            bind:group={consent}
            data-testid="dr-consent-public-web"
          />
          <span>
            <strong>Public web</strong>
            <em>Release open-web pages into this run's evidence.</em>
          </span>
        </label>
        <label class="dr-consent-option">
          <input
            type="radio"
            name="dr-consent"
            value="peer"
            bind:group={consent}
            data-testid="dr-consent-peer"
          />
          <span>
            <strong>Peer</strong>
            <em>Also release material shared by other nodes on your mesh.</em>
          </span>
        </label>
        <label class="dr-consent-option">
          <input
            type="radio"
            name="dr-consent"
            value="personal"
            bind:group={consent}
            data-testid="dr-consent-personal"
          />
          <span>
            <strong>Personal</strong>
            <em>Also release your own estate material (local files, notes,
            conversations).</em>
          </span>
        </label>
      </fieldset>

      <div class="dr-corpora">
        <span class="dr-label">Consult these corpora first (your estate)</span>
        {#if corpora.length === 0}
          <p class="dr-muted">
            No installed corpora yet. A completed run's estate corpus appears
            here for the next ask.
          </p>
        {:else}
          <div class="dr-corpora-list">
            {#each corpora as c (c.id)}
              <label class="dr-corpus-chip">
                <input
                  type="checkbox"
                  value={c.id}
                  bind:group={selectedCorpora}
                  data-testid={`dr-corpus-${c.id}`}
                />
                {c.name}
              </label>
            {/each}
          </div>
        {/if}
      </div>

      {#if startError}
        <p class="dr-error" data-testid="dr-start-error">{startError}</p>
      {/if}

      <!-- Disabled while something is in flight. The backend refuses a
           second concurrent run too (that is where the invariant lives);
           this stops the operator walking into the refusal. -->
      <button
        type="button"
        class="dr-primary"
        onclick={() => void startRun()}
        disabled={running || (question.trim() === "" && !hasResume())}
        data-testid="dr-start"
      >
        {running ? "A run is already going" : "Start the run"}
      </button>
    </section>

  {:else if face === "run"}
    <section class="dr-run" data-testid="dr-run-view">
      {#if active}
        <!-- The pulse. Three facts a waiting person actually wants: what it
             is doing, how long it has been doing it, and whether the run is
             still talking to us at all. -->
        <div class="dr-pulse" data-testid="dr-pulse" data-liveness={liveness}>
          <span
            class="dr-beat"
            class:stalled={liveness === "no-signal"}
            aria-hidden="true"
          ></span>
          <span class="dr-stage" data-testid="dr-stage" data-stage={active.stage}>
            {stageLabel(active.stage)}
          </span>
          <span class="dr-elapsed" data-testid="dr-elapsed">
            {active.lastBeatMs === null ? "—" : formatElapsed(active.elapsedSecs)}
          </span>
          {#if active.round !== null}
            <span class="dr-round" data-testid="dr-round">
              round {active.round}{active.maxRounds ? ` of ${active.maxRounds}` : ""}
            </span>
          {/if}
          {#if active.runId}
            <span class="dr-run-id" data-testid="dr-run-id">{active.runId}</span>
          {/if}
        </div>

        <!-- The liveness verdict, named rather than inferred. A run that has
             gone silent is a DIFFERENT state from a run that is thinking,
             and both look identical on a panel that only redraws when
             something changes. -->
        <p class="dr-liveness" data-testid="dr-liveness" data-state={liveness}>
          {#if liveness === "starting"}
            Waiting for the run's first signal…
          {:else if liveness === "no-signal"}
            No signal from the run for {signalAge}s. It may be stuck. Nothing
            gathered so far is lost, and stopping is safe.
          {:else if liveness === "quiet"}
            Working. Nothing has changed on disk for {formatElapsed(active.quietSecs)} —
            normal while a round is reading or waiting on a model.
          {:else}
            Working — last change {formatElapsed(active.quietSecs)} ago.
          {/if}
        </p>

        <!-- The promise the operator asked for, stated where they are
             deciding whether to wait. -->
        <p class="dr-detach-note" data-testid="dr-detach-note">
          This run keeps going if you leave this screen or use the rest of the
          app. It stops when you stop it, or when it finishes on its own.
        </p>
      {/if}

      {#if finished?.error}
        <p class="dr-error" data-testid="dr-run-failed">{finished.error}</p>
      {/if}
      {#if stopError}
        <p class="dr-error" data-testid="dr-stop-error">{stopError}</p>
      {/if}

      <div class="dr-panel">
        <h2>What it has done</h2>
        {#if !active || active.trail.length === 0}
          <p class="dr-muted" data-testid="dr-trail-empty">
            No round has reported yet.
          </p>
        {:else}
          <ol class="dr-trail" data-testid="dr-trail">
            {#each active.trail as t (t.round)}
              <li class="dr-trail-row" data-testid={`dr-trail-${t.round}`}>
                <span class="dr-trail-head">
                  <span class="dr-trail-round">round {t.round}</span>
                  <span class="dr-muted">at {formatElapsed(t.atSecs)}</span>
                </span>
                <ul class="dr-trail-gaps">
                  {#each t.gaps as g (g.id)}
                    <li>{g.text}</li>
                  {/each}
                </ul>
              </li>
            {/each}
          </ol>
        {/if}
      </div>

      <div class="dr-panel">
        <h2>The gate's named gaps</h2>
        {#if !active || active.gaps.length === 0}
          <p class="dr-muted" data-testid="dr-gaps-empty">
            No gaps named yet — the compass speaks after a round completes.
          </p>
        {:else}
          <ul class="dr-gaps" data-testid="dr-gaps">
            {#each active.gaps as g (g.id)}
              <li class="dr-gap" data-testid={`dr-gap-${g.id}`}>
                <span class="dr-gap-id">{g.id}</span>
                {g.text}
              </li>
            {/each}
          </ul>
        {/if}
      </div>

      <div class="dr-panel">
        <h2>Budget ledger</h2>
        {#if !active || meterRows(active.budget).length === 0}
          <p class="dr-muted">No spend yet.</p>
        {:else}
          <ul class="dr-meters" data-testid="dr-meters">
            {#each meterRows(active.budget) as m (m.key)}
              <li class="dr-meter" data-testid={`dr-meter-${m.key}`}>
                <span class="dr-meter-key">{m.key}</span>
                <span class="dr-meter-bar" aria-hidden="true">
                  <span class="dr-meter-fill" style={`width:${m.pct}%`}></span>
                </span>
                <span class="dr-meter-num">
                  {m.spent} spent · {m.remaining} remaining
                </span>
              </li>
            {/each}
          </ul>
        {/if}
      </div>

      <div class="dr-panel">
        <h2>Consent status</h2>
        {#if active?.consent}
          <p class="dr-consent-live" data-testid="dr-consent-live">
            Granted: {active.consent.release_floor}
            <span class="dr-muted"
              >at {new Date(active.consent.granted_at_unix * 1000).toLocaleString()}</span
            >
          </p>
        {:else}
          <p class="dr-consent-deny-live" data-testid="dr-consent-deny-live">
            Default-deny — no release granted. Anything the web leg would
            need is refused.
          </p>
        {/if}
      </div>

      <div class="dr-run-actions">
        {#if stopConfirming}
          <div class="dr-stop-confirm" data-testid="dr-stop-confirm">
            <p>
              Stop this run and keep what it has gathered? The report is
              written from the evidence in hand, with the early stop declared
              in it.{#if hasResume()}
                You can resume from the last checkpoint afterwards.{/if}
            </p>
            <div class="dr-stop-buttons">
              <button
                type="button"
                class="dr-secondary"
                onclick={() => void confirmStop()}
                data-testid="dr-stop-confirm-yes"
              >
                Stop and keep the findings
              </button>
              <button
                type="button"
                class="dr-primary"
                onclick={() => (stopConfirming = false)}
                data-testid="dr-stop-cancel"
              >
                Keep running
              </button>
            </div>
          </div>
        {:else}
          <button
            type="button"
            class="dr-secondary"
            onclick={requestStop}
            disabled={!running || active?.stopRequested}
            data-testid="dr-abort"
          >
            {active?.stopRequested
              ? "Stopping — writing the report…"
              : "Stop and keep the findings"}
          </button>
        {/if}
        <button
          type="button"
          class="dr-linkish"
          onclick={() => (face = "ask")}
          data-testid="dr-run-to-ask"
        >
          Leave it running
        </button>
      </div>
    </section>

  {:else}
    <section class="dr-report" data-testid="dr-report-view">
      {#if reportError}
        <p class="dr-error" data-testid="dr-report-error">{reportError}</p>
      {:else if report}
        <div class="dr-report-head">
          <h2 data-testid="dr-report-question">{report.question}</h2>
          <p class="dr-muted">
            {report.run_id} · {report.terminal_state} · {report.rounds.length} rounds
          </p>
        </div>

        <div class="dr-report-body">
          {@html renderMarkdown(report.report_md)}
        </div>

        <div class="dr-dimensions">
          <div class="dr-panel">
            <h3>Checked claims</h3>
            {#if report.claims.length === 0}
              <p class="dr-muted">No claims reached the gate.</p>
            {:else}
              <ul class="dr-claims">
                {#each report.claims as c (c.id)}
                  <li class="dr-claim" data-testid={`dr-claim-${c.id}`}>
                    <span class="dr-verdict dr-verdict-{c.verdict}" data-testid={`dr-verdict-${c.id}`}>
                      {c.verdict}
                    </span>
                    <span class="dr-claim-text">{c.text}</span>
                    {#if c.corroboration}
                      <span class="dr-corroboration" data-testid={`dr-corroboration-${c.id}`}>
                        {c.corroboration.support_chunks} chunks from {c.corroboration.origins.length}
                        origins · floor {c.corroboration.floor}
                        {c.corroboration.passes_floor ? "· floor passed" : "· floor capped"}
                      </span>
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
          </div>

          {#if report.residue.length > 0}
            <div class="dr-panel">
              <h3>Residue — searched, no evidence either way</h3>
              <ul class="dr-residue" data-testid="dr-residue">
                {#each report.residue as r (r.query)}
                  <li class="dr-residue-row">
                    “{r.query}” <span class="dr-muted">(round {r.round})</span>
                  </li>
                {/each}
              </ul>
            </div>
          {/if}

          {#if report.reframe}
            <div class="dr-panel" data-testid="dr-reframe">
              <h3>The question was re-framed</h3>
              <p>
                Round {report.reframe.round}: “{report.reframe.original_question}”
                → “{report.reframe.reframed_question}”
              </p>
              <p class="dr-muted">{report.reframe.reason}</p>
            </div>
          {/if}

          {#if report.alignment}
            <div class="dr-panel" data-testid="dr-alignment">
              <h3>The question was redirected before acquisition</h3>
              <p>
                “{report.alignment.original_question}”
                → “{report.alignment.redirected_question}”
              </p>
              <p class="dr-muted">{report.alignment.reason}</p>
            </div>
          {/if}

          {#if report.not_covered.length > 0}
            <div class="dr-panel">
              <h3>Not covered</h3>
              <ul class="dr-not-covered" data-testid="dr-not-covered">
                {#each report.not_covered as q (q)}
                  <li>{q}</li>
                {/each}
              </ul>
            </div>
          {/if}

          <div class="dr-panel">
            <h3>Constitution — zero untraced figures in [passed]</h3>
            {#if report.constitution.passed_claims === 0 && report.constitution.violations.length === 0}
              <p class="dr-muted" data-testid="dr-constitution">
                No [passed] claims to check.
              </p>
            {:else if report.constitution.violations.length === 0}
              <p class="dr-ok" data-testid="dr-constitution">
                Position holds — {report.constitution.passed_claims} [passed]
                claims, every figure traced in the evidence.
              </p>
            {:else}
              <ul class="dr-violations" data-testid="dr-constitution-violations">
                {#each report.constitution.violations as v (v)}
                  <li class="dr-violation">{v}</li>
                {/each}
              </ul>
            {/if}
            {#if report.constitution.unresolved > 0}
              <p class="dr-muted" data-testid="dr-constitution-unresolved">
                {report.constitution.unresolved} [passed] claim(s) whose
                evidence could not be resolved — reported, never defaulted.
              </p>
            {/if}
          </div>
        </div>

        <div class="dr-report-actions">
          <button
            type="button"
            class="dr-secondary"
            onclick={askAgainOnEstate}
            data-testid="dr-ask-again"
          >
            Ask again on this estate
          </button>
          <button
            type="button"
            class="dr-secondary"
            onclick={onOpenLibrary}
            data-testid="dr-open-library"
          >
            Find it in Library
          </button>
        </div>
      {:else}
        <p class="dr-muted">Loading the checked report…</p>
      {/if}
    </section>
  {/if}

  <section class="dr-shelf" data-testid="dr-shelf">
    <h2>Runs</h2>
    {#if runsError}
      <p class="dr-error">{runsError}</p>
    {:else if runs.length === 0}
      <p class="dr-muted" data-testid="dr-shelf-empty">
        No runs yet — the first run's report and estate land here.
      </p>
    {:else}
      <ul class="dr-runs">
        {#each runs as r (r.run_id)}
          <li
            class="dr-run-row"
            class:live={r.live}
            data-testid={`dr-run-${r.run_id}`}
          >
            <div class="dr-run-meta">
              <span class="dr-run-question">{r.question ?? r.run_id}</span>
              <span class="dr-muted">
                {r.run_id} ·
                <span
                  class="dr-run-state"
                  class:live={r.live}
                  data-testid={`dr-run-state-${r.run_id}`}>{runStateLabel(r)}</span
                >
                · {r.rounds} rounds
                {r.consent ? ` · release: ${r.consent.release_floor}` : " · default-deny"}
              </span>
            </div>
            <div class="dr-run-actions">
              <!-- A run being driven right now is neither resumable nor
                   openable: resuming it would hand a second loop the run dir
                   the first is mid-write on, and the shelf used to offer
                   exactly that because an absent manifest was defaulted to
                   `interrupted`. The only thing to do with a live run is
                   watch it. -->
              {#if r.live}
                <button
                  type="button"
                  class="dr-secondary"
                  onclick={() => (face = "run")}
                  data-testid={`dr-watch-${r.run_id}`}
                >
                  Watch it run
                </button>
              {:else if r.report_present}
                <button
                  type="button"
                  class="dr-secondary"
                  onclick={() => void openReport(r.run_id)}
                  data-testid={`dr-open-${r.run_id}`}
                >
                  Open report
                </button>
              {:else if hasResume()}
                <button
                  type="button"
                  class="dr-secondary"
                  onclick={() => void startRun(r.run_id)}
                  data-testid={`dr-resume-${r.run_id}`}
                >
                  Resume
                </button>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>

<style>
  .dr-view {
    max-width: 960px;
    margin: 0 auto;
    padding: 24px 20px 64px;
    display: flex;
    flex-direction: column;
    gap: 20px;
  }
  .dr-header h1 {
    margin: 4px 0;
  }
  .dr-sub {
    color: var(--muted, #888);
    margin: 0 0 8px;
  }
  .dr-back {
    background: none;
    border: none;
    color: var(--accent, #4a9eff);
    cursor: pointer;
    padding: 0;
  }
  .dr-caps-error,
  .dr-error {
    color: #d9534f;
    background: rgba(217, 83, 79, 0.08);
    border: 1px solid rgba(217, 83, 79, 0.35);
    border-radius: 6px;
    padding: 8px 10px;
  }
  .dr-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .dr-label {
    font-weight: 600;
    font-size: 13px;
  }
  .dr-question textarea {
    width: 100%;
    box-sizing: border-box;
    resize: vertical;
    border-radius: 8px;
    padding: 10px;
    font: inherit;
    border: 1px solid var(--border, #333);
    background: var(--surface, #17171b);
    color: inherit;
  }
  .dr-row {
    display: flex;
    gap: 12px;
  }
  .dr-number input {
    width: 96px;
    border-radius: 6px;
    padding: 6px 8px;
    border: 1px solid var(--border, #333);
    background: var(--surface, #17171b);
    color: inherit;
  }
  .dr-consent {
    border: 1px solid var(--border, #333);
    border-radius: 8px;
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .dr-consent legend {
    padding: 0 6px;
  }
  .dr-consent-option {
    display: flex;
    gap: 8px;
    align-items: baseline;
    cursor: pointer;
  }
  .dr-consent-option em {
    color: var(--muted, #888);
    font-style: normal;
    font-size: 12px;
  }
  .dr-corpora-list {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .dr-corpus-chip {
    display: inline-flex;
    gap: 6px;
    align-items: center;
    border: 1px solid var(--border, #333);
    border-radius: 999px;
    padding: 4px 10px;
    font-size: 13px;
    cursor: pointer;
  }
  .dr-primary {
    align-self: flex-start;
    border: none;
    border-radius: 8px;
    background: var(--accent, #4a9eff);
    color: #fff;
    padding: 10px 22px;
    font-weight: 600;
    cursor: pointer;
  }
  .dr-primary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .dr-secondary {
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    background: none;
    color: inherit;
    padding: 6px 12px;
    cursor: pointer;
    font-size: 13px;
  }
  .dr-run-head {
    display: flex;
    gap: 10px;
    align-items: center;
  }
  .dr-stage {
    /* No `capitalize`: the label is a sentence-cased phrase now, and
       title-casing it produced "Gathering And Checking Evidence". An
       unknown stage arrives verbatim from the backend and is shown that
       way, lowercase and all — it is the raw value, and dressing it up
       would misrepresent how much we know about it. */
    background: var(--accent, #4a9eff);
    color: #fff;
    border-radius: 999px;
    padding: 2px 12px;
    font-size: 13px;
    font-weight: 600;
  }
  .dr-round {
    font-weight: 600;
  }
  .dr-run-id {
    color: var(--muted, #888);
    font-size: 12px;
  }
  .dr-panel {
    border: 1px solid var(--border, #333);
    border-radius: 8px;
    padding: 12px 14px;
  }
  .dr-panel h2,
  .dr-panel h3 {
    margin: 0 0 8px;
    font-size: 15px;
  }
  .dr-muted {
    color: var(--muted, #888);
    font-size: 13px;
  }
  .dr-gaps,
  .dr-claims,
  .dr-residue,
  .dr-not-covered,
  .dr-runs {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .dr-gap {
    display: flex;
    gap: 8px;
    font-size: 14px;
  }
  .dr-gap-id {
    color: var(--muted, #888);
    font-size: 12px;
    line-height: 20px;
  }
  .dr-meters {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .dr-meter {
    display: flex;
    gap: 10px;
    font-size: 14px;
  }
  .dr-meter-key {
    min-width: 90px;
    font-weight: 600;
  }
  .dr-meter-bar {
    flex: 1;
    min-width: 60px;
    height: 6px;
    align-self: center;
    border-radius: 999px;
    background: var(--border, #333);
    overflow: hidden;
  }
  .dr-meter-fill {
    display: block;
    height: 100%;
    background: var(--accent, #4a9eff);
    transition: width 240ms ease-out;
  }
  .dr-meter-num {
    color: var(--muted, #888);
    font-size: 13px;
    white-space: nowrap;
  }
  .dr-consent-deny-live {
    color: #c9a227;
  }
  .dr-claim {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 14px;
  }
  .dr-verdict {
    align-self: flex-start;
    border-radius: 999px;
    padding: 1px 10px;
    font-size: 12px;
    font-weight: 600;
  }
  .dr-verdict-passed {
    background: rgba(58, 164, 95, 0.18);
    color: #3aa45f;
  }
  .dr-verdict-failed {
    background: rgba(217, 83, 79, 0.18);
    color: #d9534f;
  }
  .dr-verdict-could-not-judge {
    background: rgba(201, 162, 39, 0.18);
    color: #c9a227;
  }
  .dr-verdict-never-ran {
    background: rgba(136, 136, 136, 0.18);
    color: #888;
  }
  .dr-corroboration {
    font-size: 12px;
    color: var(--muted, #888);
  }
  .dr-residue-row {
    font-size: 14px;
  }
  .dr-violations {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .dr-violation {
    color: #d9534f;
    font-size: 13px;
  }
  .dr-ok {
    color: #3aa45f;
  }
  .dr-report-actions,
  .dr-run-actions {
    display: flex;
    gap: 10px;
    align-items: center;
    flex-wrap: wrap;
  }
  .dr-shelf {
    border-top: 1px solid var(--border, #333);
    padding-top: 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .dr-shelf h2 {
    margin: 0;
    font-size: 15px;
  }
  .dr-run-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    border: 1px solid var(--border, #333);
    border-radius: 8px;
    padding: 10px 12px;
  }
  .dr-run-meta {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .dr-run-question {
    font-weight: 600;
  }
  /* ── The pulse, and the liveness verdict ──────────────────────────── */
  .dr-pulse {
    display: flex;
    gap: 10px;
    align-items: center;
    flex-wrap: wrap;
  }
  /* A beat that keeps time with the backend's heartbeat. It stops moving
     when the signal does — the animation IS the signal, so a frozen dot
     and a frozen run are the same picture on purpose. */
  .dr-beat {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent, #4a9eff);
    animation: dr-beat 2s ease-in-out infinite;
  }
  .dr-beat.stalled {
    background: #c9a227;
    animation: none;
  }
  @keyframes dr-beat {
    0%,
    100% {
      opacity: 0.25;
      transform: scale(0.8);
    }
    50% {
      opacity: 1;
      transform: scale(1.15);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dr-beat {
      animation: none;
      opacity: 1;
    }
  }
  .dr-elapsed {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }
  .dr-liveness {
    margin: 0;
    font-size: 13px;
    color: var(--muted, #888);
  }
  .dr-liveness[data-state="no-signal"] {
    color: #c9a227;
    font-weight: 600;
  }
  .dr-pulse[data-liveness="no-signal"] .dr-stage {
    background: #c9a227;
    color: #1a1a1a;
  }
  .dr-detach-note {
    margin: 0;
    font-size: 13px;
    color: var(--muted, #888);
    border-left: 2px solid var(--border, #333);
    padding-left: 10px;
  }

  /* ── The trail: what it has already done ──────────────────────────── */
  .dr-trail {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .dr-trail-head {
    display: flex;
    gap: 8px;
    align-items: baseline;
  }
  .dr-trail-round {
    font-weight: 600;
    font-size: 14px;
  }
  .dr-trail-gaps {
    margin: 4px 0 0;
    padding-left: 18px;
    font-size: 13px;
    color: var(--muted, #888);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  /* ── Stopping is the operator's call, and it is confirmed ──────────── */
  .dr-stop-confirm {
    border: 1px solid #c9a227;
    background: rgba(201, 162, 39, 0.08);
    border-radius: 8px;
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    font-size: 13px;
  }
  .dr-stop-confirm p {
    margin: 0;
  }
  .dr-stop-buttons {
    display: flex;
    gap: 10px;
  }
  .dr-linkish {
    background: none;
    border: none;
    color: var(--accent, #4a9eff);
    cursor: pointer;
    padding: 0;
    font-size: 13px;
  }

  /* ── A run in flight, seen from the composer and the shelf ─────────── */
  .dr-running-notice {
    display: flex;
    gap: 12px;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    border: 1px solid var(--accent, #4a9eff);
    background: color-mix(in srgb, var(--accent, #4a9eff) 8%, transparent);
    border-radius: 8px;
    padding: 8px 12px;
    font-size: 13px;
  }
  .dr-run-state.live {
    color: var(--accent, #4a9eff);
    font-weight: 600;
  }
  .dr-run-row.live {
    border-left: 2px solid var(--accent, #4a9eff);
    padding-left: 10px;
  }
</style>
