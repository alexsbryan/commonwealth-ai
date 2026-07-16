<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
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
  import { atlasGetConvDetail, lcReenrichNote } from "../../api";
  import type { ConvDetailView, ConvRaptorNodeView } from "../../types";
  import EntityDrawer from "./EntityDrawer.svelte";

  interface Props {
    corpusId: string;
    convUuid: string;
    onBack: () => void;
    /** Optional jump-to-conv from the drawer's "Top conversations"
     *  list. Host (AtlasSurface) wires this to its conv routing. */
    onSelectConv?: (convUuid: string) => void;
  }

  let { corpusId, convUuid, onBack, onSelectConv }: Props = $props();

  let drawerSeed: string | null = $state(null);

  function openDrawer(name: string) {
    drawerSeed = name;
  }

  function closeDrawer() {
    drawerSeed = null;
  }

  function handleDrawerSelectConv(uuid: string) {
    closeDrawer();
    if (onSelectConv) onSelectConv(uuid);
  }

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

  function formatDay(unix: number): string {
    return new Date(unix * 1000).toLocaleDateString();
  }

  // ── Summary revision loop ────────────────────────────────────────
  // Flag a wrong summary → re-enrich just this note, guided by an
  // optional correction hint (docs/specs/SUMMARY_REVISION_LOOP.md).
  // The button is per-cluster (captures the specific wrong summary as
  // context), but the rebuild is note-wide, so node ids change and we
  // reload the whole detail on success.
  let flaggingNodeId: string | null = $state(null);
  let hintText = $state("");
  let reenriching = $state(false);
  let reenrichError: string | null = $state(null);

  function openFlag(node: ConvRaptorNodeView) {
    flaggingNodeId = node.node_id;
    hintText = "";
    reenrichError = null;
  }

  function cancelFlag() {
    flaggingNodeId = null;
    reenrichError = null;
  }

  async function submitFlag(node: ConvRaptorNodeView) {
    reenriching = true;
    reenrichError = null;
    try {
      await lcReenrichNote(corpusId, convUuid, hintText, node.summary);
      // Node ids are regenerated on rebuild — reload the whole detail.
      detail = await atlasGetConvDetail(corpusId, convUuid);
      flaggingNodeId = null;
    } catch (e) {
      reenrichError = e instanceof Error ? e.message : String(e);
    } finally {
      reenriching = false;
    }
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

  /** Plain-language labels for the per-conv enrichment states. */
  const STATE_LABEL: Record<string, string> = {
    Ready: "Ready",
    MultiHopReady: "Partly ready",
    PartiallyReady: "Indexing…",
    Pending: "Waiting",
    Failed: "Failed",
  };

  function stateLabel(state: string): string {
    return STATE_LABEL[state] ?? state;
  }

  /** Per-leaf entity-diff highlighting. Counts how many distinct
   *  level-0 RAPTOR nodes mention each entity. An entity appearing
   *  in exactly one leaf gets a "distinctive" chip style — that's
   *  what differentiates the leaf from its siblings. Entities
   *  appearing in 2+ leaves render as plain shared chips.
   *
   *  Why: leaf summaries often paraphrase to the conversation's
   *  dominant theme (e.g. "the framework was applied to country
   *  group X"), making sibling leaves read as near-identical. The
   *  entity-frequency diff is where the actual specificity lives.
   *  Sample case live 2026-05-22: "Beyond GDP" conv had two
   *  level-0 leaves with similar prose summaries; one's distinctive
   *  entities were `Park Chunghee`, `Mahathir Mohamad`, `Bumiputera`
   *  (Asian-tiger application) while the other was generic
   *  country exemplars. Highlighting surfaces that diff at-a-glance.
   *
   *  Diff only applies to level-0 leaves; roots/intermediates
   *  aggregate entities by construction so distinctiveness isn't
   *  meaningful at those levels.
   */
  let entityFreqAcrossLeaves = $derived.by(() => {
    const counts = new Map<string, number>();
    if (!detail) return counts;
    for (const n of detail.raptor_nodes) {
      if (n.level !== 0) continue;
      // Dedupe within a single node so a duplicated entity in one
      // leaf doesn't inflate its global count.
      const seen = new Set<string>();
      for (const e of n.primary_entities) {
        if (seen.has(e)) continue;
        seen.add(e);
        counts.set(e, (counts.get(e) ?? 0) + 1);
      }
    }
    return counts;
  });

  function entityClass(name: string, nodeLevel: number): string {
    if (nodeLevel !== 0) return "entity-chip";
    const count = entityFreqAcrossLeaves.get(name) ?? 0;
    return count === 1 ? "entity-chip entity-chip-distinctive" : "entity-chip";
  }

  function entityTitle(name: string, nodeLevel: number): string {
    if (nodeLevel !== 0) return name;
    const count = entityFreqAcrossLeaves.get(name) ?? 0;
    if (count === 1) {
      return `${name} · unique to this cluster`;
    }
    return `${name} · in ${count} clusters`;
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
        <span
          class={stateClass(detail.state)}
          title={detail.state}
        >{stateLabel(detail.state)}</span>
      </div>
      <div class="meta-row">
        <span>{detail.chunk_count.toLocaleString()} messages</span>
        <span>{detail.raptor_nodes.length} topic cluster{detail.raptor_nodes.length === 1 ? "" : "s"}</span>
        <span>{detail.max_level + 1} level{detail.max_level === 0 ? "" : "s"} of summary</span>
        <span>updated {formatTimestamp(detail.updated_at)}</span>
        {#if detail.correction?.status === "applied"}
          <span
            class="revised-badge"
            title={detail.correction.correction_hint ?? "You corrected this summary"}
          >✓ revised by you · {formatDay(detail.correction.created_at)}</span>
        {/if}
      </div>
    </section>

    {#if detail.raptor_nodes.length === 0}
      <div class="status empty">
        <p>No topic clusters built for this conversation yet.</p>
      </div>
    {:else if detail.raptor_nodes.length === 1 && detail.raptor_nodes[0].is_synthetic_tiny}
      <section class="tier-section">
        <h2>Conversation summary</h2>
        <p class="tiny-note">
          This conversation is too short to break into topic clusters —
          only the conversation title is shown above. Searches still
          find this chat by content.
        </p>
      </section>
    {:else if hierarchicalRender(detail)}
      <!-- Hierarchical: top-level → middle → leaf clusters. -->
      {@const maxLevel = detail.max_level}
      <section class="tier-section">
        <h2>Top-level summar{rootsOnly(detail.raptor_nodes, maxLevel).length === 1 ? "y" : "ies"}</h2>
        <ul class="node-list">
          {#each rootsOnly(detail.raptor_nodes, maxLevel) as node (node.node_id)}
            {@render renderNode(node)}
          {/each}
        </ul>
      </section>
      {#if intermediateLevels(detail.raptor_nodes, maxLevel).length > 0}
        <section class="tier-section">
          <h2>Mid-level themes</h2>
          <ul class="node-list">
            {#each intermediateLevels(detail.raptor_nodes, maxLevel) as node (node.node_id)}
              {@render renderNode(node)}
            {/each}
          </ul>
        </section>
      {/if}
      <section class="tier-section">
        <h2>Topic clusters</h2>
        <ul class="node-list">
          {#each leavesOnly(detail.raptor_nodes) as node (node.node_id)}
            {@render renderNode(node)}
          {/each}
        </ul>
      </section>
    {:else}
      <!-- Flat: just render every node ordered as backend returned. -->
      <section class="tier-section">
        <h2>Topics in this conversation</h2>
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
      <span class="level-badge" title={`Cluster depth ${node.level}`}>
        {node.level === 0 ? "topic" : `level ${node.level}`}
      </span>
      <span
        class="coherence"
        title="How tightly the messages in this cluster cohere (0–1)"
      >
        tightness {node.cluster_coherence.toFixed(2)}
      </span>
      {#if node.evidence_chunk_count > 0}
        <span class="evidence">{node.evidence_chunk_count} message{node.evidence_chunk_count === 1 ? "" : "s"}</span>
      {/if}
      <button
        type="button"
        class="flag-button"
        title="Flag this summary as wrong and re-enrich this note"
        onclick={() => openFlag(node)}
        disabled={reenriching}
      >⚑ fix</button>
    </div>
    <p class="summary">{node.summary}</p>
    {#if flaggingNodeId === node.node_id}
      <div class="flag-form">
        <label class="flag-label" for={`hint-${node.node_id}`}>
          What did it get wrong?
          <span class="flag-hint-note">optional, but a hint makes the fix far better</span>
        </label>
        <textarea
          id={`hint-${node.node_id}`}
          class="flag-textarea"
          bind:value={hintText}
          rows="3"
          placeholder="e.g. Yakumo is the village/setting; Grandmother Sato is the character who keeps the journal."
          disabled={reenriching}
        ></textarea>
        {#if reenrichError}
          <p class="flag-error" role="alert">{reenrichError}</p>
        {/if}
        <div class="flag-actions">
          <button
            type="button"
            class="flag-cancel"
            onclick={cancelFlag}
            disabled={reenriching}
          >Cancel</button>
          <button
            type="button"
            class="flag-submit"
            onclick={() => submitFlag(node)}
            disabled={reenriching}
          >{reenriching ? "Re-enriching this note…" : "Re-enrich this note"}</button>
        </div>
      </div>
    {/if}
    {#if node.primary_entities.length > 0}
      <div class="entity-row">
        {#each node.primary_entities as ent (ent)}
          <button
            type="button"
            class={entityClass(ent, node.level)}
            title={entityTitle(ent, node.level)}
            onclick={() => openDrawer(ent)}
          >{ent}</button>
        {/each}
      </div>
    {/if}
  </li>
{/snippet}

{#if drawerSeed !== null}
  <EntityDrawer
    {corpusId}
    seed={drawerSeed}
    onClose={closeDrawer}
    onSelectConv={handleDrawerSelectConv}
  />
{/if}

<style>
  /* Atlas conv detail — sibling palette to AtlasConvCorpusView.
   * Lavender Court tokens throughout. Shared entities = lavender
   * wash; distinctive entities = gold accent (signal: "this is
   * what makes this cluster stand apart from its siblings").
   * Level badges use border-bright to read as system metadata
   * rather than competing with content colour. */
  .conv-detail {
    display: flex;
    flex-direction: column;
    gap: 18px;
    padding: 28px 32px 44px;
    max-width: 64rem;
    margin: 0 auto;
    font-family: var(--font-sans);
    color: var(--text-primary);
  }
  .view-header {
    display: flex;
    align-items: center;
    gap: 16px;
  }
  .back-button {
    background: transparent;
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    padding: 6px 12px;
    cursor: pointer;
    color: var(--text-secondary);
    font-size: 0.82rem;
    font-family: inherit;
    letter-spacing: 0.01em;
    transition: border-color 120ms ease, color 120ms ease, background 120ms ease;
  }
  .back-button:hover {
    background: var(--bg-elevated);
    border-color: var(--border-bright);
    color: var(--text-primary);
  }
  .header-card {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 16px 20px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .title-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
  }
  .title-row h1 {
    margin: 0;
    font-size: 1.35rem;
    font-weight: 600;
    line-height: 1.3;
    letter-spacing: -0.01em;
    color: var(--text-primary);
  }
  .meta-row {
    display: flex;
    gap: 18px;
    font-size: 0.8rem;
    color: var(--text-muted);
    flex-wrap: wrap;
  }
  .state-pill {
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    padding: 3px 9px;
    border-radius: 999px;
    background: var(--bg-elevated);
    color: var(--text-muted);
    font-weight: 500;
  }
  .state-ready {
    background: var(--growth-dim);
    color: var(--growth);
  }
  .state-multihopready {
    background: var(--accent-dim);
    color: var(--accent-light);
  }
  .state-partiallyready {
    background: var(--lavender-dim);
    color: var(--lavender-light);
  }
  .state-pending {
    background: var(--bg-elevated);
    color: var(--text-muted);
  }
  .state-failed {
    background: var(--coral-dim);
    color: var(--coral);
  }
  .tier-section {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .tier-section h2 {
    font-size: 0.78rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    color: var(--text-muted);
    margin: 0 0 4px;
  }
  .tiny-note {
    color: var(--text-secondary);
    font-size: 0.88rem;
    padding: 6px 0;
    line-height: 1.5;
  }
  .node-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .raptor-node {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 12px 16px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  /* Deeper levels get a subtler card — visual depth follows tree
   * depth. Level 0 (leaves) read brightest; intermediates step
   * down; root tints toward bg-secondary. */
  .raptor-node[data-level="1"] {
    background: var(--bg-elevated);
  }
  .raptor-node[data-level="2"],
  .raptor-node[data-level="3"] {
    background: var(--bg-elevated);
    border-color: var(--border-mid);
  }
  .node-header {
    display: flex;
    gap: 10px;
    font-size: 0.74rem;
    color: var(--text-muted);
    align-items: center;
    flex-wrap: wrap;
  }
  .level-badge {
    font-weight: 600;
    color: var(--text-secondary);
    background: var(--bg-elevated);
    border: 1px solid var(--border-mid);
    padding: 1px 7px;
    border-radius: 999px;
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .coherence,
  .evidence {
    font-size: 0.74rem;
    color: var(--text-muted);
  }
  .summary {
    margin: 0;
    font-size: 0.92rem;
    line-height: 1.55;
    color: var(--text-primary);
  }
  .entity-row {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    margin-top: 2px;
  }
  /* Entity chip — lavender wash, matches AtlasConvCorpusView. */
  .entity-chip {
    background: var(--lavender-dim);
    color: var(--lavender-light);
    border-radius: var(--radius);
    padding: 2px 9px;
    font-size: 0.74rem;
    border: 1px solid transparent;
    font-family: inherit;
    cursor: pointer;
    transition: background 120ms ease, border-color 120ms ease, color 120ms ease;
  }
  .entity-chip:hover {
    background: var(--lavender-glow);
    border-color: var(--lavender);
    color: var(--text-primary);
  }
  .entity-chip:focus-visible {
    outline: 2px solid var(--lavender);
    outline-offset: 1px;
  }
  /* Distinctive chip — entity unique to this leaf among its
   * level-0 siblings. Gold wash + outlined accent signals "this
   * cluster owns this entity"; reads at a glance against the
   * baseline lavender. */
  .entity-chip-distinctive {
    background: var(--accent-dim);
    color: var(--accent-light);
    border: 1px solid var(--accent);
    font-weight: 500;
  }
  .entity-chip-distinctive:hover {
    background: var(--accent-glow);
    color: var(--text-primary);
  }
  .entity-chip-distinctive:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .status {
    padding: 18px 4px;
    color: var(--text-secondary);
    font-size: 0.92rem;
  }
  .status.error {
    color: var(--error);
  }
  .status.empty {
    text-align: center;
    padding: 32px 4px;
  }
  /* ── Summary revision loop — flag + inline correction form + badge ── */
  .revised-badge {
    font-size: 0.72rem;
    color: var(--growth);
    background: var(--growth-dim);
    border-radius: 999px;
    padding: 2px 9px;
    font-weight: 500;
    letter-spacing: 0.01em;
  }
  .flag-button {
    margin-left: auto;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius);
    padding: 1px 8px;
    font-size: 0.72rem;
    font-family: inherit;
    color: var(--text-muted);
    cursor: pointer;
    opacity: 0.55;
    transition: opacity 120ms ease, color 120ms ease, border-color 120ms ease,
      background 120ms ease;
  }
  .flag-button:hover:not(:disabled) {
    opacity: 1;
    color: var(--coral);
    border-color: var(--coral-dim);
    background: var(--coral-dim);
  }
  .flag-button:disabled {
    cursor: default;
    opacity: 0.3;
  }
  .flag-form {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 8px;
    padding: 12px;
    background: var(--bg-elevated);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
  }
  .flag-label {
    font-size: 0.8rem;
    color: var(--text-secondary);
    font-weight: 500;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .flag-hint-note {
    font-size: 0.72rem;
    color: var(--text-muted);
    font-weight: 400;
  }
  .flag-textarea {
    width: 100%;
    box-sizing: border-box;
    resize: vertical;
    font-family: inherit;
    font-size: 0.85rem;
    line-height: 1.5;
    padding: 8px 10px;
    color: var(--text-primary);
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
  }
  .flag-textarea:focus-visible {
    outline: 2px solid var(--lavender);
    outline-offset: 1px;
  }
  .flag-error {
    margin: 0;
    font-size: 0.8rem;
    color: var(--error);
  }
  .flag-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .flag-cancel,
  .flag-submit {
    font-family: inherit;
    font-size: 0.8rem;
    padding: 6px 14px;
    border-radius: var(--radius);
    cursor: pointer;
    border: 1px solid var(--border-mid);
    transition: background 120ms ease, border-color 120ms ease, color 120ms ease;
  }
  .flag-cancel {
    background: transparent;
    color: var(--text-secondary);
  }
  .flag-cancel:hover:not(:disabled) {
    border-color: var(--border-bright);
    color: var(--text-primary);
  }
  .flag-submit {
    background: var(--lavender-dim);
    color: var(--lavender-light);
    border-color: var(--lavender);
  }
  .flag-submit:hover:not(:disabled) {
    background: var(--lavender-glow);
    color: var(--text-primary);
  }
  .flag-cancel:disabled,
  .flag-submit:disabled {
    cursor: default;
    opacity: 0.5;
  }
</style>
