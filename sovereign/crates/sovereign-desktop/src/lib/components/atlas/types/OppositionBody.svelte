<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // A NAMED X-vs-Y framing the section sets up.
  //
  // The two sides always carry their raw label, and carry an atom id
  // only when fuzzy-match snapped them to an existing Concept. Render
  // the label either way — a side that did not resolve is still what
  // the section said, and dropping it to show only the resolved half
  // would misreport the framing.
  import type { OppositionData } from "../../../types";
  import AtomLink from "../AtomLink.svelte";

  interface Props {
    data: OppositionData;
  }

  let { data }: Props = $props();
</script>

<div class="body">
  <div class="sides">
    <div class="side">
      <span class="side-label">{data.left_label}</span>
      {#if data.left_atom_id}
        <AtomLink atomId={data.left_atom_id} />
      {/if}
    </div>
    <span class="versus" aria-hidden="true">vs</span>
    <div class="side">
      <span class="side-label">{data.right_label}</span>
      {#if data.right_atom_id}
        <AtomLink atomId={data.right_atom_id} />
      {/if}
    </div>
  </div>

  {#if data.framing}
    <p class="framing">{data.framing}</p>
  {/if}

  <dl class="fields">
    {#if data.axis}
      <dt>Axis</dt>
      <dd>{data.axis}</dd>
    {/if}

    {#if (data.anchors ?? []).length > 0}
      <dt>Anchors</dt>
      <dd class="anchors">{(data.anchors ?? []).join(" · ")}</dd>
    {/if}

    <dt>First seen</dt>
    <dd class="mono">{data.first_appearance.chunk_id}</dd>
  </dl>
</div>

<style>
  .body { display: flex; flex-direction: column; gap: 16px; }
  .sides {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }
  .side { display: flex; align-items: center; gap: 6px; }
  .side-label { font-size: 1rem; }
  .versus {
    color: var(--text-muted);
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .framing { margin: 0; line-height: 1.55; }
  .fields {
    display: grid;
    grid-template-columns: 130px 1fr;
    gap: 6px 14px;
    margin: 0;
    font-size: 0.85rem;
  }
  .fields dt { color: var(--text-muted); font-size: 0.78rem; letter-spacing: 0.02em; }
  .fields dd { margin: 0; }
  .mono { font-family: var(--font-mono, monospace); font-size: 0.78rem; }
  .anchors { color: var(--text-secondary); }
</style>
