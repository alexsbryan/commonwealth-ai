<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  PassageContextChip — small pill above the chat input that shows
  the currently-focused passage (if any). When the user sends a
  message with the chip active, the chunk's text is prepended to
  their message as a labelled context block, scoping the
  librarian's answer to what they have open.

  Persistence: the chip lives until the user explicitly clears it
  (× button) OR the reading surface closes OR the user opens a
  different chunk (which replaces the focus). Auto-clearing after
  one turn would be wrong for the "let's discuss this passage"
  use case the design optimises for.
-->
<script lang="ts">
  import { readingSession } from "../../stores/readingSession.svelte";

  let passage = $derived(readingSession.focusedPassage);

  function handleClear() {
    readingSession.clearFocus();
  }
</script>

{#if passage}
  <div class="passage-chip" role="status">
    <span class="marker" aria-hidden="true">▸</span>
    <span class="label">
      <span class="prefix">context:</span>
      <span class="title" title={passage.title}>{passage.title}</span>
    </span>
    <button
      type="button"
      class="clear"
      onclick={handleClear}
      aria-label="Clear focused passage"
      title="Don't include this passage in the next message"
    >
      ×
    </button>
  </div>
{/if}

<style>
  .passage-chip {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    margin: 6px 12px 0;
    padding: 5px 10px 5px 12px;
    background: color-mix(in srgb, var(--accent, #c9a84c) 10%, transparent);
    border: 1px solid
      color-mix(in srgb, var(--accent, #c9a84c) 35%, transparent);
    border-radius: 999px;
    font-size: 0.78rem;
    color: var(--text-secondary);
    max-width: calc(100% - 24px);
    align-self: flex-start;
    animation: chip-in 200ms ease;
  }

  .marker {
    color: var(--accent, #c9a84c);
    font-size: 0.85rem;
  }

  .label {
    display: inline-flex;
    gap: 6px;
    overflow: hidden;
    white-space: nowrap;
  }

  .prefix {
    color: var(--text-muted);
    font-family: var(--font-mono);
    letter-spacing: 0.04em;
    font-size: 0.72rem;
  }

  .title {
    color: var(--text-primary);
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .clear {
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 0.95rem;
    line-height: 1;
    padding: 0 4px;
    border-radius: 50%;
  }

  .clear:hover {
    color: var(--text-primary);
    background: rgba(255, 255, 255, 0.06);
  }

  @keyframes chip-in {
    0%   { opacity: 0; transform: translateY(4px); }
    100% { opacity: 1; transform: translateY(0); }
  }

  @media (prefers-reduced-motion: reduce) {
    .passage-chip { animation: none; }
  }
</style>
