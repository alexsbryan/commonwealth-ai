<script lang="ts">
  // Atlas Inspector — conversation detail view.
  //
  // Spec: sovereign/docs/specs/CONV_TIERED_PORT.md §"A1 conv corpora
  // in Atlas index — ConvDetail".
  //
  // Renders one conversation's full enrichment surface:
  //   - title + state + chunk count header
  //   - RAPTOR tree: flat (≤2 levels) or hierarchical (>2)
  //   - per-leaf cards: summary, entities, member chunk count
  //
  // Read-only today; future "open chunks in reading surface" hook
  // would live on the chunk-id badges below each leaf.
  import { onMount } from "svelte";
  import { atlasGetConvDetail } from "../../api";
  import type { ConvDetailView, ConvRaptorNodeView } from "../../types";

  interface Props {
    corpusId: string;
    convUuid: string;
    onBack: () => void;
  }

  let { corpusId, convUuid, onBack }: Props = $props();

  let detail: ConvDetailView | null = $state(null);
  let loading = $state(true);
  let error: string | null = $state(null);

  onMount(async () => {
    try {
      detail = await atlasGetConvDetail(corpusId, convUuid);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  });

  function formatTimestamp(unix: number): string {
    return new Date(unix * 1000).toLocaleString();
  }

  /** True when the tree warrants hierarchical rendering — more than
   *  two levels means we have root + intermediate + leaves, where
   *  the hierarchy is informative. Flat (single-level) trees render
   *  as a plain list of leaf cards. */
  function hierarchicalRender(d: ConvDetailView): boolean {
    return d.max_level > 1;
  }

  function leavesOnly(nodes: ConvRaptorNodeView[]): ConvRaptorNodeView[] {
    return nodes.filter((n) => n.level === 0);
  }

  function rootsOnly(nodes: ConvRaptorNodeView[], maxLevel: number): ConvRaptorNodeView[] {
    return nodes.filter((n) => n.level === maxLevel);
  }

  function intermediateLevels(
    nodes: ConvRaptorNodeView[],
    maxLevel: number,
  ): ConvRaptorNodeView[] {
    return nodes.filter((n) => n.level !== 0 && n.level !== maxLevel);
  }

  function stateClass(state: string): string {
    return `state-pill state-${state.toLowerCase()}`;
  }
</script>

<div class="conv-detail">
  <header class="view-header">
    <button class="back-button" type="button" onclick={onBack}>
      ← {corpusId}
    </button>
  </header>

  {#if loading}
    <div class="status">Loading conversation detail…</div>
  {:else if error}
    <div class="status error" role="alert">Failed to load: {error}</div>
  {:else if !detail}
    <div class="status empty">
      <p>No tiered enrichment for this conversation.</p>
      <p class="hint">
        Re-run the ingest to populate conv_skeletons and
        conv_raptor_nodes.
      </p>
    </div>
  {:else}
    <section class="header-card">
      <div class="title-row">
        <h1>{detail.title}</h1>
        <span class={stateClass(detail.state)}>{detail.state}</span>
      </div>
      <div class="meta-row">
        <span>{detail.chunk_count.toLocaleString()} chunks</span>
        <span>{detail.raptor_nodes.length} RAPTOR node{detail.raptor_nodes.length === 1 ? "" : "s"}</span>
        <span>levels {detail.max_level + 1}</span>
        <span>updated {formatTimestamp(detail.updated_at)}</span>
      </div>
    </section>

    {#if detail.raptor_nodes.length === 0}
      <div class="status empty">
        <p>No RAPTOR clusters were built for this conversation.</p>
      </div>
    {:else if detail.raptor_nodes.length === 1 && detail.raptor_nodes[0].is_synthetic_tiny}
      <section class="tier-section">
        <h2>Conversation summary</h2>
        <p class="tiny-note">
          Tiny opt-2: only the conversation title is available. RAPTOR
          clustering is skipped for conversations with fewer than 8
          chunks (no LLM call, no signposts).
        </p>
      </section>
    {:else if hierarchicalRender(detail)}
      <!-- Hierarchical: root → intermediate → leaves. -->
      {@const maxLevel = detail.max_level}
      <section class="tier-section">
        <h2>Root summary{rootsOnly(detail.raptor_nodes, maxLevel).length === 1 ? "" : "ies"}</h2>
        <ul class="node-list">
          {#each rootsOnly(detail.raptor_nodes, maxLevel) as node (node.node_id)}
            {@render renderNode(node)}
          {/each}
        </ul>
      </section>
      {#if intermediateLevels(detail.raptor_nodes, maxLevel).length > 0}
        <section class="tier-section">
          <h2>Intermediate clusters</h2>
          <ul class="node-list">
            {#each intermediateLevels(detail.raptor_nodes, maxLevel) as node (node.node_id)}
              {@render renderNode(node)}
            {/each}
          </ul>
        </section>
      {/if}
      <section class="tier-section">
        <h2>Leaf clusters</h2>
        <ul class="node-list">
          {#each leavesOnly(detail.raptor_nodes) as node (node.node_id)}
            {@render renderNode(node)}
          {/each}
        </ul>
      </section>
    {:else}
      <!-- Flat: just render every node ordered as backend returned. -->
      <section class="tier-section">
        <h2>Clusters</h2>
        <ul class="node-list">
          {#each detail.raptor_nodes as node (node.node_id)}
            {@render renderNode(node)}
          {/each}
        </ul>
      </section>
    {/if}
  {/if}
</div>

{#snippet renderNode(node: ConvRaptorNodeView)}
  <li class="raptor-node" data-level={node.level}>
    <div class="node-header">
      <span class="level-badge">L{node.level}</span>
      <span class="coherence" title="cluster coherence">
        coherence {node.cluster_coherence.toFixed(2)}
      </span>
      {#if node.evidence_chunk_count > 0}
        <span class="evidence">{node.evidence_chunk_count} chunks</span>
      {/if}
    </div>
    <p class="summary">{node.summary}</p>
    {#if node.primary_entities.length > 0}
      <div class="entity-row">
        {#each node.primary_entities as ent (ent)}
          <span class="entity-chip">{ent}</span>
        {/each}
      </div>
    {/if}
  </li>
{/snippet}

<style>
  .conv-detail {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    padding: 1.5rem 2rem;
    max-width: 60rem;
    margin: 0 auto;
  }
  .view-header {
    display: flex;
    align-items: center;
    gap: 1rem;
  }
  .back-button {
    background: transparent;
    border: 1px solid var(--border, #444);
    border-radius: 0.4rem;
    padding: 0.3rem 0.7rem;
    cursor: pointer;
    color: inherit;
    font-size: 0.85rem;
  }
  .back-button:hover {
    background: var(--surface-2, #2a2a2a);
  }
  .header-card {
    background: var(--surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 0.5rem;
    padding: 1rem 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  .title-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.8rem;
  }
  .title-row h1 {
    margin: 0;
    font-size: 1.3rem;
    line-height: 1.3;
  }
  .meta-row {
    display: flex;
    gap: 1.2rem;
    font-size: 0.82rem;
    color: var(--text-muted, #888);
    flex-wrap: wrap;
  }
  .state-pill {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 0.18rem 0.55rem;
    border-radius: 0.7rem;
    background: var(--surface-2, #333);
    color: var(--text-muted, #aaa);
  }
  .state-ready {
    background: rgba(46, 160, 67, 0.18);
    color: #4ec06b;
  }
  .state-multihopready {
    background: rgba(212, 167, 44, 0.18);
    color: #d4a72c;
  }
  .state-partiallyready {
    background: rgba(212, 167, 44, 0.12);
    color: #c39530;
  }
  .state-pending {
    background: rgba(150, 150, 150, 0.18);
    color: #999;
  }
  .state-failed {
    background: rgba(216, 76, 76, 0.18);
    color: #e25555;
  }
  .tier-section h2 {
    font-size: 0.95rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted, #888);
    margin: 0 0 0.6rem;
  }
  .tiny-note {
    color: var(--text-muted, #888);
    font-size: 0.9rem;
    padding: 0.5rem 0;
  }
  .node-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .raptor-node {
    background: var(--surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 0.5rem;
    padding: 0.8rem 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .node-header {
    display: flex;
    gap: 0.7rem;
    font-size: 0.76rem;
    color: var(--text-muted, #888);
    align-items: center;
  }
  .level-badge {
    font-weight: 600;
    color: var(--text-muted, #aaa);
    background: var(--surface-2, #333);
    padding: 0.08rem 0.45rem;
    border-radius: 0.6rem;
  }
  .summary {
    margin: 0;
    font-size: 0.92rem;
    line-height: 1.45;
  }
  .entity-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
  }
  .entity-chip {
    background: rgba(96, 132, 232, 0.16);
    color: #92ade8;
    border-radius: 0.5rem;
    padding: 0.1rem 0.55rem;
    font-size: 0.75rem;
  }
  .status {
    padding: 1rem;
    color: var(--text-muted, #888);
  }
  .status.error {
    color: var(--error, #d44);
  }
  .status.empty {
    text-align: center;
  }
</style>
