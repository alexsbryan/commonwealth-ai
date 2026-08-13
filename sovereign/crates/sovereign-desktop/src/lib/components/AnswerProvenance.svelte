<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  AnswerProvenance — the flag-on provenance strip under an answer.

  Renders `metadata.answer_segments` (NATIVE_GROUNDING.md §6) as a
  collapsed one-line summary that opens into the answer tiled by
  provenance: which sentences were found verbatim in the user's own
  sources (and where), which are the model's own words, which failed to
  resolve. A grounded row with a real address opens the SAME reading
  surface an inline citation opens — one reader, one way in.

  Absent on every flag-off turn: `readAnswerProvenance` returns null when
  the metadata key is missing, and this component renders nothing. A turn
  that segmented and resolved nothing renders a zero-count strip instead,
  because "not measured" and "measured, found nothing" are different
  facts (ARCH §18.3).

  It says WHERE TEXT IS, never whether it is true — the span resolver
  certifies at 0.7429 precision against the incumbent judge
  (bench/calibration/resolver-precision/), so the copy is deliberately
  locational.

  ── G4: the stack-attribution strip ──────────────────────────────────
  Also renders `metadata.stage_attribution` — which SYSTEM spent the
  turn's time, stage by stage (NATIVE_GROUNDING_ECONOMY.md §3.4, §9
  Phase 1). It is an ATTRIBUTION, not a profiler: a profiler says
  "gate 121s" and still needs the reader to know what belongs in a gate,
  so every row here carries the owning stack beside its cost.

  The operator's sentence this exists to satisfy: "we should be able to
  tell immediately and in the UI that we're using only the new system ...
  so we can just look at one UI element and say 'oh wait that's part of
  the old system and it's eating up all the time'." Establishing that
  same fact by hand cost four hours of archaeology on 2026-08-12.

  Rendering only. Every judgement — which stack owns a stage, which
  stacks served the turn, how much the old one took — is made by the
  runtime and read here (ARCH §10.6).
-->
<script lang="ts">
  import {
    readAnswerProvenance,
    readStageAttribution,
    readTypedAbstention,
  } from "./answerProvenance";

  interface Props {
    metadata?: Record<string, unknown>;
    /** The prose the user actually read — the string the byte ranges
     *  index into. Must be the released text, not the raw content with
     *  think blocks, or every range lands in the wrong place. */
    answerText: string;
    /** Opens a chunk in the reading surface. Unset = rows render
     *  un-clickable (the strip still reports what resolved). */
    onOpenCitation?: (corpusId: string, chunkId: number) => void;
  }

  let { metadata, answerText, onOpenCitation }: Props = $props();

  let prov = $derived(readAnswerProvenance(metadata, answerText));
  let abstention = $derived(readTypedAbstention(metadata));
  let stacks = $derived(readStageAttribution(metadata));
  let open = $state(false);
  let stacksOpen = $state(false);

  /** One decimal, always — "43.2s" reads as a measurement, "43s" reads as
   *  a rounding, and the numbers this strip exists to expose differ by
   *  tenths (surgery 5.4s vs its fallback 43.2s). */
  const secs = (s: number) => `${s.toFixed(1)}s`;
</script>

