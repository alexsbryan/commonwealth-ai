<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import PositionBadge from "./PositionBadge.svelte";
  import type { PositionStyle } from "../types";

  interface Props {
    text: string;
    index: number;
    position?: { name: string; style: PositionStyle };
    alreadyClipped: boolean;
    onclip: (detail: {
      text: string;
      paragraphIndex: number;
      position?: { name: string; style: PositionStyle };
    }) => void;
  }

  let { text, index, position, alreadyClipped, onclip }: Props = $props();

  let hovered = $state(false);
  let clipping = $state(false);
  let clipped = $derived(alreadyClipped || clipping);

  function handleClip() {
    if (alreadyClipped || clipping) return;
    clipping = true;

    onclip({
      text,
      paragraphIndex: index,
      position,
    });

    // Amber flash settles after 600ms.
    setTimeout(() => {
      clipping = false;
    }, 600);
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="sv-para-wrap"
  class:clipping
  class:clipped={alreadyClipped}
  onmouseenter={() => (hovered = true)}
  onmouseleave={() => (hovered = false)}
>
  {#if position}
    <PositionBadge name={position.name} style={position.style} />
  {/if}

  <p class="sv-para-text">{text}</p>

  {#if hovered && !alreadyClipped}
    <button class="sv-clip-btn" onclick={handleClip}>&#x25C8; Clip</button>
  {/if}

  {#if alreadyClipped}
    <span class="sv-clip-mark" class:top-offset={!!position}>&#x25C8;</span>
  {/if}
</div>

<style>
  .sv-para-wrap {
    position: relative;
    margin-bottom: 12px;
    padding-right: 38px;
  }

  .sv-para-text {
    font-family: var(--font-serif);
    font-size: 15px;
    line-height: 1.8;
    color: var(--text-primary);
    margin: 0;
    border-radius: 2px;
    transition: background 0.6s;
  }

  .clipping .sv-para-text {
    background: var(--amber-flash);
    transition: background 0s;
  }

  .clipped .sv-para-text {
    background: var(--amber-settled);
  }

  .sv-clip-btn {
    position: absolute;
    right: 0;
    top: 4px;
    background: var(--bg-primary);
    border: 0.5px solid var(--border-mid);
    border-radius: var(--radius);
    padding: 3px 8px;
    cursor: pointer;
    font-size: 11px;
    color: var(--text-secondary);
    font-family: var(--font-sans);
    white-space: nowrap;
    transition: border-color 0.15s, color 0.15s;
  }

  .sv-clip-btn:hover {
    border-color: var(--amber);
    color: var(--amber);
  }

  .sv-clip-mark {
    position: absolute;
    right: 4px;
    top: 4px;
    font-size: 13px;
    color: var(--amber);
  }

  .sv-clip-mark.top-offset {
    top: 24px;
  }
</style>
