<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  InterpretationBanner — rendered above the streaming assistant
  message on moderate-confidence Propose turns. Shows the router's
  reading of the message and a set of redirect chips drawn from
  `RouterClassification.alternatives`.

  Data source: `routingStore.proposed` (reactive via Svelte 5 $state
  under the hood of the singleton). The component never calls
  `invoke()` directly — it dispatches `REDIRECT_SUBMIT` to the FSM
  which invokes the Tauri command as an XState actor.

  Dismiss behaviour: the banner stays visible through the turn.
  `chat.machine.ts` fires `DISMISS_PROPOSED` 30s after
  `MESSAGE_COMPLETE` to GC the banner so a next turn starts clean.
-->
<script lang="ts">
  import { routingStore } from "../stores/routing.svelte";
  import type { ProposedAlternative } from "../types";

  let proposed = $derived(routingStore.proposed);

  function handleRedirect(alt: ProposedAlternative) {
    const p = routingStore.proposed;
    if (!p) return;
    // PR2c — cancel the in-flight sampler AND start a new stream
    // against the chosen alternative. The `message-chunk` /
    // `message-complete` listener in ChatView.svelte already
    // handles arbitrary message_ids, so the new bubble appears
    // beneath whatever partial content the cancelled stream left.
    routingStore.send({
      type: "REDIRECT_SUBMIT",
      sessionId: p.session_id,
      intentHint: alt.intent_hint,
    });
  }

  function tierBadge(confidence: number): string {
    if (confidence >= 0.8) return "high";
    if (confidence >= 0.55) return "moderate";
    return "low";
  }
</script>

{#if proposed}
  <div class="interpretation-banner" data-testid="interpretation-banner">
    <div class="banner-header">
      <span class="header-mark">◈</span>
      <span class="header-label">Proposed interpretation</span>
      <span
        class="confidence-chip"
        data-tier={tierBadge(proposed.confidence)}
        title="Confidence tier"
      >
        {Math.round(proposed.confidence * 100)}%
      </span>
    </div>

    <p class="interpretation-text">
      {proposed.interpretation}
    </p>

    {#if proposed.alternatives.length > 0}
      <div class="alternatives">
        <span class="alt-label">Or redirect to:</span>
        <div class="alt-chips">
          {#each proposed.alternatives as alt}
            <button
              class="alt-chip"
              onclick={() => handleRedirect(alt)}
              title="Cancel current answer and redirect"
            >
              {alt.label}
            </button>
          {/each}
        </div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .interpretation-banner {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-left: 3px solid var(--accent);
    border-radius: var(--radius-lg);
    margin-bottom: 8px;
    overflow: hidden;
    flex-shrink: 0;
    font-size: 0.88rem;
  }

  .banner-header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px;
    border-bottom: 1px dashed var(--border);
  }
  .header-mark {
    color: var(--accent);
    font-size: 0.9rem;
    line-height: 1;
  }
  .header-label {
    font-size: 0.7rem;
    font-weight: 700;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--accent);
    flex: 1;
  }
  .confidence-chip {
    font-size: 0.7rem;
    font-weight: 600;
    padding: 2px 8px;
    border-radius: 10px;
    border: 1px solid var(--border-mid);
    color: var(--text-muted);
    background: var(--bg-primary);
  }
  .confidence-chip[data-tier="high"] {
    color: var(--success, #3b8e3b);
    border-color: var(--success, #3b8e3b);
  }
  .confidence-chip[data-tier="moderate"] {
    color: var(--accent);
    border-color: var(--accent);
  }
  .confidence-chip[data-tier="low"] {
    color: var(--text-muted);
    border-color: var(--border-mid);
  }

  .interpretation-text {
    margin: 0;
    padding: 10px 14px;
    font-family: var(--font-serif, Georgia, serif);
    line-height: 1.55;
    color: var(--text-primary, var(--text-secondary));
  }

  .alternatives {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 14px 10px;
    border-top: 1px dashed var(--border);
  }
  .alt-label {
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .alt-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .alt-chip {
    padding: 5px 10px;
    font-size: 0.82rem;
    background: var(--bg-primary);
    color: var(--text-secondary);
    border: 1px solid var(--border-mid);
    border-radius: 12px;
    cursor: pointer;
    transition: background 0.15s, color 0.15s, border-color 0.15s;
  }
  .alt-chip:hover {
    background: var(--accent);
    color: var(--bg-primary);
    border-color: var(--accent);
  }
</style>
