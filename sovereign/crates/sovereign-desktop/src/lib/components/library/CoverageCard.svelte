<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  CoverageCard — what this corpus answers, over what period, as of which
  filing, and what it structurally cannot. FINANCIAL_CORPORA §7.7, bars
  F5 (coverage visible) and F6 (freshness).

  §7.7 settles this surface from the honest-abstention ethos, not from
  taste. Three of its rules are held HERE, by structure:

  1. "A refusal is a correct answer and is never styled as a failure."
     This component uses no `--error`, no `--warning`, no alert role and
     no apology copy. There is no styling hook to key one off: the
     `CoverageLimit` type carries no severity field (types.ts).

  2. "Capability leads; boundaries are facts at EQUAL WEIGHT." Both
     sections render through the SAME `.cc-section` / `.cc-fact` rules,
     so equal weight is one stylesheet rule rather than two that could
     drift apart. Boundaries are not fine print, not a smaller type
     ramp, and not a collapsed <details> disclosure.

  3. "Content is DERIVED, never authored." Every string below except the
     three section headings comes from `coverage_card()` in
     corpus-engine, which reads the corpus's typed store. Nothing here
     names a company, a ticker, or a concept, so a second installed
     filer renders truthfully with no new copy written.

  Renders NOTHING for a corpus with no typed authoritative store — which
  is most of them — so it never adds empty chrome.
-->
<script lang="ts">
  import { corpusCoverageCard } from "../../api";
  import type { CoverageCard } from "../../types";

  interface Props {
    /** The notebook's corpus id. */
    corpusId: string;
  }

  let { corpusId }: Props = $props();

  let card = $state<CoverageCard | null>(null);

  // Same stale-write guard as NotebookOpenQuestions: a slow response for
  // a previous corpus must not land against a newer one.
  $effect(() => {
    const cid = corpusId;
    card = null;
    if (!cid) return;
    corpusCoverageCard(cid)
      .then((c) => {
        if (cid !== corpusId) return;
        card = c;
      })
      .catch(() => {
        // No typed store, or the engine is not up yet. Show nothing
        // rather than an empty or invented card.
        if (cid === corpusId) card = null;
      });
  });
</script>

{#if card && card.answers.length > 0}
  <section class="coverage-card" data-testid="coverage-card">
    <h2>What this notebook answers</h2>

    <!-- Capability, first (§7.7(2)). -->
    <div class="cc-section" data-testid="coverage-answers">
      <h3 class="cc-heading">
        Exact reported figures
        {#if card.period_label}<span class="cc-period">{card.period_label}</span>{/if}
      </h3>
      <ul class="cc-list">
        {#each card.answers as concept (concept.id)}
          <li class="cc-fact">
            <span class="cc-label">{concept.label}</span>
            <span class="cc-period">{concept.period_label}</span>
          </li>
        {/each}
      </ul>
    </div>

    <!-- Boundaries, beside it at the same weight (§7.7(2)) — same
         `.cc-section` and `.cc-fact` rules as the capability list. -->
    <div class="cc-section" data-testid="coverage-limits">
      <h3 class="cc-heading">What it does not answer</h3>
      <ul class="cc-list">
        {#each card.limits as limit (limit.kind)}
          <li class="cc-fact cc-statement" data-limit-kind={limit.kind}>
            {limit.statement}
          </li>
        {/each}
      </ul>
    </div>

    <!-- Always shown (§7.7(5), F6): a corpus that cannot say how current
         it is cannot be trusted about periods. -->
    <p class="cc-as-of" data-testid="coverage-as-of">
      As of the {card.as_of.form} filed {card.as_of.filed} · accession
      {card.as_of.accession} · latest reported period ends
      {card.as_of.latest_period_end}
    </p>
  </section>
{/if}

<style>
  .coverage-card {
    margin-bottom: 24px;
  }

  /* Capability and boundaries share ONE rule. §7.7(2) forbids putting
     boundaries in smaller type or behind a disclosure; making them the
     same class means a future change cannot demote one without visibly
     demoting the other. */
  .cc-section {
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--bg-secondary);
    margin-top: 12px;
  }

  .cc-heading {
    margin: 0 0 10px;
    font-size: 0.92rem;
    font-weight: 600;
    color: var(--text-primary);
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }

  .cc-list {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  /* Also one rule for both lists — same size, same colour. A limit is a
     fact, not fine print. */
  .cc-fact {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    font-size: 0.86rem;
    color: var(--text-secondary);
    line-height: 1.45;
  }

  .cc-statement {
    display: block;
  }

  .cc-label {
    color: var(--text-primary);
  }

  .cc-period {
    font-family: var(--font-mono);
    font-size: 0.78rem;
    color: var(--text-muted);
    white-space: nowrap;
  }

  .cc-as-of {
    margin: 12px 0 0;
    font-size: 0.78rem;
    color: var(--text-muted);
    line-height: 1.5;
  }
</style>
