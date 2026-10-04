<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { runHealthCheck, prepareDiagnosticReport } from "../api";
  // Defined in api.ts beside `CrashReportInfo`, which is the local
  // convention for types that exist only as a command's wire shape.
  import type {
    HealthReport,
    HealthCheck,
    CheckStatus,
    ReportReason,
  } from "../api";

  // The self-service half of remote support. Two jobs, in order:
  //
  // 1. Let the user fix it themselves. Every non-OK check carries a
  //    fix_hint written for someone with no terminal, so the majority
  //    of "it's broken" moments should end here without anyone being
  //    contacted. That is the whole design goal — not a nicer bug
  //    report, but fewer bug reports.
  // 2. When it can't be self-fixed, produce one file that is enough
  //    to debug from. Nothing auto-uploads: the file lands on the
  //    Desktop and the user reads it before sending it anywhere.
  //
  // Deliberately NOT gated on a crash. The crash bundle it sits beside
  // only appears after the daemon dies, which is the minority of the
  // ways this product fails somebody.

  let report: HealthReport | null = $state(null);
  let loading = $state(true);
  let error = $state("");

  // Report composer.
  let composing = $state(false);
  let reason: ReportReason = $state("other");
  let note = $state("");
  let busy = $state(false);
  let savedPath = $state("");

  const REASONS: { value: ReportReason; label: string }[] = [
    { value: "slow", label: "It's too slow" },
    { value: "answer", label: "An answer was wrong" },
    { value: "mesh", label: "I can't see other people" },
    { value: "import", label: "Something wouldn't import" },
    { value: "crash", label: "It crashed or froze" },
    { value: "other", label: "Something else" },
  ];

  function glyph(s: CheckStatus): string {
    return s === "ok" ? "✓" : s === "warn" ? "!" : s === "fail" ? "✗" : "?";
  }

  async function refresh() {
    loading = true;
    try {
      report = await runHealthCheck();
      error = "";
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function send() {
    busy = true;
    try {
      const info = await prepareDiagnosticReport(reason, note);
      savedPath = info.report_path;
      composing = false;
      error = "";
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  // Ordered worst-first so the thing that needs attention is at the
  // top of the list rather than wherever the check happens to be
  // declared. A user scanning this should not have to read all seven.
  const RANK: Record<CheckStatus, number> = {
    fail: 0,
    warn: 1,
    unknown: 2,
    ok: 3,
  };
  const ordered = $derived.by((): HealthCheck[] => {
    const r = report;
    if (!r) return [];
    return [...r.checks].sort((a, b) => RANK[a.status] - RANK[b.status]);
  });
  const needsAttention = $derived(
    ordered.filter((c) => c.status === "fail" || c.status === "warn").length,
  );

  onMount(refresh);
</script>

<div class="health">
  <div class="health-header">
    <span class="health-label">Health check</span>
    <button class="linkish" onclick={refresh} disabled={loading}>
      {loading ? "Checking…" : "Re-check"}
    </button>
  </div>

  {#if error}
    <div class="health-error">{error}</div>
  {/if}

  {#if report}
    <p class="summary">
      {#if needsAttention === 0}
        Everything looks healthy.
      {:else}
        {needsAttention} of {report.checks.length} checks need attention.
      {/if}
    </p>

    <ul class="checks">
      {#each ordered as c (c.id)}
        <li class="check {c.status}">
          <div class="check-head">
            <span class="glyph" aria-hidden="true">{glyph(c.status)}</span>
            <span class="check-label">{c.label}</span>
            <!-- The stable id is shown, not hidden: it is what a
                 support conversation names, and it has to survive a
                 screenshot. -->
            <code class="check-id">{c.id}</code>
          </div>
          <div class="check-detail">{c.detail}</div>
          {#if c.fix_hint}
            <div class="check-fix">
              <strong>Try this:</strong>
              {c.fix_hint}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {:else if loading}
    <p class="summary">Checking…</p>
  {/if}

  <div class="report-area">
    {#if savedPath}
      <p class="saved">
        Report saved. Open it, read it, then send it to whoever set up your mesh.
      </p>
      <code class="path">{savedPath}</code>
      <button class="linkish" onclick={() => (savedPath = "")}>Report something else</button>
    {:else if composing}
      <label class="field">
        <span>What went wrong?</span>
        <select bind:value={reason}>
          {#each REASONS as r (r.value)}
            <option value={r.value}>{r.label}</option>
          {/each}
        </select>
      </label>
      <label class="field">
        <span>What were you doing, and what did you expect instead?</span>
        <textarea
          bind:value={note}
          rows="4"
          placeholder="e.g. I asked about last quarter's numbers and it said it had no sources, but the document is in my library."
        ></textarea>
      </label>
      <p class="privacy">
        This writes a file to your Desktop containing your app version, settings and
        the check results above — <strong>not</strong> your documents, conversations
        or answers. Nothing is sent anywhere. You read it, then send it yourself.
      </p>
      <div class="row">
        <button class="primary" onclick={send} disabled={busy}>
          {busy ? "Writing…" : "Create report"}
        </button>
        <button class="linkish" onclick={() => (composing = false)} disabled={busy}>
          Cancel
        </button>
      </div>
    {:else}
      <button class="primary" onclick={() => (composing = true)}>
        Something's wrong — create a report
      </button>
    {/if}
  </div>
</div>

<style>
  .health {
    margin-top: 20px;
    padding: 14px 16px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .health-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .health-label {
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--text-muted);
  }

  .summary {
    margin: 0;
    font-size: 0.85rem;
  }

  .health-error {
    font-size: 0.78rem;
    color: var(--danger, #e5484d);
  }

  .checks {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .check {
    padding: 8px 10px;
    border-radius: var(--radius);
    border-left: 3px solid var(--border);
    background: var(--bg-primary, transparent);
  }
  .check.fail {
    border-left-color: var(--danger, #e5484d);
  }
  .check.warn {
    border-left-color: var(--warning, #f5a524);
  }
  .check.ok {
    border-left-color: var(--success, #30a46c);
  }

  .check-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }

  .glyph {
    font-weight: 700;
  }
  .check.fail .glyph {
    color: var(--danger, #e5484d);
  }
  .check.warn .glyph {
    color: var(--warning, #f5a524);
  }
  .check.ok .glyph {
    color: var(--success, #30a46c);
  }

  .check-label {
    font-weight: 600;
    font-size: 0.85rem;
  }

  .check-id {
    font-size: 0.68rem;
    color: var(--text-muted);
  }

  .check-detail {
    font-size: 0.8rem;
    margin-top: 2px;
  }

  .check-fix {
    font-size: 0.78rem;
    margin-top: 4px;
    color: var(--text-muted);
  }

  .report-area {
    display: flex;
    flex-direction: column;
    gap: 8px;
    border-top: 1px solid var(--border);
    padding-top: 10px;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 0.8rem;
  }

  .field textarea,
  .field select {
    font: inherit;
    padding: 6px 8px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--bg-primary);
    color: inherit;
  }

  .privacy {
    margin: 0;
    font-size: 0.75rem;
    color: var(--text-muted);
    line-height: 1.45;
  }

  .saved {
    margin: 0;
    font-size: 0.82rem;
  }

  .path {
    font-size: 0.72rem;
    word-break: break-all;
    color: var(--text-muted);
  }

  .row {
    display: flex;
    gap: 8px;
    align-items: center;
  }

  button.primary {
    font: inherit;
    font-size: 0.82rem;
    padding: 6px 12px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--accent, #4a9eff);
    color: #fff;
    cursor: pointer;
  }

  button.linkish {
    font: inherit;
    font-size: 0.78rem;
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    text-decoration: underline;
    padding: 0;
  }

  button:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
