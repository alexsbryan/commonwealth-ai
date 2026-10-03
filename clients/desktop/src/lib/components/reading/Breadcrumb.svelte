<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Breadcrumb — the trail-of-inquiry indicator.

  Renders the steps that brought the user to the current reading
  position. Leftmost item is "← back to conversation" — click it to
  collapse the reading surface. Middle items click to truncate the
  trail to that step (PR4 wires the jump-back action; v1 truncates
  the trail array which the user sees as the indicator dimming).

  Quiet by design: meant to read past, not to dominate. Mirrors the
  glass-box-reading-surface plan's "trace of inquiry — visible to
  you, not loud, fading slightly if it gets long."
-->
<script lang="ts">
  import { readingSession, type BreadcrumbStep } from "../../stores/readingSession.svelte";

  let trail = $derived(readingSession.trail);

  function handleStepClick(idx: number) {
    if (idx === -1) {
      readingSession.closeReading();
      return;
    }
    readingSession.truncateTrailTo(idx);
  }

  function iconFor(kind: BreadcrumbStep["kind"]): string {
    switch (kind) {
      case "question": return "❓";
      case "chunk": return "▶";
      case "atom-jump": return "↳";
    }
  }
</script>

<nav class="breadcrumb" aria-label="Reading trail">
  <button
    type="button"
    class="back"
    onclick={() => handleStepClick(-1)}
    title="Close the reading surface and return to chat"
  >
    ← back to conversation
  </button>
  {#each trail as step, idx (idx)}
    <span class="separator" aria-hidden="true">›</span>
    <button
      type="button"
      class="step"
      class:current={idx === trail.length - 1}
      onclick={() => handleStepClick(idx)}
      title={step.label}
    >
      <span class="kind-icon" aria-hidden="true">{iconFor(step.kind)}</span>
      <span class="label">{step.label}</span>
    </button>
  {/each}
</nav>

<style>
  .breadcrumb {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 10px 16px;
    font-size: 0.78rem;
    color: var(--text-muted);
    border-bottom: 1px solid var(--border-mid);
    background: var(--bg-secondary);
    overflow: hidden;
    flex-wrap: nowrap;
    white-space: nowrap;
  }

  .back, .step {
    background: none;
    border: none;
    color: inherit;
    cursor: pointer;
    padding: 2px 4px;
    font: inherit;
    border-radius: 4px;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 240px;
    overflow: hidden;
  }

  .back {
    color: var(--text-secondary);
    opacity: 0.85;
  }

  .back:hover, .step:hover {
    background: var(--bg-elevated, rgba(255, 255, 255, 0.05));
    color: var(--text-primary);
  }

  .step.current {
    color: var(--text-primary);
    font-weight: 500;
  }

  .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .kind-icon {
    font-size: 0.7rem;
    opacity: 0.7;
  }

  .separator {
    opacity: 0.4;
    font-weight: 300;
  }
</style>
