<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  The "Run a workflow" surface (VISION's first verb — Use). Pick a starter (or
  one you authored), fill in a folder + any params it declares, watch it run
  step-by-step, then chat with the corpus it built. Runs in-process via the
  `workflow_run` command, which drives the same Runner the CLI uses — no
  subprocess, nothing leaves the machine.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import { workflowListRunnable, workflowCapabilities, workflowRun } from "../../api";
  import type { WorkflowCatalogEntry, WorkflowRunProgress } from "../../types";
  import WorkflowRunProgressPanel from "./WorkflowRunProgress.svelte";

  // `onOpenChat` lands the user in chat once a run builds a searchable corpus —
  // the host (App.svelte) passes the same handler Settings → Knowledge uses.
  // `preselectName` deep-links from the recipe-author dashboard's "Run it".
  let {
    onOpenChat,
    preselectName = null,
  }: {
    onOpenChat?: () => void;
    preselectName?: string | null;
  } = $props();

  let workflows = $state<WorkflowCatalogEntry[]>([]);
  let loadError = $state<string | null>(null);
  let selected = $state<WorkflowCatalogEntry | null>(null);
  let paramValues = $state<Record<string, string>>({});
  let capabilities = $state<string[]>([]);

  type Stage = "idle" | "running" | "done" | "failed";
  let stage = $state<Stage>("idle");
  let lines = $state<string[]>([]);
  let runInfo = $state<{ items: number; steps: number } | null>(null);
  let error = $state<string | null>(null);
  let corpusBuilt = $state<string | null>(null);

  let unlisten: (() => void) | null = null;

  onMount(() => {
    void load();
    return () => unlisten?.();
  });

  async function load() {
    try {
      workflows = await workflowListRunnable();
      const pick =
        (preselectName && workflows.find((w) => w.name === preselectName)) || workflows[0];
      if (pick) void select(pick);
    } catch (e) {
      loadError = typeof e === "string" ? e : String(e);
    }
  }

  async function select(wf: WorkflowCatalogEntry) {
    if (stage === "running") return;
    selected = wf;
    paramValues = Object.fromEntries(wf.params.map((p) => [p.key, ""]));
    stage = "idle";
    lines = [];
    error = null;
    corpusBuilt = null;
    runInfo = null;
    capabilities = [];
    try {
      capabilities = await workflowCapabilities(wf.name);
    } catch {
      capabilities = [];
    }
  }

  async function pickFolder(key: string) {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked === "string") paramValues[key] = picked;
  }

  // The folder param (if any) is the one required input — everything else can
  // default (corpus is auto-derived from the folder; glob/blank means "all").
  let folderKey = $derived(selected?.params.find((p) => p.kind === "folder")?.key ?? null);
  let canRun = $derived(
    stage === "idle" && selected !== null && (folderKey === null || !!paramValues[folderKey]),
  );

  async function run() {
    if (!selected || !canRun) return;
    stage = "running";
    lines = [];
    error = null;
    corpusBuilt = null;
    runInfo = null;
    // Only send filled params (an empty string would override a workflow default).
    const params: Record<string, string> = {};
    for (const [k, v] of Object.entries(paramValues)) if (v) params[k] = v;
    try {
      const handle = await workflowRun(selected.name, params);
      unlisten = await listen<WorkflowRunProgress>(handle.channel, (evt) => {
        const e = evt.payload;
        switch (e.kind) {
          case "run_started":
            runInfo = { items: e.items, steps: e.steps };
            break;
          case "step_done":
            lines = [
              ...lines,
              `${e.cached ? "·" : "✓"} ${e.step} (${e.uses})${e.item !== "·" ? ` — ${e.item}` : ""}`,
            ];
            break;
          case "element_skipped":
            lines = [...lines, `⚠ ${e.step}: skipped item ${e.index} — ${e.error}`];
            break;
          case "complete":
            stage = "done";
            corpusBuilt = e.corpus;
            cleanup();
            break;
          case "failed":
            stage = "failed";
            error = e.error;
            cleanup();
            break;
          // run_finished / item_done carry only aggregates — the line log + the
          // terminal `complete` already cover what the user needs to see.
        }
      });
    } catch (e) {
      stage = "failed";
      error = typeof e === "string" ? e : String(e);
      cleanup();
    }
  }

  function cleanup() {
    unlisten?.();
    unlisten = null;
  }

  function reset() {
    stage = "idle";
    lines = [];
    error = null;
    corpusBuilt = null;
    runInfo = null;
  }

  function paramPlaceholder(kind: string): string {
    switch (kind) {
      case "corpus":
        return "name (defaults to the folder's name)";
      case "glob":
        return "*.pdf,*.md — blank matches every file";
      default:
        return "";
    }
  }
</script>

