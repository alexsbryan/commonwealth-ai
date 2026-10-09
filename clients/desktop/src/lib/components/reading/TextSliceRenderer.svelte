<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  TextSliceRenderer — a verified quotation, shown where it stands in its
  stored text (ADDRESSED_TEXT §5.2). The slice comes from the daemon's
  `GET {text_endpoint}/{sha}` read; the quoted words are marked, with the
  text's own context either side. When the words at the address are no
  longer the words the answer quoted, that is said, not smoothed over.
-->
<script lang="ts">
  import { quoteLabel, type TextReading } from "../../stores/readingSession.svelte";

  interface Props {
    reading: TextReading;
  }

  let { reading }: Props = $props();
  let title = $derived(quoteLabel(reading.slice));
</script>

<article class="text-slice" data-testid="text-slice">
  <header>
    <h2>{title}</h2>
    <p class="where">
      {reading.slice.document.source.id} · characters {reading.slice.start}–{reading.slice.end}
    </p>
  </header>
  {#if !reading.matches}
    <p class="mismatch" role="alert" data-testid="text-slice-mismatch">
      The source no longer reads as quoted at this address.
    </p>
  {/if}
  <p class="body">
    {#if reading.slice.start > [...reading.slice.before].length}…{/if}{reading.slice
      .before}<mark>{reading.slice.text}</mark>{reading.slice.after}
  </p>
</article>

<style>
  .text-slice {
    max-width: 68ch;
    margin: 0 auto;
    padding: 24px 32px;
    line-height: 1.6;
  }

  h2 {
    font-size: 1rem;
    margin: 0 0 4px;
  }

  .where {
    color: var(--text-muted);
    font-size: 0.78rem;
    margin: 0 0 16px;
  }

  .mismatch {
    color: var(--error, #b85450);
    font-size: 0.85rem;
  }

  .body {
    white-space: pre-wrap;
  }

  mark {
    background: color-mix(in srgb, var(--accent, #c9a84c) 30%, transparent);
    color: inherit;
    padding: 0 1px;
  }
</style>
