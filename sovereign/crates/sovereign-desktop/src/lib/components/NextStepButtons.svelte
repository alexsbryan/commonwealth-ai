<!--
  NextStepButtons — clickable grounded follow-up chips rendered
  under a completed KnowledgeQuery assistant message. The runtime
  populates `metadata.next_steps` with offers keyed on the session
  that just completed; within the 30s session retention window a
  click resumes that session and skips router classification.

  Wiring contract (PR3):
    - Component reads `offers: NextStepOffer[]` as a prop (bubble
      gets to decide whether to render at all — e.g. hides on
      redirected-away bubbles).
    - Click dispatches `NEXT_STEP_SELECTED` to chat.machine via the
      `onselect` callback. chat.machine's actor handles the choice
      of `resumeSession` vs `sendMessageStream` based on whether
      `session_ref` is live.
    - No `invoke()` calls from this component — the FSM owns the
      Tauri side, just like ClarificationCard / InterpretationBanner.
-->
<script lang="ts">
  import type { NextStepOffer } from "../types";

  interface Props {
    offers: NextStepOffer[];
    onselect: (offer: NextStepOffer) => void;
  }

  let { offers, onselect }: Props = $props();
</script>

{#if offers.length > 0}
  <div class="next-steps" data-testid="next-steps">
    <span class="label">Follow up:</span>
    <div class="offer-row">
      {#each offers as offer}
        <button
          class="offer-chip"
          onclick={() => onselect(offer)}
          title={offer.description ?? offer.follow_up_query}
        >
          {offer.label}
        </button>
      {/each}
    </div>
  </div>
{/if}

<style>
  .next-steps {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-top: 10px;
    padding-top: 8px;
    border-top: 1px dashed var(--border);
  }

  .label {
    font-size: 0.7rem;
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--text-muted);
  }

  .offer-row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .offer-chip {
    padding: 5px 12px;
    font-size: 0.82rem;
    color: var(--text-secondary);
    background: var(--bg-primary);
    border: 1px solid var(--border-mid);
    border-radius: 12px;
    cursor: pointer;
    transition: background 0.15s, color 0.15s, border-color 0.15s;
  }

  .offer-chip:hover {
    background: var(--accent);
    color: var(--bg-primary);
    border-color: var(--accent);
  }
</style>
