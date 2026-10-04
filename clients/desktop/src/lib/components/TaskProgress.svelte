<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { TaskStep } from "../types";

  interface Props {
    steps: TaskStep[];
  }

  let { steps }: Props = $props();

  function statusIcon(status: TaskStep["status"]): string {
    switch (status) {
      case "done":
        return "\u2713";
      case "skipped":
        return "\u2013";
      case "running":
        return "\u25CB";
      default:
        return "\u00B7";
    }
  }
</script>

{#if steps.length > 0}
  <div class="task-progress">
    <div class="task-header">Task Progress</div>
    <div class="steps">
      {#each steps as step (step.id)}
        <div class="step" class:done={step.status === "done"} class:skipped={step.status === "skipped"}>
          <span class="step-icon">{statusIcon(step.status)}</span>
          <span class="step-desc">{step.description}</span>
        </div>
      {/each}
    </div>
  </div>
{/if}

<style>
  .task-progress {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 12px;
    margin-bottom: 12px;
  }

  .task-header {
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    margin-bottom: 8px;
  }

  .steps {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .step {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.9rem;
    color: var(--text-secondary);
  }

  .step.done {
    color: var(--success);
  }

  .step.skipped {
    color: var(--text-muted);
    text-decoration: line-through;
  }

  .step-icon {
    width: 16px;
    text-align: center;
    font-weight: bold;
  }
</style>
