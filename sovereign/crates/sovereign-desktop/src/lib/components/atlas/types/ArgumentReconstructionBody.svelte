<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { ArgumentReconstructionData } from "../../../types";
  import AtomLink from "../AtomLink.svelte";

  interface Props {
    data: ArgumentReconstructionData;
  }

  let { data }: Props = $props();
</script>

<div class="body">
  <h2 class="argument-name">{data.name}</h2>
  {#if data.proponent}
    <p class="proponent">
      Proponent: <AtomLink atomId={data.proponent} />
    </p>
  {/if}

  <section class="premises">
    <h3>Premises</h3>
    <ol>
      {#each data.premises as p, i (i)}
        <li>{p}</li>
      {/each}
    </ol>
  </section>

  <section class="conclusion">
    <h3>Conclusion</h3>
    <p>{data.conclusion}</p>
  </section>

  {#if (data.objections?.length ?? 0) > 0}
    <section class="objections">
      <h3>Objections</h3>
      <ul>
        {#each data.objections ?? [] as o, i (i)}
          <li>
            <span class="objection-name">{o.name}</span>
            {#if o.content}
              <span class="objection-content">— {o.content}</span>
            {/if}
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <dl class="fields">
    <dt>Location</dt>
    <dd class="mono">{data.section_position.section_id}</dd>
  </dl>
</div>

<style>
  .body { display: flex; flex-direction: column; gap: 16px; }
  .argument-name { margin: 0; font-size: 1.05rem; font-weight: 600; }
  .proponent { margin: 0; font-size: 0.85rem; color: var(--text-muted); }
  section h3 {
    margin: 0 0 6px;
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
    font-weight: 500;
  }
  .premises ol {
    margin: 0;
    padding-left: 22px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    line-height: 1.5;
  }
  .conclusion p {
    margin: 0;
    padding: 10px 14px;
    background: var(--bg-secondary);
    border-radius: var(--radius);
    line-height: 1.5;
    font-weight: 500;
  }
  .objections ul {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .objection-name { font-weight: 500; }
  .objection-content { color: var(--text-muted); }
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
</style>
