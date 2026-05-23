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
  .entity-drawer {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    width: 360px;
    max-width: 90vw;
    background: var(--surface, #1a1a1a);
    border-left: 1px solid var(--border, #333);
    box-shadow: -4px 0 24px rgba(0, 0, 0, 0.3);
    display: flex;
    flex-direction: column;
    padding: 1rem 1.1rem;
    gap: 1rem;
    overflow-y: auto;
    z-index: 50;
    color: var(--text-primary, #ddd);
    font-family: var(--font-sans);
  }
  .drawer-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .seed {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
  }
  .seed-label {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted, #888);
  }
  .seed-text {
    margin: 0;
    font-size: 1.2rem;
    line-height: 1.3;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .close-button {
    background: transparent;
    border: 1px solid var(--border, #444);
    color: inherit;
    width: 28px;
    height: 28px;
    border-radius: 0.4rem;
    cursor: pointer;
    font-size: 1.1rem;
    line-height: 1;
    flex-shrink: 0;
  }
  .close-button:hover {
    background: var(--surface-2, #2a2a2a);
  }
  .totals {
    display: flex;
    gap: 1rem;
    padding: 0.75rem;
    background: var(--surface-2, #232323);
    border-radius: 0.5rem;
  }
  .total {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.1rem;
  }
  .total-n {
    font-size: 1.4rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--text-primary, #ddd);
  }
  .total-label {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted, #888);
  }
  .section-h {
    margin: 0 0 0.4rem;
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted, #888);
  }
  .label-list,
  .conv-list,
  .co-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .label-row {
    display: flex;
    justify-content: space-between;
    padding: 0.25rem 0.5rem;
    background: var(--surface-2, #232323);
    border-radius: 0.35rem;
    font-size: 0.85rem;
  }
  .label-count {
    color: var(--text-muted, #888);
    font-variant-numeric: tabular-nums;
  }
  .hint {
    margin: 0.4rem 0 0;
    color: var(--text-muted, #888);
    font-size: 0.78rem;
    font-style: italic;
  }
  .conv-link,
  .co-chip {
    width: 100%;
    background: var(--surface-2, #232323);
    border: 1px solid transparent;
    color: inherit;
    border-radius: 0.4rem;
    padding: 0.4rem 0.55rem;
    text-align: left;
    cursor: pointer;
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.82rem;
  }
  .conv-link:hover,
  .co-chip:hover {
    background: var(--surface-3, #2a2a2a);
    border-color: var(--border, #444);
  }
  .conv-link:disabled {
    cursor: default;
    opacity: 0.7;
  }
  .conv-id {
    font-family: var(--font-mono, monospace);
    font-size: 0.78rem;
    color: var(--text-muted, #999);
  }
  .conv-count,
  .co-meta {
    color: var(--text-muted, #888);
    font-variant-numeric: tabular-nums;
    font-size: 0.76rem;
  }
  .co-chip {
    flex-direction: column;
    align-items: flex-start;
    gap: 0.15rem;
  }
  .co-name {
    color: var(--text-primary, #ddd);
  }
  .status {
    color: var(--text-muted, #888);
    padding: 0.6rem 0;
  }
  .status.error {
    color: #e25555;
  }
  .status.empty p {
    margin: 0.3rem 0;
  }
</style>
