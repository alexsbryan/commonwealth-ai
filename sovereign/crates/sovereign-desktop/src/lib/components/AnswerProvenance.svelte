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
-->
<script lang="ts">
  import { readAnswerProvenance, readTypedAbstention } from "./answerProvenance";

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
  let open = $state(false);
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
</style>
