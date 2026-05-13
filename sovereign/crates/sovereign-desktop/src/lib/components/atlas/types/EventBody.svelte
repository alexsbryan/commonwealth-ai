<script lang="ts">
  import type { EventData } from "../../../types";
  import AtomLink from "../AtomLink.svelte";

  interface Props {
    data: EventData;
  }

  let { data }: Props = $props();
</script>

<div class="body">
  <p class="description">{data.description}</p>

  <dl class="fields">
    <dt>Event type</dt>
    <dd class="kind">{data.event_type}</dd>

    <dt>Location</dt>
    <dd class="mono">
      {data.section_position.section_id}{#if data.section_position.paragraph_index !== undefined}
        / ¶{data.section_position.paragraph_index}
      {/if}
    </dd>

    {#if (data.participants?.length ?? 0) > 0}
      <dt>Participants</dt>
      <dd>
        <ul class="atom-link-list">
          {#each data.participants ?? [] as id (id)}
            <li><AtomLink atomId={id} /></li>
          {/each}
        </ul>
      </dd>
    {/if}

    {#if (data.causal_antecedents?.length ?? 0) > 0}
      <dt>Caused by</dt>
      <dd>
        <ul class="atom-link-list">
          {#each data.causal_antecedents ?? [] as id (id)}
            <li><AtomLink atomId={id} /></li>
          {/each}
        </ul>
      </dd>
    {/if}
  </dl>
</div>

<style>
  .body { display: flex; flex-direction: column; gap: 16px; }
  .description { margin: 0; line-height: 1.55; }
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
  .atom-link-list { list-style: none; padding: 0; margin: 0; display: flex; flex-wrap: wrap; gap: 4px; }
</style>