{#if abstention}
  <!-- The typed abstention. Read from the gate's own field, not from the
       answer's prose: what the turn DID is a fact the runtime records,
       and a bubble that reads like a refusal is not the same thing. -->
  <div class="typed-abstention" role="note" data-testid="typed-abstention">
    <span class="ta-mark" aria-hidden="true">∅</span>
    <span class="ta-text">
      Nothing asserted — this answer withheld rather than guessed.
    </span>
    {#if abstention.nativeAnswerability !== null}
      <span
        class="ta-score"
        title="How answerable the retrieved passages looked to the grounding instrument. Recorded only — it did not decide this turn."
      >
        answerability {abstention.nativeAnswerability.toFixed(2)}
      </span>
    {/if}
  </div>
{/if}

{#if prov}
  <details
    class="answer-provenance"
    bind:open
    data-testid="answer-provenance"
  >
    <summary>
      <span class="ap-mark" aria-hidden="true">◫</span>
      {#if prov.total === 0}
        Provenance: nothing to segment
      {:else}
        Provenance: {prov.groundedAddressed} of {prov.total} passage{prov.total ===
        1
          ? ""
          : "s"} traced to your sources{#if prov.unverified > 0}
          · {prov.unverified} not found{/if}
      {/if}
    </summary>
    <ul class="ap-rows">
      {#each prov.rows as row, i (i)}
        <li class="ap-row ap-{row.kind}">
          <span class="ap-label">{row.label}</span>
          {#if row.address && onOpenCitation}
            <button
              type="button"
              class="ap-open"
              onclick={() =>
                onOpenCitation(row.address!.corpusId, row.address!.chunkId)}
            >
              open passage
            </button>
          {:else if row.kind === "grounded"}
            <!-- Grounded, but the runtime could not resolve a handle for
                 that pool slot. Said plainly rather than linked to a
                 guess. -->
            <span class="ap-noaddr">no openable address</span>
          {/if}
          <span class="ap-text">{row.text}</span>
        </li>
      {/each}
    </ul>
  </details>
{/if}

{#if stacks}
  <!-- G4 — the stack attribution. The summary line is the "one UI
       element" the order names: it answers "which system served this
       turn, and did the old one eat the time" without opening it. -->
  <details
    class="stack-attribution"
    class:sa-old={stacks.oldStackRan}
    bind:open={stacksOpen}
    data-testid="stack-attribution"
  >
    <summary>
      <span class="sa-mark" aria-hidden="true">◷</span>
      <span class="sa-total">Answered in {secs(stacks.totalSeconds)}</span>
      <span class="sa-served" data-testid="stack-attribution-served">
        {stacks.servedBy}
      </span>
      {#if stacks.oldStackRan}
        <span class="sa-old-cost" data-testid="stack-attribution-old-cost">
          {secs(stacks.oldStackSeconds)} in the old stack
        </span>
      {/if}
    </summary>
    <ul class="sa-rows">
      {#each stacks.rows as row, i (i)}
        <li
          class="sa-row sa-owner-{row.ownerKind}"
          class:sa-residual={row.isResidual}
          data-stage={row.stage}
        >
          <span class="sa-stage">{row.label}</span>
          <span class="sa-ms">{secs(row.seconds)}</span>
          <span class="sa-owner">{row.owner}</span>
          <span class="sa-detail">{row.detail}</span>
          <!-- Proportion of the turn, so a 43s row LOOKS like 43s. The
               bar is decoration over the number, never instead of it. -->
          <span
            class="sa-bar"
            aria-hidden="true"
            style="--sa-share: {(row.share * 100).toFixed(1)}%"
          ></span>
        </li>
      {/each}
    </ul>
    <p class="sa-foot">
      Attributed from what ran, not from what the flags say. Time no stage
      claimed is shown as unattributed rather than hidden — a mechanism
      that runs without a row is a defect in this strip, and that is what
      makes it visible.
    </p>
  </details>
{/if}

<style>
  .typed-abstention {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    margin-top: 0.5rem;
    font-size: 0.8125rem;
    color: var(--text-secondary, #6b7280);
  }
  .ta-mark {
    color: var(--text-tertiary, #9ca3af);
  }
  .ta-score {
    font-variant-numeric: tabular-nums;
    opacity: 0.7;
  }
  .answer-provenance {
    margin-top: 0.5rem;
    font-size: 0.8125rem;
    color: var(--text-secondary, #6b7280);
  }
  .answer-provenance summary {
    cursor: pointer;
    list-style: none;
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
  }
  .ap-mark {
    color: var(--text-tertiary, #9ca3af);
  }
  .ap-rows {
    margin: 0.375rem 0 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }
  .ap-row {
    display: grid;
    grid-template-columns: auto auto 1fr;
    gap: 0.5rem;
    align-items: baseline;
    padding-left: 0.5rem;
    border-left: 2px solid var(--border-subtle, #e5e7eb);
  }
  .ap-row.ap-grounded {
    border-left-color: var(--accent-success, #10b981);
  }
  .ap-row.ap-unverified {
    border-left-color: var(--accent-warning, #f59e0b);
  }
  .ap-label {
    white-space: nowrap;
    opacity: 0.8;
  }
  .ap-text {
    color: var(--text-primary, #111827);
    opacity: 0.85;
  }
  .ap-noaddr {
    opacity: 0.6;
    font-style: italic;
    white-space: nowrap;
  }
  .ap-open {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    color: var(--accent-primary, #2563eb);
    cursor: pointer;
    white-space: nowrap;
  }
  .ap-open:hover {
    text-decoration: underline;
  }

  /* ── G4 stack attribution ─────────────────────────────────────── */
  .stack-attribution {
    margin-top: 0.5rem;
    font-size: 0.8125rem;
    color: var(--text-secondary, #6b7280);
  }
  .stack-attribution summary {
    cursor: pointer;
    list-style: none;
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .sa-mark {
    color: var(--text-tertiary, #9ca3af);
  }
  .sa-total {
    font-variant-numeric: tabular-nums;
  }
  .sa-served {
    opacity: 0.8;
  }
  /* The one glance the order asks for: when the old stack ran, its cost
     is the loudest thing on the line. */
  .stack-attribution.sa-old .sa-served,
  .sa-old-cost {
    color: var(--accent-warning, #b45309);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .sa-rows {
    margin: 0.375rem 0 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .sa-row {
    display: grid;
    grid-template-columns: 7rem 4rem 5.5rem 1fr;
    grid-template-rows: auto auto;
    gap: 0 0.5rem;
    align-items: baseline;
    padding-left: 0.5rem;
    border-left: 2px solid var(--border-subtle, #e5e7eb);
  }
  .sa-row.sa-owner-incumbent {
    border-left-color: var(--accent-warning, #f59e0b);
  }
  .sa-row.sa-owner-native {
    border-left-color: var(--accent-success, #10b981);
  }
  .sa-row.sa-residual {
    border-left-style: dashed;
    opacity: 0.75;
  }
  .sa-stage {
    white-space: nowrap;
    color: var(--text-primary, #111827);
  }
  .sa-ms {
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
  .sa-owner {
    white-space: nowrap;
    opacity: 0.85;
  }
  .sa-row.sa-owner-incumbent .sa-owner {
    color: var(--accent-warning, #b45309);
    font-weight: 600;
  }
  .sa-detail {
    opacity: 0.75;
  }
  .sa-bar {
    grid-column: 1 / -1;
    height: 2px;
    width: var(--sa-share, 0%);
    background: currentColor;
    opacity: 0.35;
    border-radius: 1px;
  }
  .sa-foot {
    margin: 0.5rem 0 0;
    padding-left: 0.5rem;
    font-size: 0.75rem;
    opacity: 0.6;
  }
</style>
