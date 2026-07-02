<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  SetupReportCard — renders the "What setup did" record written at the end of
  onboarding (~/.svrnmesh/setup-report.json). Part of making setup glassbox:
  the consequential decisions (hardware → profile → models, with their sources
  and destinations) stay inspectable after the fact, not just during setup.
  Renders nothing if no report exists (e.g. a desktop attached to a daemon
  that did its own setup).
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { getSetupReport } from "../api";

  type ReportModel = {
    role: string;
    name: string;
    quant: string;
    size_gb: number;
    repo: string;
    dest: string;
  };
  type SetupReport = {
    completed_at: string;
    hardware: { effective_memory_gb: number; is_unified_memory: boolean };
    profile: string;
    primary_customized: boolean;
    models: ReportModel[];
  };

  let report = $state<SetupReport | null>(null);
  let loaded = $state(false);

  onMount(async () => {
    try {
      const raw = await getSetupReport();
      if (raw) report = JSON.parse(raw) as SetupReport;
    } catch {
      // No report / unreadable — render nothing.
    }
    loaded = true;
  });

  function fmtGb(n: number): string {
    return n < 1 ? `${Math.round(n * 1024)} MB` : `${n.toFixed(n < 10 ? 1 : 0)} GB`;
  }
  function fmtWhen(iso: string): string {
    const d = new Date(iso);
    return isNaN(d.getTime()) ? iso : d.toLocaleString();
  }
</script>

{#if loaded && report}
  <div class="setup-report">
    <h3 class="eyebrow">What setup did</h3>
    <p class="meta">
      {fmtWhen(report.completed_at)} · {report.hardware.effective_memory_gb.toFixed(0)} GB
      {report.hardware.is_unified_memory ? "unified memory" : "GPU / RAM"} · profile
      <code>{report.profile}</code>
    </p>
    <ul class="models">
      {#each report.models as m (m.role)}
        <li>
          <span class="role">{m.role}</span>
          <b>{m.name}</b>
          <span class="quant">{m.quant} · {fmtGb(m.size_gb)}</span>
          <span class="prov">from <code>{m.repo}</code> → <code>{m.dest}</code></span>
        </li>
      {/each}
    </ul>
    <p class="note">
      Primary model:
      {report.primary_customized ? "customized by you at setup" : "hardware-recommended default"}.
      Full record at <code>~/.svrnmesh/setup-report.json</code> (and <code>.md</code>).
    </p>
  </div>
{/if}

<style>
  .setup-report {
    margin-top: 18px;
    padding-top: 16px;
    border-top: 1px solid var(--border);
  }
  .eyebrow {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    font-weight: 600;
    color: var(--text-secondary);
    margin: 0 0 6px;
  }
  .meta {
    font-size: 0.84rem;
    color: var(--text-secondary);
    margin: 0 0 10px;
    line-height: 1.5;
  }
  .models {
    list-style: none;
    padding: 0;
    margin: 0 0 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .models li {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 8px;
    font-size: 0.86rem;
    line-height: 1.5;
  }
  .role {
    font-size: 0.72rem;
    color: var(--text-muted);
    min-width: 4.5rem;
  }
  .quant {
    font-family: var(--font-mono);
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  .prov {
    flex-basis: 100%;
    font-size: 0.76rem;
    color: var(--text-muted);
  }
  .prov code,
  .meta code,
  .note code {
    font-family: var(--font-mono);
    font-size: 0.92em;
    color: var(--text-secondary);
    word-break: break-all;
  }
  .note {
    font-size: 0.78rem;
    line-height: 1.5;
    color: var(--text-muted);
    margin: 0;
  }
</style>
