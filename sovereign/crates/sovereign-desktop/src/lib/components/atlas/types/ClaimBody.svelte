<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { ClaimData } from "../../../types";
  import AtomLink from "../AtomLink.svelte";
  import AttributesRows from "./AttributesRows.svelte";

  interface Props {
    data: ClaimData;
  }

  let { data }: Props = $props();
</script>

<div class="body">
  <p class="content">{data.content}</p>

  {#if data.claim_kind}
    <!-- The author's own noun for this claim (`attribution`, `rule`),
         where the recipe declared one. Also carries the typed-
         extension qualifiers on undeclared corpora — both are "what
         kind of claim is this", which is why they share the field. -->
    <div class="declared-row">
      <span class="declared-chip" data-testid="claim-declared-type">
        {data.claim_kind}
      </span>
    </div>
  {/if}


  {#if data.quotable_excerpt}
    <blockquote class="excerpt">
      <p>"{data.quotable_excerpt}"</p>
      <footer>verbatim from source</footer>
    </blockquote>
  {/if}

  <dl class="fields">
    <dt>Discourse act</dt>
    <dd class="kind">{data.discourse_act}</dd>

    <dt>Epistemic status</dt>
    <dd class="kind">{data.epistemic_status}</dd>

    <dt>Scope</dt>
    <dd class="kind">{data.scope}</dd>

    {#if data.subject}
      <!-- The REFERENT, not the voice. A declared claim type that
           names a `subject` exists precisely for this link: an
           `attribution` claim is ABOUT the coin it dates, and without
           the link the type is present in name only. -->
      <dt>About</dt>
      <dd data-testid="claim-subject"><AtomLink atomId={data.subject} /></dd>
    {/if}

    {#if data.attributed_to}
      <dt>Attributed to</dt>
      <dd><AtomLink atomId={data.attributed_to} /></dd>
    {/if}


    {#if data.confidence !== undefined}
      <dt>Confidence</dt>
      <dd>{data.confidence.toFixed(2)}</dd>
    {/if}
  </dl>

  <AttributesRows attributes={data.attributes} />
</div>


<style>
  .body { display: flex; flex-direction: column; gap: 16px; }
  .content { margin: 0; line-height: 1.55; font-size: 1rem; }
  .excerpt {
    margin: 0;
    padding: 12px 16px;
    border-left: 2px solid var(--accent);
    background: var(--bg-secondary);
    border-radius: 0 var(--radius) var(--radius) 0;
  }
  .excerpt p { margin: 0; font-style: italic; line-height: 1.55; }
  .excerpt footer { margin-top: 6px; font-size: 0.72rem; color: var(--text-muted); letter-spacing: 0.02em; }
  .fields {
    display: grid;
    grid-template-columns: 140px 1fr;
    gap: 6px 14px;
    margin: 0;
    font-size: 0.85rem;
  }
  .fields dt { color: var(--text-muted); font-size: 0.78rem; letter-spacing: 0.02em; }
  .fields dd { margin: 0; }
  .kind { text-transform: capitalize; }
  .declared-row { display: flex; gap: 6px; flex-wrap: wrap; }
  .declared-chip {
    padding: 2px 9px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 10px;
    font-size: 0.75rem;
    color: var(--text-secondary);
    font-family: var(--font-mono, monospace);
  }
</style>

