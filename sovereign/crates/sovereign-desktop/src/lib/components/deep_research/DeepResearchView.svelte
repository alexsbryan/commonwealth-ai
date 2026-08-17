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
  import { listen } from "@tauri-apps/api/event";
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
    DeepResearchRunProgress,
    DrBudget,
    DrCapabilities,
    DrConsent,
    DrGap,
    DrReport,
    DrRunSummary,
  } from "../../types";
  import { renderMarkdown } from "../../utils/markdown";

  let {
    onExit,
    onOpenLibrary,
  }: { onExit: () => void; onOpenLibrary: () => void } = $props();

  // ── State tiers ─────────────────────────────────────────────
  // UI state is $state; Tauri events are INPUTS that update it in the
  // listen handler — never a second source of truth.

  type Face = "ask" | "run" | "report";
  let face = $state<Face>("ask");
  let running = $state(false);

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

  // Run view
  let jobId = $state<string | null>(null);
  let runId = $state<string | null>(null);
  let liveRound = $state<number | null>(null);
  let liveStage = $state<string>("");
  let liveGaps = $state<DrGap[]>([]);
  let liveBudget = $state<DrBudget>({ spent: {}, remaining: {} });
  let liveConsent = $state<DrConsent | null>(null);
  let runFailed = $state<string | null>(null);
  let unlisten: (() => void) | null = null;

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
    return () => unlisten?.();
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
    runFailed = null;
    report = null;
    try {
      const handle = await drStart(question, {
        maxRounds,
        search: searchBudget,
        fetch: fetchBudget,
        corpora: selectedCorpora,
        consent: consent === "" ? null : consent,
        resumeRunId: resumeRunId ?? null,
      });
      jobId = handle.job_id;
      running = true;
      face = "run";
      unlisten?.();
      unlisten = await listen<DeepResearchRunProgress>(handle.channel, (ev) => {
        const e = ev.payload;
        if (e.kind === "started") {
          runId = e.run_id;
        } else if (e.kind === "live") {
          liveRound = e.round;
          liveStage = e.stage;
          liveGaps = e.gaps;
          liveBudget = e.budget;
          liveConsent = e.consent;
        } else if (e.kind === "report_ready") {
          running = false;
          report = e.report;
          face = "report";
        } else if (e.kind === "failed") {
          running = false;
          runFailed = e.error;
        }
      });
    } catch (e) {
      running = false;
      startError = typeof e === "string" ? e : String(e);
    }
  }

  /// Kill the child; the run dir keeps its artifacts and the driver's
  /// failed event carries the truthful terminal message (resume is
  /// available when the verb supports it).
  async function abortRun() {
    if (!running || !jobId) return;
    try {
      await drAbort(jobId);
      running = false;
      void loadRuns();
    } catch (e) {
      runFailed = typeof e === "string" ? e : String(e);
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

  // ── Budget rendering helpers ────────────────────────────────

  function meterRows(budget: DrBudget): { key: string; spent: number; remaining: number }[] {
    const keys = new Set([...Object.keys(budget.spent), ...Object.keys(budget.remaining)]);
    return [...keys]
      .sort()
      .map((key) => ({
        key,
        spent: budget.spent[key] ?? 0,
        remaining: budget.remaining[key] ?? 0,
      }));
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

      <button
        type="button"
        class="dr-primary"
        onclick={() => void startRun()}
        disabled={question.trim() === "" && !hasResume()}
        data-testid="dr-start"
      >
        Start the run
      </button>
    </section>

  {:else if face === "run"}
    <section class="dr-run" data-testid="dr-run-view">
      <div class="dr-run-head">
        <span class="dr-stage" data-testid="dr-stage">{liveStage}</span>
        {#if liveRound !== null}
          <span class="dr-round" data-testid="dr-round">round {liveRound}</span>
        {/if}
        {#if runId}
          <span class="dr-run-id" data-testid="dr-run-id">{runId}</span>
        {/if}
      </div>

      <div class="dr-panel">
        <h2>The gate's named gaps</h2>
        {#if liveGaps.length === 0}
          <p class="dr-muted" data-testid="dr-gaps-empty">
            No gaps named yet — the compass speaks after a round completes.
          </p>
        {:else}
          <ul class="dr-gaps" data-testid="dr-gaps">
            {#each liveGaps as g (g.id)}
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
        {#if meterRows(liveBudget).length === 0}
          <p class="dr-muted">No spend yet.</p>
        {:else}
          <ul class="dr-meters" data-testid="dr-meters">
            {#each meterRows(liveBudget) as m (m.key)}
              <li class="dr-meter" data-testid={`dr-meter-${m.key}`}>
                <span class="dr-meter-key">{m.key}</span>
                <span class="dr-meter-spent">{m.spent} spent</span>
                <span class="dr-meter-remaining">{m.remaining} remaining</span>
              </li>
            {/each}
          </ul>
        {/if}
      </div>

      <div class="dr-panel">
        <h2>Consent status</h2>
        {#if liveConsent}
          <p class="dr-consent-live" data-testid="dr-consent-live">
            Granted: {liveConsent.release_floor}
            <span class="dr-muted">at {new Date(liveConsent.granted_at_unix * 1000).toLocaleString()}</span>
          </p>
        {:else}
          <p class="dr-consent-deny-live" data-testid="dr-consent-deny-live">
            Default-deny — no release granted. Anything the web leg would
            need is refused.
          </p>
        {/if}
      </div>

      {#if runFailed}
        <p class="dr-error" data-testid="dr-run-failed">{runFailed}</p>
      {/if}

      <div class="dr-run-actions">
        <button
          type="button"
          class="dr-secondary"
          onclick={() => void abortRun()}
          disabled={!running}
          data-testid="dr-abort"
        >
          Abort the run
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
    <h2>Previous runs</h2>
    {#if runsError}
      <p class="dr-error">{runsError}</p>
    {:else if runs.length === 0}
      <p class="dr-muted" data-testid="dr-shelf-empty">
        No runs yet — the first run's report and estate land here.
      </p>
    {:else}
      <ul class="dr-runs">
        {#each runs as r (r.run_id)}
          <li class="dr-run-row" data-testid={`dr-run-${r.run_id}`}>
            <div class="dr-run-meta">
              <span class="dr-run-question">{r.question ?? r.run_id}</span>
              <span class="dr-muted">
                {r.run_id} · {r.terminal_state} · {r.rounds} rounds
                {r.consent ? ` · release: ${r.consent.release_floor}` : " · default-deny"}
              </span>
            </div>
            <div class="dr-run-actions">
              {#if r.report_present}
                <button
                  type="button"
                  class="dr-secondary"
                  onclick={() => void openReport(r.run_id)}
                  data-testid={`dr-open-${r.run_id}`}
                >
                  Open report
                </button>
              {/if}
              {#if hasResume() && !r.report_present}
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
    text-transform: capitalize;
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
  .dr-meter-spent {
    color: var(--accent, #4a9eff);
  }
  .dr-meter-remaining {
    color: var(--muted, #888);
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
</style>
