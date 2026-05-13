<script lang="ts">
  import type { RelationData } from "../../../types";
  import AtomLink from "../AtomLink.svelte";

  interface Props {
    data: RelationData;
  }

  let { data }: Props = $props();
</script>

<div class="body">
  <p class="label">{data.label}</p>

  <dl class="fields">
    <dt>Relation type</dt>
    <dd class="kind">{data.relation_type}</dd>

    <dt>Participants</dt>
    <dd>
      <ul class="atom-link-list">
        {#each data.participants ?? [] as id, i (id)}
          <li><AtomLink atomId={id} /></li>
          {#if i < (data.participants ?? []).length - 1}
            <li class="separator" aria-hidden="true">↔</li>
          {/if}
        {/each}
      </ul>
    </dd>

    <dt>Span</dt>
    <dd class="mono">
      {data.section_range.start}
      {#if data.section_range.start !== data.section_range.end}
        – {data.section_range.end}
      {/if}
    </dd>
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
  .atom-link-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
  }
  .atom-link-list .separator { color: var(--text-muted); font-size: 0.85rem; }
</style>
