<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import {
    listCrashRecords,
    deleteCrashRecord,
    exportCrashRecord,
    type CrashRecord,
  } from "../api";

  // Self-contained Diagnostics surface: the accessible face of the local
  // crash store (~/.sovereign/crashes/*.json). svrnmesh is decentralized —
  // there is no central error pipeline — so a crash has to be reviewable
  // and shareable *from the machine it happened on*. Nothing auto-uploads:
  // the user reads a record, and (one click) exports a redacted copy they
  // can attach to a GitHub issue.

  let records: CrashRecord[] = $state([]);
  let loading = $state(true);
  let loadError: string | null = $state(null);
  let expanded: string | null = $state(null);
  let busyId: string | null = $state(null);
  let exportedPath: string | null = $state(null);
  let actionError: string | null = $state(null);

  async function refresh() {
    loading = true;
    loadError = null;
    try {
      records = await listCrashRecords();
    } catch (e) {
      loadError = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  onMount(refresh);

  function toggle(id: string) {
    expanded = expanded === id ? null : id;
  }

  function fmtTime(unix: number): string {
    try {
      return new Date(unix * 1000).toLocaleString();
    } catch {
      return String(unix);
    }
  }

  function kindLabel(k: CrashRecord["kind"]): string {
    return k === "panic" ? "App panic" : "Model crash";
  }

  function basename(p: string | null | undefined): string {
    if (!p) return "";
    const parts = p.split(/[\\/]/);
    return parts[parts.length - 1] || p;
  }

  async function handleExport(rec: CrashRecord) {
    if (busyId) return;
    busyId = rec.id;
    actionError = null;
    exportedPath = null;
    try {
      const info = await exportCrashRecord(rec.id);
      exportedPath = info.report_path;
      // Open the project's GitHub Issues page so the user can file it and
      // attach the redacted markdown we just wrote. Best-effort — the path
      // stays visible even if no default browser is configured.
      try {
        await invoke("plugin:shell|open", { path: info.issues_url });
      } catch {
        /* no-op: path is surfaced below regardless */
      }
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      busyId = null;
    }
  }

  async function handleDelete(rec: CrashRecord) {
    if (busyId) return;
    busyId = rec.id;
    actionError = null;
    try {
      await deleteCrashRecord(rec.id);
      records = records.filter((r) => r.id !== rec.id);
      if (expanded === rec.id) expanded = null;
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      busyId = null;
    }
  }
</script>

<section class="doc-section">
  <span class="section-eyebrow">crashes &middot; panics &middot; sharing</span>
  <h2 class="doc-h2">Diagnostics</h2>
  <p class="doc-intro">
    When something breaks hard — the app panics, or a model takes the inference
    process down with it — svrnmesh writes down what happened, here, on your
    machine. Nothing leaves it on its own. Read a record; if you want to help fix
    it, export a redacted copy and send it along. Be patient, be constructive,
    then demand the best.
  </p>

  {#if loading}
    <p class="cd-muted">Loading records…</p>
  {:else if loadError}
    <p class="cd-error">Couldn't read crash records: {loadError}</p>
  {:else if records.length === 0}
    <div class="cd-empty">
      <p class="cd-empty-title">Nothing's fallen over</p>
      <p class="cd-muted">No crashes recorded on this machine. 🎉</p>
    </div>
  {:else}
    <ul class="cd-list">
      {#each records as rec (rec.id)}
        <li class="cd-item" class:cd-item--open={expanded === rec.id}>
          <button class="cd-head" onclick={() => toggle(rec.id)} aria-expanded={expanded === rec.id}>
            <span class="cd-kind" class:cd-kind--native={rec.kind === "native-crash"}>
              {kindLabel(rec.kind)}
            </span>
            <span class="cd-summary">{rec.summary}</span>
            <span class="cd-when">{fmtTime(rec.captured_at_unix)}</span>
          </button>

          {#if expanded === rec.id}
            <div class="cd-body">
              <dl class="cd-meta">
                <div><dt>App version</dt><dd>{rec.app_version}</dd></div>
                <div><dt>Platform</dt><dd>{rec.os} · {rec.cpu_arch}</dd></div>
                {#if rec.model_path}
                  <div><dt>Model</dt><dd>{basename(rec.model_path)}</dd></div>
                {/if}
                {#if rec.model_arch}
                  <div><dt>Architecture</dt><dd>{rec.model_arch}</dd></div>
                {/if}
                {#if rec.gpu_layers != null}
                  <div><dt>GPU layers</dt><dd>{rec.gpu_layers}</dd></div>
                {/if}
                {#if rec.signal != null}
                  <div><dt>Signal</dt><dd>{rec.signal}</dd></div>
                {/if}
              </dl>

              {#if rec.detail}
                <pre class="cd-detail">{rec.detail}</pre>
              {/if}

              <div class="cd-actions">
                <button
                  class="cd-btn cd-btn--primary"
                  onclick={() => handleExport(rec)}
                  disabled={busyId === rec.id}
                >
                  {busyId === rec.id ? "Preparing…" : "Export & report"}
                </button>
                <button
                  class="cd-btn"
                  onclick={() => handleDelete(rec)}
                  disabled={busyId === rec.id}
                >
                  Delete
                </button>
              </div>
            </div>
          {/if}
        </li>
      {/each}
    </ul>

    {#if exportedPath}
      <p class="cd-note">
        Redacted report saved to <code>{exportedPath}</code> — attach it to the
        GitHub issue that just opened.
      </p>
    {/if}
    {#if actionError}
      <p class="cd-error">{actionError}</p>
    {/if}
  {/if}
</section>

<style>
  .cd-muted {
    color: oklch(52% 0.01 250);
    font-size: 0.85rem;
  }
  .cd-error {
    color: oklch(45% 0.13 25);
    font-size: 0.85rem;
  }

  .cd-empty {
    padding: 20px;
    border: 1px dashed oklch(82% 0.01 250 / 0.7);
    border-radius: 8px;
    text-align: center;
  }
  .cd-empty-title {
    font-weight: 600;
    margin-bottom: 2px;
  }

  .cd-list {
    list-style: none;
    margin: 12px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .cd-item {
    border: 1px solid oklch(85% 0.01 250 / 0.7);
    border-radius: 8px;
    overflow: hidden;
    background: oklch(99% 0.003 250);
  }
  .cd-item--open {
    border-color: oklch(72% 0.06 250 / 0.7);
  }

  .cd-head {
    width: 100%;
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 12px;
    padding: 10px 14px;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    font-family: var(--font-sans);
  }
  .cd-head:hover {
    background: oklch(50% 0.02 250 / 0.04);
  }

  .cd-kind {
    flex: 0 0 auto;
    font-size: 0.68rem;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    padding: 2px 8px;
    border-radius: 999px;
    background: oklch(92% 0.04 250);
    color: oklch(38% 0.09 250);
  }
  .cd-kind--native {
    background: oklch(93% 0.06 40);
    color: oklch(42% 0.13 40);
  }

  .cd-summary {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.85rem;
    color: oklch(30% 0.01 250);
  }
  .cd-when {
    flex: 0 0 auto;
    font-size: 0.72rem;
    color: oklch(58% 0.01 250);
    font-variant-numeric: tabular-nums;
  }

  .cd-body {
    padding: 4px 14px 14px;
    border-top: 1px solid oklch(90% 0.01 250 / 0.7);
  }

  .cd-meta {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
    gap: 6px 20px;
    margin: 12px 0;
  }
  .cd-meta div {
    display: flex;
    flex-direction: column;
  }
  .cd-meta dt {
    font-size: 0.66rem;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: oklch(58% 0.01 250);
  }
  .cd-meta dd {
    margin: 0;
    font-size: 0.82rem;
    color: oklch(28% 0.01 250);
  }

  .cd-detail {
    margin: 0 0 12px;
    padding: 10px 12px;
    max-height: 280px;
    overflow: auto;
    background: oklch(96% 0.005 250);
    border: 1px solid oklch(88% 0.01 250 / 0.7);
    border-radius: 6px;
    font-family: var(--font-mono);
    font-size: 0.72rem;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
    color: oklch(32% 0.01 250);
  }

  .cd-actions {
    display: flex;
    gap: 8px;
  }
  .cd-btn {
    font-family: var(--font-sans);
    font-size: 0.78rem;
    font-weight: 500;
    letter-spacing: 0.04em;
    color: oklch(38% 0.02 250);
    background: none;
    border: 1px solid oklch(78% 0.02 250 / 0.8);
    padding: 5px 14px;
    border-radius: 6px;
    cursor: pointer;
    transition: background 160ms ease;
  }
  .cd-btn:hover:not(:disabled) {
    background: oklch(50% 0.02 250 / 0.06);
  }
  .cd-btn:disabled {
    opacity: 0.6;
    cursor: progress;
  }
  .cd-btn--primary {
    color: oklch(36% 0.1 250);
    border-color: oklch(66% 0.08 250 / 0.8);
    background: oklch(96% 0.03 250);
  }

  .cd-note {
    margin-top: 12px;
    padding: 8px 12px;
    background: oklch(98% 0.005 250);
    border: 1px solid oklch(85% 0.01 250 / 0.7);
    border-radius: 6px;
    font-size: 0.78rem;
    color: oklch(35% 0.012 250);
  }
  .cd-note code {
    font-family: var(--font-mono);
    background: oklch(94% 0.008 250);
    padding: 1px 5px;
    border-radius: 3px;
    word-break: break-all;
  }
</style>
