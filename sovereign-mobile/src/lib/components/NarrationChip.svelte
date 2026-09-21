<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Live "glassbox" progress for an in-flight turn — the mobile port of
  // the desktop NarrationChip stack. Each entry is a phase the host's
  // runtime narrated as it did the real work (routing → retrieval →
  // synthesis → gap check / tool calls). We render the runtime's own
  // text, iconned by phase, newest at the bottom (where the answer
  // forms), older entries dimmed. This replaces the silent "wait … wait"
  // with a trace of what the host is actually doing.
  import type { NarrationEntry } from "../events";

  let { entries }: { entries: NarrationEntry[] } = $props();

  // NarrationPhase is either a snake_case string ("retrieval_start") or a
  // single-key object ({ retrieval_complete: {...} }) — read the key.
  function phaseKey(p: NarrationEntry["phase"]): string {
    return typeof p === "string" ? p : (Object.keys(p ?? {})[0] ?? "");
  }

  function icon(key: string): string {
    if (key.endsWith("complete")) return "✓";
    if (key === "gap_check_fired") return "✦";
    if (key.startsWith("tool_invocation")) return "⚙";
    return "◈";
  }

  function label(e: NarrationEntry): string {
    const t = e.text?.trim();
    if (t) return t;
    // Fallback: humanize the phase key when the runtime sent no text.
    return phaseKey(e.phase).replace(/_/g, " ");
  }

  // The runtime caps a turn's narration; show the last few, latest last.
  const visible = $derived(entries.slice(-4));
</script>

<div class="narration" aria-live="polite">
  {#each visible as e, i (entries.length - visible.length + i)}
    {@const last = i === visible.length - 1}
    <div class="chip" class:latest={last}>
      <span class="ico" aria-hidden="true">{icon(phaseKey(e.phase))}</span>
      <span class="text">{label(e)}</span>
      {#if last && e.elapsed_ms > 0}
        <span class="ms">{(e.elapsed_ms / 1000).toFixed(1)}s</span>
      {/if}
    </div>
  {/each}
</div>

<style>
  .narration {
    align-self: flex-start;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    max-width: 94%;
    min-width: 0;
  }
  .chip {
    display: inline-flex;
    align-items: baseline;
    gap: 0.4rem;
    font-family: var(--font-sans);
    font-size: 0.76rem;
    font-style: italic;
    line-height: 1.35;
    color: var(--text-muted);
    border: 1px dashed var(--border-bright);
    border-radius: var(--radius);
    padding: 0.28rem 0.6rem;
    background: color-mix(in srgb, var(--lavender) 5%, transparent);
    /* Older entries recede; the latest is the live one. */
    opacity: 0.5;
    transition: opacity 0.25s, color 0.25s, border-color 0.25s;
    /* Each new phase rises gently into view as the host makes progress —
       runs once on creation. Stable keys mean only NEW chips animate. */
    animation: chipRise 0.3s ease-out;
  }
  .chip.latest {
    opacity: 1;
    color: var(--text-secondary);
    border-color: color-mix(in srgb, var(--lavender) 40%, transparent);
    /* A slow lavender sheen sweeps across the live chip — a gentle, continuous
       "still working" pulse. It's a background layer (under the text), so the
       label stays crisp; the base tint from `.chip` shows through. */
    background-image: linear-gradient(
      100deg,
      transparent 28%,
      color-mix(in srgb, var(--lavender) 18%, transparent) 50%,
      transparent 72%
    );
    background-size: 220% 100%;
    background-repeat: no-repeat;
    animation: sheen 2.4s ease-in-out infinite;
  }
  .ico {
    font-style: normal;
    font-size: 0.72em;
    color: var(--lavender-light);
    flex: none;
  }
  /* The live chip's icon breathes to signal work in progress. */
  .chip.latest .ico {
    animation: breathe 1.6s ease-in-out infinite;
    text-shadow: 0 0 10px var(--lavender-glow);
  }
  .text {
    overflow-wrap: anywhere;
  }
  .ms {
    font-style: normal;
    font-size: 0.92em;
    color: var(--text-muted);
    margin-left: 0.1rem;
    flex: none;
  }
  @keyframes breathe {
    0%, 100% { opacity: 0.5; }
    50%      { opacity: 1; }
  }
  /* New phase rises in (settles to its declared opacity — no fill). */
  @keyframes chipRise {
    from { opacity: 0; transform: translateY(6px); }
    to   { opacity: 1; transform: none; }
  }
  /* Lavender sheen sweeping left→right with a brief rest between sweeps. */
  @keyframes sheen {
    0%        { background-position: 165% 0; }
    55%, 100% { background-position: -65% 0; }
  }
  @media (prefers-reduced-motion: reduce) {
    .chip { animation: none; }
    .chip.latest { animation: none; background-image: none; }
    .chip.latest .ico { animation: none; }
  }
</style>
