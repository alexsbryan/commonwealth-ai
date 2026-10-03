<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  ReadingSurface — the middle column of the glass-box reading view.

  Owns the breadcrumb (top, sticky), the chunk renderer (scrollable
  middle), and the outbound-link footer ("Read the full source"
  when present). Mounted by App.svelte when
  `readingSession.currentReading != null`.

  The component itself is a thin shell — actual rendering happens in
  Breadcrumb and ChunkRenderer. Keeping the shell minimal lets the
  atom panel (PR4) slot in as a sibling column without restructuring.
-->
<script lang="ts">
  import { fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { readingSession } from "../../stores/readingSession.svelte";
  import Breadcrumb from "./Breadcrumb.svelte";
  import ChunkRenderer from "./ChunkRenderer.svelte";
  import ConversationChunkRenderer from "./ConversationChunkRenderer.svelte";

  let reading = $derived(readingSession.currentReading);
  let loading = $derived(readingSession.loading);
  let error = $derived(readingSession.error);

  // Pick the renderer based on whether the backend tagged this
  // chunk as a conversation. The presence of a `conversation`
  // payload is the discriminator — `corpus_id == "conversation-history"`
  // would also work but is duplicative with the backend's own check
  // and would diverge if the corpus id ever changes (e.g. multiple
  // conversational corpora per skill).
  let isConversation = $derived(
    reading?.center.conversation != null,
  );

  // Detect reduced-motion preference at mount; transitions zero
  // their duration when the user has asked for less motion.
  let reducedMotion = $state(false);
  $effect(() => {
    if (typeof window === "undefined") return;
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    reducedMotion = mq.matches;
    const handler = (e: MediaQueryListEvent) => (reducedMotion = e.matches);
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  });

  // Esc closes the reading surface entirely (chat returns to full
  // width). Keyboard parity with the breadcrumb's "← back to
  // conversation" affordance — works from anywhere inside the
  // reading column without requiring a mouse trip back to the top.
  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape" && readingSession.isOpen) {
      // If the atom panel is open, close just that first; second Esc
      // closes the whole reading surface. Matches the visual
      // hierarchy — Esc dismisses the topmost overlay layer.
      if (readingSession.isAtomPanelOpen) {
        readingSession.closeAtom();
      } else {
        readingSession.closeReading();
      }
    }
  }
</script>

<svelte:window on:keydown={handleKeydown} />

<aside
  class="reading-surface"
  aria-label="Source reading surface"
  in:fly={{ x: 200, duration: reducedMotion ? 0 : 220, easing: cubicOut }}
  out:fly={{ x: 80, duration: reducedMotion ? 0 : 160, easing: cubicOut }}
>
  <Breadcrumb />

  <div class="content">
    {#if loading && !reading}
      <div class="status">Loading passage…</div>
    {:else if error}
      <div class="status error">
        {error}
      </div>
    {:else if reading}
      {#if isConversation}
        <ConversationChunkRenderer
          prev={reading.prev}
          center={reading.center}
          next={reading.next}
        />
      {:else}
        <ChunkRenderer
          prev={reading.prev}
          center={reading.center}
          next={reading.next}
        />
        {#if reading.outbound_url}
          <footer class="outbound">
            <a
              href={reading.outbound_url}
              target="_blank"
              rel="noopener noreferrer"
            >
              Read the full source ↗
            </a>
          </footer>
        {/if}
      {/if}
    {/if}
  </div>
</aside>

<style>
  .reading-surface {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg-primary);
    border-left: 1px solid var(--border-mid);
    overflow: hidden;
  }

  .content {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    position: relative;
  }

  .status {
    padding: 32px;
    text-align: center;
    color: var(--text-muted);
    font-size: 0.85rem;
  }

  .status.error {
    color: var(--error, #b85450);
  }

  .outbound {
    text-align: center;
    padding: 16px 32px 32px;
    font-size: 0.82rem;
    max-width: 68ch;
    margin: 0 auto;
  }

  .outbound a {
    color: var(--text-muted);
    text-decoration: none;
    border-bottom: 1px dotted var(--border-mid);
    padding-bottom: 1px;
  }

  .outbound a:hover {
    color: var(--accent, #c9a84c);
    border-bottom-color: var(--accent, #c9a84c);
  }
</style>
