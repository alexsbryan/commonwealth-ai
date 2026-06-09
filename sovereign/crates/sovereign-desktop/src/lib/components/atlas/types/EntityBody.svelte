<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { EntityData } from "../../../types";
  import AtomLink from "../AtomLink.svelte";

  interface Props {
    data: EntityData;
  }

  let { data }: Props = $props();
</script>

<div class="body">
  {#if data.description}
    <p class="description">{data.description}</p>
  {/if}

  {#if data.defining_quote}
    <blockquote class="defining-quote">
      <p>"{data.defining_quote}"</p>
      <footer>verbatim from source</footer>
    </blockquote>
  {/if}

  <dl class="fields">
    <dt>Entity type</dt>
    <dd class="kind">{data.entity_type}</dd>

    {#if (data.aliases?.length ?? 0) > 0}
      <dt>Aliases</dt>
      <dd>
        <ul class="aliases">
          {#each data.aliases ?? [] as alias (alias)}
            <li>{alias}</li>
          {/each}
        </ul>
      </dd>
    {/if}

    {#if data.affiliation}
      <dt>Affiliation</dt>
      <dd>{data.affiliation}</dd>
    {/if}
    {#if data.role}
      <dt>Role</dt>
      <dd>{data.role}</dd>
    {/if}

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
  </dl>
</div>

<style>
  .body {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .description {
    margin: 0;
    line-height: 1.55;
    color: var(--text-primary);
  }

  .defining-quote {
    margin: 0;
    padding: 12px 16px;
    border-left: 2px solid var(--accent);
    background: var(--bg-secondary);
    border-radius: 0 var(--radius) var(--radius) 0;
  }

  .defining-quote p {
    margin: 0;
    font-style: italic;
    line-height: 1.55;
  }

  .defining-quote footer {
    margin-top: 6px;
    font-size: 0.72rem;
    color: var(--text-muted);
    letter-spacing: 0.02em;
  }

  .fields {
    display: grid;
    grid-template-columns: 130px 1fr;
    gap: 6px 14px;
    margin: 0;
    font-size: 0.85rem;
  }

  .fields dt {
    color: var(--text-muted);
    font-size: 0.78rem;
    letter-spacing: 0.02em;
  }

  .fields dd {
    margin: 0;
    color: var(--text-primary);
  }

  .kind {
    text-transform: capitalize;
  }

  .aliases {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .aliases li {
    padding: 2px 8px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 10px;
    font-size: 0.78rem;
  }

  .atom-link-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
</style>
