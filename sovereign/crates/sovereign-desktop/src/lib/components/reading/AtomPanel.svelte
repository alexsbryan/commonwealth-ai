<!--
  AtomPanel — the index card for a clicked atom.

  Shape per the glass-box reading-surface plan: atom_type pill +
  canonical name + brief description, the relations + claims it
  appears in (one-hop edges), and a "where else this appears"
  section grouping same-corpus sections and cross-corpus bridges.

  Discipline: not a knowledge-graph visualization. The card is a
  flat structured panel a reader can scan. Force-directed layouts
  belong in a different product.
-->
<script lang="ts">
  import { fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { readingSession } from "../../stores/readingSession.svelte";
  import { atlasNavigation } from "../../stores/atlasNavigation.svelte";

  let panel = $derived(readingSession.atomPanel);

  let reducedMotion = $state(false);
  $effect(() => {
    if (typeof window === "undefined") return;
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    reducedMotion = mq.matches;
    const handler = (e: MediaQueryListEvent) => (reducedMotion = e.matches);
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  });

  function handleClose() {
    readingSession.closeAtom();
  }

  function handleOpenInAtlas() {
    // Bridges chat-view (where AtomPanel lives) to atlas-view via a
    // small store: App.svelte observes the pending request, flips
    // the rail, and AtlasSurface picks it up on mount.
    if (!panel?.card) return;
    atlasNavigation.requestAtom(panel.card.corpus_id, panel.card.atom_id);
  }

  function handleJump(
    corpusId: string,
    chunkId: number,
    label: string,
  ) {
    void readingSession.jumpToChunk(corpusId, chunkId, `via ${label}`);
  }
</script>

{#if panel}
  <aside
    class="atom-panel"
    aria-label="Atom card"
    in:fly={{ x: 120, duration: reducedMotion ? 0 : 160, easing: cubicOut }}
    out:fly={{ x: 60, duration: reducedMotion ? 0 : 120, easing: cubicOut }}
  >
    <header class="header">
      {#if panel.card}
        <span class="type-pill type-{panel.card.atom_type}">{panel.card.atom_type}</span>
        <h2 class="title">{panel.card.canonical_name}</h2>
        {#if panel.card.salience != null}
          <span class="salience" title="Atlas salience score (0–1)">
            ◌ {panel.card.salience.toFixed(2)}
          </span>
        {/if}
      {:else}
        <h2 class="title placeholder">Loading atom…</h2>
      {/if}
      {#if panel.card}
        <button
          type="button"
          class="open-in-atlas"
          onclick={handleOpenInAtlas}
          aria-label="Open this atom in the Atlas inspector"
          title="Open in Atlas inspector"
        >
          <!-- Lucide: external-link -->
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M15 3h6v6"/>
            <path d="M10 14 21 3"/>
            <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>
          </svg>
          <span>Atlas</span>
        </button>
      {/if}
      <button
        type="button"
        class="close"
        onclick={handleClose}
        aria-label="Close atom panel"
        title="Close"
      >
        ×
      </button>
    </header>

    <div class="content">
      {#if panel.loading && !panel.card}
        <div class="status">Loading…</div>
      {:else if panel.error}
        <div class="status error">{panel.error}</div>
      {:else if panel.card}
        <p class="description">{panel.card.description}</p>

        {#if panel.card.aliases.length > 0}
          <section class="section">
            <h3>Also known as</h3>
            <p class="aliases">
              {#each panel.card.aliases as alias, i}
                <span class="alias">{alias}</span>{#if i < panel.card.aliases.length - 1},
                {/if}
              {/each}
            </p>
          </section>
        {/if}

        {#if panel.card.related.length > 0}
          <section class="section">
            <h3>Appears in</h3>
            <ul class="relations">
              {#each panel.card.related as rel (rel.atom_id)}
                <li class="relation">
                  <span class="rel-edge">{rel.edge_type}</span>
                  <span class="rel-arrow">→</span>
                  <span class="rel-name">{rel.canonical_name}</span>
                  <span class="rel-type">({rel.atom_type})</span>
                </li>
              {/each}
            </ul>
          </section>
        {/if}

        {#if panel.elsewhere && panel.elsewhere.same_corpus.length > 0}
          <section class="section">
            <h3>Where else in this corpus</h3>
            <ul class="elsewhere">
              {#each panel.elsewhere.same_corpus as ref (ref.section_id)}
                <li class="elsewhere-row">
                  {#if ref.chunk_id != null}
                    <button
                      type="button"
                      class="jump"
                      onclick={() =>
                        handleJump(
                          panel.corpusId,
                          ref.chunk_id!,
                          panel.card?.canonical_name ?? "atom",
                        )}
                      title="Jump to this section"
                    >
                      <span class="section-id">{ref.section_id}</span>
                      {#if ref.preview}
                        <span class="preview">{ref.preview}</span>
                      {/if}
                      <span class="jump-arrow" aria-hidden="true">↗</span>
                    </button>
                  {:else}
                    <span class="jump disabled" title="Section not resolvable to a chunk in this index">
                      <span class="section-id">{ref.section_id}</span>
                      {#if ref.preview}
                        <span class="preview">{ref.preview}</span>
                      {/if}
                    </span>
                  {/if}
                </li>
              {/each}
            </ul>
          </section>
        {/if}

        {#if panel.elsewhere && panel.elsewhere.cross_corpus.length > 0}
          <section class="section">
            <h3>Linked across corpora</h3>
            <ul class="cross-corpus">
              {#each panel.elsewhere.cross_corpus as link (link.peer_corpus_id + link.peer_atom_id)}
                <li class="cross-row">
                  <span class="peer-corpus">{link.peer_corpus_id}</span>
                  <span class="rel-arrow">·</span>
                  <span class="peer-name">{link.peer_canonical_name}</span>
                  <span class="signal" title="{link.edge_type} via {link.signal} ({(link.confidence * 100).toFixed(0)}% conf)">
                    {link.edge_type}
                  </span>
                </li>
              {/each}
            </ul>
          </section>
        {/if}
      {/if}
    </div>
  </aside>
{/if}

<style>
  .atom-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg-elevated, var(--bg-surface, #1a1a1a));
    border-left: 1px solid var(--border-mid);
    box-shadow: -4px 0 12px rgba(0, 0, 0, 0.16);
    overflow: hidden;
  }

  .header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 14px 16px 12px;
    border-bottom: 1px solid var(--border-mid);
    background: var(--bg-secondary);
    flex-wrap: wrap;
  }

  .type-pill {
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    padding: 2px 8px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent, #c9a84c) 18%, transparent);
    color: var(--accent, #c9a84c);
    font-weight: 600;
  }

  .type-state {
    background: color-mix(in srgb, var(--lavender, #9b87c4) 18%, transparent);
    color: var(--lavender, #9b87c4);
  }

  .title {
    flex: 1;
    margin: 0;
    font-size: 1.05rem;
    font-weight: 600;
    color: var(--text-primary);
    line-height: 1.3;
  }

  .title.placeholder {
    color: var(--text-muted);
    font-weight: 400;
  }

  .salience {
    font-family: var(--font-mono);
    font-size: 0.7rem;
    color: var(--text-muted);
    letter-spacing: 0.04em;
  }

  .close {
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 1.2rem;
    line-height: 1;
    padding: 2px 6px;
    border-radius: 4px;
  }

  .close:hover {
    background: var(--bg-surface);
    color: var(--text-primary);
  }

  .open-in-atlas {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 3px 8px;
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-muted);
    cursor: pointer;
    font: inherit;
    font-size: 0.7rem;
    letter-spacing: 0.02em;
    border-radius: 4px;
    transition: background 150ms ease, color 150ms ease, border-color 150ms ease;
  }

  .open-in-atlas:hover {
    background: var(--bg-surface);
    border-color: var(--border-mid, var(--border));
    color: var(--text-primary);
  }

  .content {
    flex: 1;
    overflow-y: auto;
    padding: 16px;
  }

  .status {
    text-align: center;
    color: var(--text-muted);
    font-size: 0.85rem;
    padding: 24px 0;
  }

  .status.error {
    color: var(--error, #b85450);
  }

  .description {
    margin: 0 0 18px 0;
    font-size: 0.88rem;
    color: var(--text-secondary);
    line-height: 1.55;
  }

  .section {
    margin-bottom: 18px;
  }

  .section h3 {
    margin: 0 0 6px 0;
    font-size: 0.66rem;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    color: var(--text-muted);
    font-weight: 600;
  }

  .aliases {
    margin: 0;
    font-size: 0.82rem;
    color: var(--text-secondary);
  }

  .alias {
    color: var(--text-primary);
  }

  .relations, .elsewhere, .cross-corpus {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .relation {
    font-size: 0.82rem;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px;
    color: var(--text-secondary);
  }

  .rel-edge {
    color: var(--accent, #c9a84c);
    font-family: var(--font-mono);
    font-size: 0.74rem;
  }

  .rel-arrow {
    color: var(--text-muted);
  }

  .rel-name {
    color: var(--text-primary);
    font-weight: 500;
  }

  .rel-type {
    color: var(--text-muted);
    font-size: 0.74rem;
  }

  .jump, .elsewhere-row .jump.disabled {
    background: none;
    border: 1px solid transparent;
    color: inherit;
    cursor: pointer;
    text-align: left;
    width: 100%;
    padding: 8px 10px;
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    font: inherit;
    font-size: 0.82rem;
    transition: background-color 120ms, border-color 120ms;
  }

  .jump:hover:not(.disabled) {
    background: var(--bg-surface);
    border-color: var(--border-mid);
  }

  .jump.disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }

  .section-id {
    font-family: var(--font-mono);
    font-size: 0.74rem;
    color: var(--accent, #c9a84c);
    letter-spacing: 0.04em;
  }

  .preview {
    color: var(--text-secondary);
    line-height: 1.4;
    overflow: hidden;
    text-overflow: ellipsis;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }

  .jump-arrow {
    align-self: flex-end;
    color: var(--text-muted);
    font-size: 0.72rem;
  }

  .cross-row {
    font-size: 0.82rem;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px;
    padding: 6px 10px;
    border-radius: 4px;
    color: var(--text-secondary);
  }

  .peer-corpus {
    font-family: var(--font-mono);
    font-size: 0.74rem;
    color: var(--lavender, #9b87c4);
  }

  .peer-name {
    color: var(--text-primary);
    font-weight: 500;
  }

  .signal {
    font-size: 0.72rem;
    color: var(--text-muted);
    margin-left: auto;
    padding: 1px 6px;
    border: 1px solid var(--border-mid);
    border-radius: 999px;
  }
</style>
