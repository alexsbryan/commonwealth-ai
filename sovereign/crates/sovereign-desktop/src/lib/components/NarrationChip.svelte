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
  import { narrationPhaseTag } from "../types";

  let entries = $derived(routingStore.narrationLog);

  // Icons keyed by the snake_case discriminator. Works for both unit
  // variants (bare strings) and struct variants (one-key objects) via
  // `narrationPhaseTag`. New tool-invocation variants use a magnifying
  // glass + check/cross to read as a clear active→done arc.
  const ICONS: Record<string, string> = {
    routing_committed: "→",
    routing_start: "→",
    routing_complete: "→",
    retrieval_start: "⌕",
    retrieval_complete: "◇",
    curation_start: "✂",
    curation_complete: "✓",
    drafting_start: "✎",
    drafting_complete: "✎",
    primary_synthesis_start: "✎",
    presentation_start: "❧",
    presentation_complete: "❧",
    gap_check_fired: "?",
    tool_invocation_start: "⌕",
    tool_invocation_complete: "✓",
    stage_error: "!",
  };

  function iconFor(phase: NarrationPhase): string {
    return ICONS[narrationPhaseTag(phase)] ?? "·";
  }

  function phaseLabel(phase: NarrationPhase): string {
    return narrationPhaseTag(phase);
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
      {@const isLatest = i === entries.length - 1}
      {@const isGapFired = phaseLabel(entry.phase) === "gap_check_fired"}
      <div
        class="narration-chip"
        class:latest={isLatest}
        class:bridging={isLatest && isGapFired}
        data-phase={phaseLabel(entry.phase)}
        title="Phase: {phaseLabel(entry.phase)}"
        style:--age-step={entries.length - 1 - i}
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

  /* Each chip carries `--age-step` (0 = newest, N = oldest). Older
     chips recede so the most recent reads as "what's happening now"
     instead of "fourth nag in a row." Capped at 3 visible steps of
     decay; the runtime's 3-per-turn cap means we never go deeper. */
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
    /* Linear fade: step 0 → 1.0 alpha, step 1 → 0.65, step 2 → 0.42, step 3+ → 0.3 */
    opacity: max(0.30, calc(1 - var(--age-step, 0) * 0.30));
    transition: opacity 380ms cubic-bezier(0.2, 0.7, 0.2, 1),
                transform 380ms cubic-bezier(0.2, 0.7, 0.2, 1);
    transform-origin: left center;
  }

  /* Latest chip — full presence + slight scale-in entrance the first
     time it renders. Built on a CSS animation rather than a Svelte
     transition so the fade-in fires reliably whether or not the
     stack mounted with prior entries. */
  .narration-chip.latest {
    color: var(--text-secondary);
    border-color: color-mix(in srgb, var(--accent) 25%, var(--border));
    background: color-mix(in srgb, var(--accent) 4%, var(--bg-secondary));
    animation: chip-arrive 280ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }

  /* "Bridging" state — when the latest chip is `gap_check_fired`,
     a card is imminent. The chip grows a downward gold tether so
     the eye links chip → card landing zone instead of treating the
     card as an unrelated object. The tether sits below the chip in
     the same flex column, drawn as a ::after pseudo-element. */
  .narration-chip.bridging {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 55%, transparent);
    border-style: solid;
    background: color-mix(in srgb, var(--accent) 8%, var(--bg-secondary));
    position: relative;
    /* Slow heartbeat — telegraphs "something is about to land" without
       being a spinner. Disabled under prefers-reduced-motion. */
    animation: chip-arrive 280ms cubic-bezier(0.2, 0.7, 0.2, 1),
               bridge-pulse 1.8s ease-in-out 280ms infinite;
  }
  .narration-chip.bridging .phase-icon {
    /* Replace the default `?` glyph visually with a more anticipatory
       mark via CSS — keeps the icon-by-phase map intact for tests. */
    font-size: 0.9rem;
  }
  .narration-chip.bridging::after {
    content: '';
    position: absolute;
    left: 16px;
    top: 100%;
    width: 1px;
    height: 6px;
    background: linear-gradient(
      to bottom,
      color-mix(in srgb, var(--accent) 80%, transparent),
      transparent
    );
    pointer-events: none;
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

  @keyframes chip-arrive {
    from {
      opacity: 0;
      transform: translateY(-2px) scale(0.96);
    }
    to {
      opacity: 1;
      transform: translateY(0) scale(1);
    }
  }
  @keyframes bridge-pulse {
    0%, 100% {
      box-shadow: 0 0 0 0 color-mix(in srgb, var(--accent) 0%, transparent);
    }
    50% {
      box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 12%, transparent);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .narration-chip,
    .narration-chip.latest,
    .narration-chip.bridging {
      animation: none;
      transition: none;
    }
    .narration-chip.bridging::after {
      display: none;
    }
  }
</style>
