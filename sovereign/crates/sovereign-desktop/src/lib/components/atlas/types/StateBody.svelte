<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { StateData } from "../../../types";
  import AtomLink from "../AtomLink.svelte";

  interface Props {
    data: StateData;
  }

  let { data }: Props = $props();
</script>

<div class="body">
  <p class="label">{data.label}</p>

  <dl class="fields">
    <dt>State type</dt>
    <dd class="kind">{data.state_type}</dd>

    <dt>Subject</dt>
    <dd><AtomLink atomId={data.entity_id} /></dd>

    <dt>Span</dt>
    <dd class="mono">
      {data.section_range.start}
      {#if data.section_range.start !== data.section_range.end}
        – {data.section_range.end}
      {/if}
    </dd>

    {#if data.confidence !== undefined}
      <dt>Confidence</dt>
      <dd>{data.confidence.toFixed(2)}</dd>
    {/if}
  </dl>
</div>

<style>
  .body { display: flex; flex-direction: column; gap: 16px; }
  .label { margin: 0; font-size: 1rem; }
  .fields {
    display: grid;
    grid-template-columns: 130px 1fr;
    gap: 6px 14px;
    margin: 0;
    font-size: 0.85rem;
  }
  .fields dt { color: var(--text-muted); font-size: 0.78rem; letter-spacing: 0.02em; }
  .fields dd { margin: 0; }
  .kind { text-transform: capitalize; }
  .mono { font-family: var(--font-mono, monospace); font-size: 0.78rem; }
</style>
