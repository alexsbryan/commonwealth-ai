<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Presentational progress panel for a running workflow. The parent
  (WorkflowRunView) owns the invoke + listen and feeds this the live state; this
  just renders "watch it go" — the per-step line log while running, and a
  terminal panel (chat with the built corpus, or the error) when it ends.
-->
<script lang="ts">
  let {
    workflowName,
    stage,
    runInfo,
    lines,
    error,
    corpus,
    onChat,
    onReset,
  }: {
    workflowName: string;
    stage: "running" | "done" | "failed";
    runInfo: { items: number; steps: number } | null;
    lines: string[];
    error: string | null;
    corpus: string | null;
    onChat: () => void;
    onReset: () => void;
  } = $props();
</script>

<div class="run-progress" data-testid="workflow-run-progress" data-stage={stage}>
  <header class="head">
    <div class="title">
      {#if stage === "running"}
        <span class="spinner" aria-hidden="true"></span>
      {/if}
      <span class="name">{workflowName}</span>
    </div>
    {#if runInfo}
      <span class="counts">{runInfo.items} item{runInfo.items === 1 ? "" : "s"} · {runInfo.steps} steps</span>
    {/if}
  </header>

  {#if lines.length > 0}
    <ol class="lines" data-testid="workflow-run-steps">
      {#each lines as line}
        <li>{line}</li>
      {/each}
    </ol>
  {:else if stage === "running"}
    <p class="muted">Starting…</p>
  {/if}

  {#if stage === "done"}
    <div class="terminal ok" data-testid="workflow-run-complete">
      {#if corpus}
        <p>Your notebook <strong>{corpus}</strong> is built and searchable.</p>
        <div class="actions">
          <button class="primary" data-testid="workflow-chat-cta" onclick={onChat}>
            Chat with {corpus}
          </button>
          <button class="ghost" onclick={onReset}>Run another</button>
        </div>
      {:else}
        <p>Done.</p>
        <div class="actions">
          <button class="ghost" onclick={onReset}>Run another</button>
        </div>
      {/if}
    </div>
  {:else if stage === "failed"}
    <div class="terminal err" data-testid="workflow-run-failed">
      <p>{error ?? "The workflow failed."}</p>
      <div class="actions">
        <button class="ghost" onclick={onReset}>Try again</button>
      </div>
    </div>
  {/if}
</div>

<style>
  .run-progress {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .name {
    font-weight: 600;
    color: var(--text-primary);
  }
  .counts {
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  .spinner {
    width: 13px;
    height: 13px;
    border: 2px solid var(--border-mid);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .lines {
    list-style: none;
    margin: 0;
    padding: 10px 12px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    max-height: 260px;
    overflow-y: auto;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.78rem;
    line-height: 1.6;
    color: var(--text-secondary);
  }
  .lines li {
    white-space: pre-wrap;
    word-break: break-word;
  }
  .muted {
    color: var(--text-muted);
    font-size: 0.85rem;
  }
  .terminal {
    padding: 12px 14px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
  }
  .terminal.ok {
    background: color-mix(in oklch, var(--accent) 8%, transparent);
    border-color: color-mix(in oklch, var(--accent) 30%, var(--border));
  }
  .terminal.err {
    background: color-mix(in oklch, var(--danger, oklch(60% 0.18 25)) 8%, transparent);
    border-color: color-mix(in oklch, var(--danger, oklch(60% 0.18 25)) 35%, var(--border));
    color: var(--text-primary);
  }
  .terminal p {
    margin: 0 0 10px;
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  button {
    font: inherit;
    cursor: pointer;
    border-radius: var(--radius);
    padding: 7px 14px;
    border: 1px solid var(--border-mid);
  }
  .primary {
    background: var(--accent);
    color: var(--accent-contrast, white);
    border-color: var(--accent);
    font-weight: 600;
  }
  .ghost {
    background: transparent;
    color: var(--text-secondary);
  }
  .ghost:hover {
    color: var(--text-primary);
  }
</style>
