<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Atlas-view entity drawer.
  //
  // Mounted as a right-side panel by AtlasConvCorpusView + ConvDetail
  // when the user clicks an entity-chip. Renders the
  // `EntityAggregateRow` returned by `atlas_get_entity_aggregate`:
  // mention/conv/chunk counts, per-label breakdown, top conversations
  // by mention count, and co-occurring entities ranked by
  // shared-chunk count.
  //
  // Read-only Phase 1; clicking a co-occurring entity swaps the
  // drawer's seed so the user can hop laterally without closing.
  // Clicking a conv hit emits `onSelectConv` so the host can route
  // back to ConvDetail.

  import { onMount } from "svelte";
  import { atlasGetEntityAggregate } from "../../api";
  import type { EntityAggregateRow } from "../../types";

  interface Props {
    corpusId: string;
    /** Surface form of the entity that opened the drawer. */
    seed: string;
    onClose: () => void;
    /** Optional jump-back into ConvDetail for the host page that
     *  owns conv routing. */
    onSelectConv?: (convUuid: string) => void;
  }

  let { corpusId, seed = $bindable(), onClose, onSelectConv }: Props =
    $props();

  let row: EntityAggregateRow | null = $state(null);
  let loading = $state(true);
  let error: string | null = $state(null);

  async function loadFor(text: string) {
    loading = true;
    error = null;
    try {
      row = await atlasGetEntityAggregate(corpusId, text);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      row = null;
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void loadFor(seed);
  });

  // Lateral hop: clicking a co-occurring entity re-seeds the drawer
  // in place without unmounting. Faster than close → re-open and
  // matches how readers explore a knowledge graph.
  function jumpToEntity(text: string) {
    seed = text;
    void loadFor(text);
  }

  function handleConvClick(convUuid: string) {
    if (onSelectConv) onSelectConv(convUuid);
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onClose();
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<aside
  class="entity-drawer"
  aria-label={`Details for ${seed}`}
  data-testid="entity-drawer"
>
  <header class="drawer-header">
    <div class="seed">
      <span class="seed-label">Entity</span>
      <h2 class="seed-text" title={seed}>{seed}</h2>
    </div>
    <button
      type="button"
      class="close-button"
      onclick={onClose}
      aria-label="Close entity drawer"
    >
      ×
    </button>
  </header>

  {#if loading}
    <div class="status">Loading…</div>
  {:else if error}
    <div class="status error" role="alert">
      Couldn't load details: {error}
    </div>
  {:else if row}
    <section class="totals" data-testid="entity-drawer-totals">
      <div class="total">
        <span class="total-n">{row.mention_count.toLocaleString()}</span>
        <span class="total-label">mention{row.mention_count === 1 ? "" : "s"}</span>
      </div>
      <div class="total">
        <span class="total-n">{row.conv_count.toLocaleString()}</span>
        <span class="total-label">conv{row.conv_count === 1 ? "" : "s"}</span>
      </div>
      <div class="total">
        <span class="total-n">{row.chunk_count.toLocaleString()}</span>
        <span class="total-label">chunk{row.chunk_count === 1 ? "" : "s"}</span>
      </div>
    </section>

    {#if row.labels.length > 0}
      <section class="labels-section">
        <h3 class="section-h">By type</h3>
        <ul class="label-list">
          {#each row.labels as l (l.label)}
            <li class="label-row">
              <span class="label-name">{l.label}</span>
              <span class="label-count">{l.count.toLocaleString()}</span>
            </li>
          {/each}
        </ul>
        {#if row.labels.length > 1}
          <p class="hint">
            Multiple types — likely a homonym (e.g. a person and an
            organisation sharing the surface form).
          </p>
        {/if}
      </section>
    {/if}

    {#if row.top_convs.length > 0}
      <section class="convs-section">
        <h3 class="section-h">Top conversations</h3>
        <ul class="conv-list">
          {#each row.top_convs as c (c.conv_uuid)}
            <li>
              <button
                type="button"
                class="conv-link"
                onclick={() => handleConvClick(c.conv_uuid)}
                disabled={!onSelectConv}
                title={c.conv_uuid}
              >
                <span class="conv-id">{c.conv_uuid.slice(0, 8)}…</span>
                <span class="conv-count">{c.mention_count.toLocaleString()}</span>
              </button>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if row.co_occurring.length > 0}
      <section class="co-section">
        <h3 class="section-h">Mentioned alongside</h3>
        <ul class="co-list">
          {#each row.co_occurring as co (co.text + ":" + co.label)}
            <li>
              <button
                type="button"
                class="co-chip"
                onclick={() => jumpToEntity(co.text)}
                title={`${co.label} · co-occurs in ${co.shared_chunk_count} chunk${co.shared_chunk_count === 1 ? "" : "s"}`}
              >
                <span class="co-name">{co.text}</span>
                <span class="co-meta">{co.label} · {co.shared_chunk_count.toLocaleString()}</span>
              </button>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if row.mention_count === 0}
      <div class="status empty">
        <p>No mentions found.</p>
        <p class="hint">
          The chip's source may use a different surface form, or the
          entity extraction pass hasn't covered the newest chunks yet.
        </p>
      </div>
    {/if}
  {/if}
</aside>

<style>
  /* Entity drawer — Lavender Court palette. Sibling of
   * AtlasConvCorpusView + ConvDetail. Seed label uses gold accent
   * (it's the active query handle); conv links + co-chips use
   * lavender-flavoured backgrounds to read as "this is the
   * conversations world". */
  .entity-drawer {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    width: 360px;
    max-width: 90vw;
    background: var(--bg-surface);
    border-left: 1px solid var(--border-mid);
    box-shadow: -8px 0 32px rgba(0, 0, 0, 0.35);
    display: flex;
    flex-direction: column;
    padding: 18px 20px;
    gap: 16px;
    overflow-y: auto;
    z-index: 50;
    color: var(--text-primary);
    font-family: var(--font-sans);
  }
  .drawer-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 8px;
  }
  .seed {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .seed-label {
    font-size: 0.66rem;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    color: var(--accent-light);
    font-weight: 600;
  }
  .seed-text {
    margin: 0;
    font-size: 1.2rem;
    line-height: 1.3;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-primary);
    letter-spacing: -0.01em;
  }
  .close-button {
    background: transparent;
    border: 1px solid var(--border-mid);
    color: var(--text-secondary);
    width: 28px;
    height: 28px;
    border-radius: var(--radius);
    cursor: pointer;
    font-size: 1.1rem;
    line-height: 1;
    flex-shrink: 0;
    font-family: inherit;
    transition: border-color 120ms ease, color 120ms ease, background 120ms ease;
  }
  .close-button:hover {
    background: var(--bg-elevated);
    border-color: var(--border-bright);
    color: var(--text-primary);
  }
  .totals {
    display: flex;
    gap: 18px;
    padding: 12px 14px;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .total {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
  }
  .total-n {
    font-size: 1.4rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--text-primary);
    letter-spacing: -0.01em;
  }
  .total-label {
    font-size: 0.66rem;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    color: var(--text-muted);
    font-weight: 500;
  }
  .section-h {
    margin: 0 0 6px;
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    color: var(--text-muted);
    font-weight: 600;
  }
  .label-list,
  .conv-list,
  .co-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .label-row {
    display: flex;
    justify-content: space-between;
    padding: 4px 10px;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 0.82rem;
    color: var(--text-secondary);
  }
  .label-count {
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
  .hint {
    margin: 6px 0 0;
    color: var(--text-muted);
    font-size: 0.76rem;
    font-style: italic;
  }
  .conv-link,
  .co-chip {
    width: 100%;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    color: var(--text-primary);
    border-radius: var(--radius);
    padding: 7px 10px;
    text-align: left;
    cursor: pointer;
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    font-size: 0.82rem;
    font-family: inherit;
    transition: border-color 120ms ease, background 120ms ease;
  }
  .conv-link:hover,
  .co-chip:hover {
    background: var(--lavender-dim);
    border-color: var(--lavender);
  }
  .conv-link:disabled {
    cursor: default;
    opacity: 0.6;
  }
  .conv-id {
    font-family: var(--font-mono);
    font-size: 0.74rem;
    color: var(--text-muted);
  }
  .conv-count,
  .co-meta {
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
    font-size: 0.74rem;
  }
  .co-chip {
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
  }
  .co-name {
    color: var(--text-primary);
    font-weight: 500;
  }
  .status {
    color: var(--text-secondary);
    padding: 10px 0;
    font-size: 0.88rem;
  }
  .status.error {
    color: var(--error);
  }
  .status.empty p {
    margin: 4px 0;
    color: var(--text-muted);
  }
</style>
