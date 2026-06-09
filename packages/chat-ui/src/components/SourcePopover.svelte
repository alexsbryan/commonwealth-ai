<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  interface ChunkInfo {
    title: string;
    corpus_id: string;
    url?: string;
    snippet: string;
  }

  interface Props {
    chunk: ChunkInfo;
    anchor: { x: number; y: number };
    onclose: () => void;
  }

  let { chunk, anchor, onclose }: Props = $props();

  // Position the popover below and slightly left of the click point.
  // Clamp to viewport bounds.
  let style = $derived(() => {
    const maxX = window.innerWidth - 360;
    const maxY = window.innerHeight - 250;
    const x = Math.min(Math.max(8, anchor.x - 40), maxX);
    const y = Math.min(anchor.y + 12, maxY);
    return `left: ${x}px; top: ${y}px;`;
  });

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }

  function handleBackdropClick(e: MouseEvent) {
    // Close if clicking the backdrop (not the popover itself).
    if ((e.target as HTMLElement).classList.contains("popover-backdrop")) {
      onclose();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="popover-backdrop" onclick={handleBackdropClick}>
  <div class="source-popover" style={style()}>
    <div class="popover-header">
      <span class="corpus-badge">{chunk.corpus_id}</span>
      <span class="popover-title">{chunk.title || "Retrieved passage"}</span>
      <button class="popover-close" onclick={onclose}>&times;</button>
    </div>

    <div class="popover-snippet">{chunk.snippet}</div>

    {#if chunk.url}
      <a
        class="popover-link"
        href={chunk.url}
        target="_blank"
        rel="noopener noreferrer"
      >
        View source &rarr;
      </a>
    {/if}
  </div>
</div>

<style>
  .popover-backdrop {
    position: fixed;
    inset: 0;
    z-index: 1000;
  }

  .source-popover {
    position: fixed;
    z-index: 1001;
    width: 340px;
    max-height: 240px;
    background: var(--bg-elevated);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius-lg);
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4), 0 0 1px rgba(155, 135, 196, 0.2);
    overflow: hidden;
    display: flex;
    flex-direction: column;
    animation: popover-in 0.15s ease-out;
  }

  @keyframes popover-in {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .popover-header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px 8px;
    border-bottom: 0.5px solid var(--border);
  }

  .corpus-badge {
    font-size: 0.65rem;
    font-family: var(--font-mono);
    padding: 1px 6px;
    border-radius: 3px;
    background: var(--lavender-dim);
    color: var(--lavender-light);
    white-space: nowrap;
    flex-shrink: 0;
  }

  .popover-title {
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--text-primary);
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-sans);
  }

  .popover-close {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 1.1rem;
    cursor: pointer;
    padding: 0 2px;
    line-height: 1;
    flex-shrink: 0;
  }
  .popover-close:hover {
    color: var(--text-primary);
  }

  .popover-snippet {
    padding: 10px 12px;
    font-size: 0.78rem;
    font-family: var(--font-serif);
    color: var(--text-secondary);
    line-height: 1.6;
    overflow-y: auto;
    flex: 1;
  }

  .popover-link {
    display: block;
    padding: 8px 12px;
    font-size: 0.72rem;
    font-family: var(--font-sans);
    color: var(--lavender-light);
    text-decoration: none;
    border-top: 0.5px solid var(--border);
  }
  .popover-link:hover {
    color: var(--accent-light);
    background: var(--accent-glow);
  }
</style>
