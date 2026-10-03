<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { dialogFocus } from "@sovereign/chat-ui";

  interface Props {
    /// The paragraph the user wrote earlier that resonated with the
    /// just-committed paragraph.
    fragment: string;
    /// A neutral date label — "earlier today", "yesterday", or a
    /// formatted date like "March 14". Never analytical, never
    /// numeric ("0.74 similarity"). The point is to feel like a
    /// thoughtful reader's pencil mark, not a search result.
    dateLabel: string;
    /// Closes the overlay. Wired to the click-outside, Esc, and the
    /// small dismiss control.
    onClose: () => void;
  }

  let { fragment, dateLabel, onClose }: Props = $props();

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    }
  }

  // Trap Escape at the window level so the surface's own Esc handler
  // (which would otherwise try to cancel a witness) doesn't fire when
  // the overlay is the user's intent. We attach it on mount and detach
  // on unmount so the overlay is the only Esc consumer while it's up.
  onMount(() => {
    window.addEventListener("keydown", handleKeydown, true);
  });

  onDestroy(() => {
    window.removeEventListener("keydown", handleKeydown, true);
  });

  function handleBackdropClick(e: MouseEvent) {
    // Click outside the card closes; clicks inside don't bubble.
    if (e.target === e.currentTarget) {
      onClose();
    }
  }
</script>

<!-- dialogFocus is used for focus-in + focus-restore ONLY (no onEscape):
     Escape is owned by the window-capture handler above, which
     deliberately wins over the surface's own Esc handler. -->
<div
  class="backdrop"
  role="dialog"
  aria-modal="true"
  aria-label="Echo from earlier writing"
  onclick={handleBackdropClick}
  onkeydown={handleKeydown}
  tabindex="-1"
  use:dialogFocus
>
  <article class="card">
    <header class="meta">{dateLabel}</header>
    <p class="fragment">{fragment}</p>
  </article>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 10;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 4vh 6vw;
    /* The brief: backdrop blur preserves the user's place visually
       while pulling focus to the content. The saturate boost is the
       Apple-style trick that keeps the blurred field feeling alive
       rather than dishwater-grey. */
    backdrop-filter: blur(12px) saturate(1.2);
    -webkit-backdrop-filter: blur(12px) saturate(1.2);
    background: oklch(50% 0.008 250 / 0.18);
    cursor: pointer;
    animation: backdrop-fade 220ms ease-out;
  }

  @keyframes backdrop-fade {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }

  .card {
    max-width: 52ch;
    width: 100%;
    padding: 2.4em 2.4em 2.6em;
    /* No border, no shadow card. The blurred field behind already
       gives separation; an additional shadow would read as a popup
       rather than a pencil mark. The slight color-mix lifts the
       card from the field just enough to be readable. */
    background: color-mix(in oklch, var(--inner-bg-cool) 85%, white);
    border-radius: 4px;
    cursor: default;
    animation: card-arrive 280ms ease-out;
    color: var(--inner-ink);
    font: inherit;
    line-height: 1.7;
  }

  @media (prefers-color-scheme: dark) {
    .card {
      background: color-mix(in oklch, var(--inner-bg-cool) 85%, black);
    }
  }

  @keyframes card-arrive {
    from {
      opacity: 0;
      transform: translateY(4px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .meta {
    color: var(--inner-ink-muted);
    font-size: 0.85em;
    letter-spacing: 0.02em;
    margin-bottom: 1em;
  }

  .fragment {
    margin: 0;
    white-space: pre-wrap;
  }

  @media (prefers-reduced-motion: reduce) {
    .backdrop,
    .card {
      animation: none;
    }
  }
</style>
