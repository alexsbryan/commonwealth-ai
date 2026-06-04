<script lang="ts">
  // Glass-box reader — the mobile equivalent of the desktop reading
  // column. Tapping a citation opens the cited passage (highlighted) with
  // a window of surrounding chunks for context, served full-text from the
  // host's corpus engine. Slides up over the chat; tap the scrim or ✕ to
  // dismiss.
  import { onMount } from "svelte";
  import { fly, fade } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { readCitation, type ReadingWindow } from "../api";

  let {
    corpusId,
    chunkId,
    title,
    isPrivate = false,
    onclose,
  }: {
    corpusId: string;
    chunkId: string;
    title: string;
    isPrivate?: boolean;
    onclose: () => void;
  } = $props();

  let win = $state<ReadingWindow | null>(null);
  let loading = $state(true);
  let failed = $state(false);

  onMount(async () => {
    try {
      win = await readCitation(corpusId, chunkId);
      if (!win?.center) failed = true;
    } catch {
      failed = true;
    } finally {
      loading = false;
    }
  });

  const url = $derived(win?.center?.url ?? null);
</script>

<div
  class="scrim"
  onclick={onclose}
  transition:fade={{ duration: 140 }}
  role="presentation"
></div>

<section class="reader" transition:fly={{ y: 48, duration: 240, easing: cubicOut }}>
  <header>
    <div class="crumb">
      {#if isPrivate}<span class="lock" title="Private to this host — never shared with mesh peers">🔒</span>{/if}
      <span class="corpus">{corpusId}</span>
      <span class="sep">›</span>
      <span class="title">{title}</span>
    </div>
    <button class="close" onclick={onclose} aria-label="Close reader">✕</button>
  </header>

  <div class="body">
    {#if loading}
      <div class="loading"><span class="crest">◈</span></div>
    {:else if failed || !win?.center}
      <p class="empty">Couldn't load this passage from the host.</p>
    {:else}
      {#each win.prev as c (c.chunk_id)}
        <p class="ctx">{c.content}</p>
      {/each}
      <div class="center">{win.center.content}</div>
      {#each win.next as c (c.chunk_id)}
        <p class="ctx">{c.content}</p>
      {/each}
    {/if}
  </div>

  {#if url}
    <footer>
      <a href={url} target="_blank" rel="noopener noreferrer">Read the full source ↗</a>
    </footer>
  {/if}
</section>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(8, 5, 14, 0.66);
    backdrop-filter: blur(2px);
    z-index: 200;
    border: none;
  }
  .reader {
    position: fixed;
    /* Leave a thumb's-worth of the chat visible up top, clearing the
       status bar / Dynamic Island on whatever device this is. */
    inset: calc(env(safe-area-inset-top) + 1.2rem) 0 0 0;
    z-index: 201;
    display: flex;
    flex-direction: column;
    background: var(--bg-primary);
    border-top: 1px solid var(--border-bright);
    border-radius: var(--radius-xl) var(--radius-xl) 0 0;
    box-shadow: 0 -18px 54px rgba(0, 0, 0, 0.5);
    overflow: hidden;
  }
  header {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.85rem var(--pad-r) 0.85rem var(--pad-l);
    border-bottom: 1px solid var(--border);
    background: var(--bg-secondary);
  }
  .crumb {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.35rem;
    font-family: var(--font-sans);
    font-size: 0.78rem;
    min-width: 0;
  }
  .lock { font-size: 0.82em; }
  .corpus {
    color: var(--lavender-light);
    font-weight: 500;
    white-space: nowrap;
  }
  .sep { color: var(--text-muted); }
  .title {
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .close {
    color: var(--text-muted);
    font-size: 0.95rem;
    padding: 0.35rem 0.55rem;
    border-radius: var(--radius);
  }
  .close:active {
    color: var(--text-primary);
    background: var(--bg-surface);
  }
  .body {
    flex: 1;
    overflow-y: auto;
    /* Full-width scroll area; the prose itself caps at the reading
       measure and centers, so lines never run too long on a tablet. */
    width: 100%;
    max-width: var(--measure);
    margin-inline: auto;
    padding: 1.4rem var(--pad-r) calc(1.6rem + env(safe-area-inset-bottom)) var(--pad-l);
    font-family: var(--font-serif);
    font-variation-settings: "opsz" 14;
    font-weight: 380;
    font-feature-settings: "kern", "liga", "calt";
    font-size: 16px;
    line-height: 1.78;
    color: var(--text-primary);
    overflow-wrap: break-word;
  }
  .ctx {
    color: var(--text-muted);
    margin-bottom: 0.9rem;
    white-space: pre-wrap;
  }
  /* The cited passage — gently lit, the lavender margin marks it as the
     reason this reader opened. */
  .center {
    color: var(--text-primary);
    background: var(--lavender-glow);
    border-left: 2px solid var(--lavender);
    padding: 0.6rem 0.9rem;
    margin: 0.3rem -0.35rem 0.9rem;
    border-radius: 0 var(--radius) var(--radius) 0;
    white-space: pre-wrap;
  }
  .loading {
    display: flex;
    justify-content: center;
    padding: 3rem 0;
  }
  .crest {
    font-size: 1.8rem;
    color: var(--lavender);
    text-shadow: 0 0 18px var(--lavender-glow);
    animation: breathe 1.6s ease-in-out infinite;
  }
  @keyframes breathe {
    0%, 100% { opacity: 0.32; transform: scale(0.96); }
    50%      { opacity: 0.72; transform: scale(1.04); }
  }
  .empty {
    color: var(--text-muted);
    text-align: center;
    padding: 2.5rem 0;
  }
  footer {
    padding: 0.8rem var(--pad-r) calc(0.8rem + env(safe-area-inset-bottom)) var(--pad-l);
    border-top: 1px solid var(--border);
    background: var(--bg-secondary);
  }
  footer a {
    font-family: var(--font-sans);
    font-size: 0.82rem;
    font-weight: 500;
    color: var(--accent-light);
    text-decoration: none;
  }
</style>
