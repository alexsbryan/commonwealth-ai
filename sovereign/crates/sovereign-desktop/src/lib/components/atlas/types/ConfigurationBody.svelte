<script lang="ts">
  import type { ConfigurationData } from "../../../types";
  import AtomLink from "../AtomLink.svelte";

  interface Props {
    data: ConfigurationData;
  }

  let { data }: Props = $props();
</script>

<div class="body">
  <p class="label">{data.label}</p>
  <p class="description">{data.description}</p>

  {#if data.interpretive_note}
    <div class="ricoeur-note" aria-label="Interpretive note">
      <span class="tag">Interpretive note</span>
      <p>{data.interpretive_note}</p>
    </div>
  {/if}

  <dl class="fields">
    <dt>Confidence</dt>
    <dd>{data.confidence.toFixed(2)}</dd>

    {#if data.constituent_atoms.length > 0}
      <dt>Constituent atoms</dt>
      <dd>
        <ul class="atom-link-list">
          {#each data.constituent_atoms as id (id)}
            <li><AtomLink atomId={id} /></li>
          {/each}
        </ul>
      </dd>
    {/if}
  </dl>
</div>

<style>
  .body { display: flex; flex-direction: column; gap: 16px; }
  .label { margin: 0; font-size: 1rem; font-weight: 600; }
  .description { margin: 0; line-height: 1.55; }
  .ricoeur-note {
    padding: 10px 14px;
    background: var(--bg-secondary);
    border-left: 2px solid var(--text-muted);
    border-radius: 0 var(--radius) var(--radius) 0;
  }
  .ricoeur-note .tag {
    display: inline-block;
    font-size: 0.7rem;
    letter-spacing: 0.04em;
    color: var(--text-muted);
    margin-bottom: 4px;
    text-transform: uppercase;
  }
  .ricoeur-note p { margin: 0; font-size: 0.88rem; line-height: 1.5; font-style: italic; }
  .fields {
    display: grid;
    grid-template-columns: 150px 1fr;
    gap: 6px 14px;
    margin: 0;
    font-size: 0.85rem;
  }
  .fields dt { color: var(--text-muted); font-size: 0.78rem; letter-spacing: 0.02em; }
  .fields dd { margin: 0; }
  .atom-link-list { list-style: none; padding: 0; margin: 0; display: flex; flex-wrap: wrap; gap: 4px; }
</style>
