<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // A NAMED stance the corpus identifies — "the view that X".
  //
  // Until 2026-09-02 there was no arm for `Position` in AtomDetail's
  // dispatch chain, so opening one rendered an EMPTY body under a
  // BLANK type pill. No error and no boundary trip: the `{#if}` chain
  // simply ran out of arms.
  import type { PositionData } from "../../../types";
  import AtomLink from "../AtomLink.svelte";

  interface Props {
    data: PositionData;
  }

  let { data }: Props = $props();

  /** How the corpus holds the stance. Four known values; an unknown
   *  one prints verbatim rather than being coerced into one of them. */
  const STANCE_GLOSS: Record<string, string> = {
    endorse: "the section endorses this view",
    rebut: "the section rebuts this view",
    survey: "the section surveys this view without taking it",
    mixed: "the section is of two minds about this view",
  };
</script>

<div class="body">
  <p class="content">{data.content}</p>

  <dl class="fields">
    <dt>Stance</dt>
    <dd class="kind" title={STANCE_GLOSS[data.stance] ?? ""}>{data.stance}</dd>

    {#if data.proponent_id}
      <dt>Proponent</dt>
      <dd><AtomLink atomId={data.proponent_id} /></dd>
    {/if}

    {#if (data.evidence_ids ?? []).length > 0}
      <dt>Supported by</dt>
      <dd>
        <ul class="atom-link-list">
          {#each data.evidence_ids ?? [] as id (id)}
            <li><AtomLink atomId={id} /></li>
          {/each}
        </ul>
      </dd>
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
  .content { margin: 0; line-height: 1.55; font-size: 1rem; }
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
  .anchors { color: var(--text-secondary); }
  .atom-link-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
  }
</style>
