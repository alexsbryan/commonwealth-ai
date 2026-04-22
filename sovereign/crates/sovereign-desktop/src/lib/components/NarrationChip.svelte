<!--
  NarrationChip — renders the running `routingStore.narrationLog`
  as a vertical stack of model-voice chips. Each entry marks a
  substantive phase boundary during a long turn (retrieval done,
  primary-synthesis beginning, gap-check fired).

  Suppression lives in the runtime (`QuerySession.try_emit_narration`
  drops events below 5s elapsed and caps at 3 per turn). The UI is
  just a passive renderer reading from the FSM store.

  Reset: `chat.machine.ts` dispatches `CLEAR_NARRATION` on new user
  turn so a fresh turn starts with an empty log.
-->
<script lang="ts">
  import { routingStore } from "../stores/routing.svelte";
  import type { NarrationPhase } from "../types";

  let entries = $derived(routingStore.narrationLog);

  function iconFor(phase: NarrationPhase): string {
    switch (phase) {
      case "routing_committed":
        return "→";
      case "retrieval_complete":
        return "◇";
      case "primary_synthesis_start":
        return "✎";
      case "gap_check_fired":
        return "?";
      default:
        return "·";
    }
  }

  function formatElapsed(ms: number): string {
    if (ms < 1000) return `${ms}ms`;
    const s = (ms / 1000).toFixed(1);
    return `${s}s`;
  }
</script>

{#if entries.length > 0}
  <div class="narration-stack" data-testid="narration-stack">
    {#each entries as entry, i (entry.elapsed_ms + "-" + i)}
      <div
        class="narration-chip"
        data-phase={entry.phase}
        title="Phase: {entry.phase}"
      >
        <span class="phase-icon" aria-hidden="true">{iconFor(entry.phase)}</span>
        <span class="narration-text">{entry.text}</span>
        <span class="elapsed">{formatElapsed(entry.elapsed_ms)}</span>
      </div>
    {/each}
  </div>
{/if}

<style>
  .narration-stack {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-bottom: 6px;
  }

  .narration-chip {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 4px 10px;
    font-size: 0.78rem;
    color: var(--text-muted);
    background: var(--bg-secondary);
    border: 1px dashed var(--border);
    border-radius: 12px;
    align-self: flex-start;
    max-width: 100%;
    font-style: italic;
  }

  .phase-icon {
    color: var(--accent);
    font-style: normal;
    font-weight: 600;
    font-size: 0.85rem;
    line-height: 1;
  }

  .narration-text {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .elapsed {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.7rem;
    color: var(--text-muted);
    opacity: 0.7;
    font-style: normal;
  }
</style>