<div class="run-view" data-testid="workflow-run-view">
  <aside class="picker">
    <h2>Run a workflow</h2>
    <p class="lede">Point a workflow at a folder and watch it work — nothing leaves your machine.</p>
    {#if loadError}
      <p class="error">{loadError}</p>
    {/if}
    <ul class="list">
      {#each workflows as wf}
        <li>
          <button
            class="row"
            class:active={selected?.name === wf.name}
            data-testid="workflow-pick-{wf.name}"
            onclick={() => select(wf)}
          >
            <span class="row-name">{wf.name}</span>
            <span class="row-desc">{wf.description}</span>
          </button>
        </li>
      {/each}
    </ul>
  </aside>

  <section class="detail">
    {#if !selected}
      <p class="muted">Pick a workflow to begin.</p>
    {:else if stage === "idle"}
      <header class="detail-head">
        <h3>{selected.name}</h3>
        <p class="desc">{selected.description}</p>
      </header>

      <div class="form" data-testid="workflow-run-form">
        {#each selected.params as p (p.key)}
          <label class="field">
            <span class="label">{p.label}</span>
            {#if p.kind === "folder"}
              <div class="folder-row">
                <input
                  type="text"
                  bind:value={paramValues[p.key]}
                  placeholder="/path/to/folder"
                  data-testid="workflow-folder-value"
                />
                <button class="ghost" onclick={() => pickFolder(p.key)} data-testid="workflow-pick-folder">
                  Choose folder…
                </button>
              </div>
            {:else}
              <input
                type="text"
                bind:value={paramValues[p.key]}
                placeholder={paramPlaceholder(p.kind)}
                data-testid="workflow-param-{p.key}"
              />
            {/if}
          </label>
        {/each}

        {#if capabilities.length > 0}
          <p class="caps">This workflow can: {capabilities.join(", ")}.</p>
        {/if}

        <button class="primary run" disabled={!canRun} onclick={run} data-testid="workflow-run-button">
          Run
        </button>
      </div>
    {:else}
      <WorkflowRunProgressPanel
        workflowName={selected.name}
        {stage}
        {runInfo}
        {lines}
        {error}
        corpus={corpusBuilt}
        onChat={() => onOpenChat?.()}
        onReset={reset}
      />
    {/if}
  </section>
</div>

<style>
  .run-view {
    display: grid;
    grid-template-columns: 300px 1fr;
    height: 100%;
    overflow: hidden;
  }
  .picker {
    border-right: 1px solid var(--border);
    background: var(--bg-secondary);
    padding: 22px 16px;
    overflow-y: auto;
  }
  .picker h2 {
    margin: 0 0 4px;
    font-size: 1rem;
    color: var(--text-primary);
  }
  .lede {
    margin: 0 0 16px;
    font-size: 0.78rem;
    color: var(--text-muted);
    line-height: 1.5;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .row {
    width: 100%;
    text-align: left;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 9px 11px;
    border-radius: var(--radius);
    border: 1px solid transparent;
    background: transparent;
    cursor: pointer;
  }
  .row:hover {
    background: var(--bg-elevated);
  }
  .row.active {
    background: var(--bg-elevated);
    border-color: color-mix(in oklch, var(--accent) 40%, var(--border));
  }
  .row-name {
    font-weight: 600;
    color: var(--text-primary);
    font-size: 0.88rem;
  }
  .row-desc {
    font-size: 0.72rem;
    color: var(--text-muted);
    line-height: 1.4;
  }
  .detail {
    padding: 28px 32px;
    overflow-y: auto;
  }
  .detail-head h3 {
    margin: 0 0 4px;
    color: var(--text-primary);
  }
  .desc {
    margin: 0 0 20px;
    color: var(--text-muted);
    font-size: 0.85rem;
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: 14px;
    max-width: 520px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .label {
    font-size: 0.78rem;
    font-weight: 600;
    color: var(--text-secondary);
  }
  input {
    font: inherit;
    padding: 8px 10px;
    border-radius: var(--radius);
    border: 1px solid var(--border-mid);
    background: var(--bg-primary, var(--bg-secondary));
    color: var(--text-primary);
  }
  input[readonly] {
    color: var(--text-secondary);
  }
  .folder-row {
    display: flex;
    gap: 8px;
  }
  .folder-row input {
    flex: 1;
  }
  .caps {
    font-size: 0.76rem;
    color: var(--text-muted);
    margin: 2px 0 0;
  }
  button {
    font: inherit;
    cursor: pointer;
    border-radius: var(--radius);
    border: 1px solid var(--border-mid);
    padding: 8px 16px;
  }
  .primary {
    background: var(--accent);
    color: var(--accent-contrast, white);
    border-color: var(--accent);
    font-weight: 600;
  }
  .primary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .run {
    align-self: flex-start;
    margin-top: 4px;
  }
  .ghost {
    background: transparent;
    color: var(--text-secondary);
  }
  .muted {
    color: var(--text-muted);
  }
  .error {
    color: var(--danger, oklch(60% 0.18 25));
    font-size: 0.8rem;
  }
</style>
