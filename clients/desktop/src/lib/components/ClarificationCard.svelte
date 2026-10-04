<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  ClarificationCard — rendered on low-confidence Ask turns. The
  runtime suppresses synthesis and waits for the user to pick an
  option or type a freeform answer. On submit, dispatches
  `CLARIFICATION_SUBMIT` to the routing FSM, which invokes
  `resume_session` via the Tauri command (skips re-classification
  and streams the follow-up through the chosen intent).

  Data source: `routingStore.clarification`. Component never calls
  `invoke()` directly — data flow is unidirectional via the FSM.
-->
<script lang="ts">
  import { routingStore } from "../stores/routing.svelte";
  import {
    type ClarificationOption,
    MAX_TURN_MESSAGE_CHARS,
    OVERSIZE_MESSAGE_HINT,
  } from "../types";

  let clarification = $derived(routingStore.clarification);
  let freeformText = $state("");

  // PR2e — the free-text box can swallow arbitrarily long paste
  // (including 20-page documents). Block submit + surface a hint
  // pointing at the attached-file flow. Backend enforces the same
  // cap; this keeps the request from ever firing.
  let freeformIsOversized = $derived(
    freeformText.length > MAX_TURN_MESSAGE_CHARS,
  );

  // PR6 — recognise cancel-intent freeform so the user typing
  // "nevermind" doesn't get taken literally as a follow-up query
  // (which spins up a full Primary-slot turn against the previous
  // context). Treat as dismiss instead. List is deliberately small
  // so legitimate questions that happen to contain the word
  // "cancel" still submit — the whole input must be a cancel phrase.
  const CANCEL_PHRASES = [
    "nevermind",
    "never mind",
    "cancel",
    "stop",
    "skip",
    "forget it",
    "n/a",
    "disregard",
  ];
  function isCancelIntent(text: string): boolean {
    const t = text.trim().toLowerCase().replace(/[.!?]+$/, "");
    return CANCEL_PHRASES.includes(t);
  }

  function dismiss() {
    routingStore.send({ type: "DISMISS_CLARIFICATION" });
    freeformText = "";
  }

  function handleOption(option: ClarificationOption) {
    const c = routingStore.clarification;
    if (!c) return;
    routingStore.send({
      type: "CLARIFICATION_SUBMIT",
      sessionId: c.session_id,
      conversationId: c.conversation_id,
      followUp: option.follow_up,
      intentHint: option.intent_hint,
    });
    freeformText = "";
  }

  function handleFreeform() {
    const c = routingStore.clarification;
    if (!c || !freeformText.trim() || freeformIsOversized) return;
    // PR6 — "nevermind" / "cancel" / etc. short-circuits to dismiss
    // so users can cleanly walk away without spinning up a turn.
    if (isCancelIntent(freeformText)) {
      dismiss();
      return;
    }
    // Freeform input reuses the most-specific intent_hint the
    // classifier surfaced, defaulting to `deep_query` if none.
    // (When the user types freeform they've opted out of the
    // keyword heuristics; DeepQuery is the conservative catch-all.)
    const intentHint = c.options[0]?.intent_hint ?? "deep_query";
    routingStore.send({
      type: "CLARIFICATION_SUBMIT",
      sessionId: c.session_id,
      conversationId: c.conversation_id,
      followUp: freeformText.trim(),
      intentHint,
    });
    freeformText = "";
  }
</script>

{#if clarification}
  <div class="clarification-card" data-testid="clarification-card">
    <div class="clar-header">
      <span class="header-mark">?</span>
      <span class="header-label">Quick clarification</span>
      <button
        class="dismiss-btn"
        type="button"
        onclick={dismiss}
        title="Dismiss — don't answer this question"
        aria-label="Dismiss clarification"
      >
        ×
      </button>
    </div>

    <p class="clar-question">
      {clarification.question}
    </p>

    {#if clarification.options.length > 0}
      <div class="option-chips">
        {#each clarification.options as opt}
          <button
            class="option-chip"
            onclick={() => handleOption(opt)}
            title={opt.follow_up}
          >
            {opt.label}
          </button>
        {/each}
        <!-- PR6: explicit bail-out chip, styled muted so it doesn't
             compete with the substantive options. -->
        <button
          class="option-chip dismiss-chip"
          onclick={dismiss}
          title="Dismiss the clarification without starting a new turn"
        >
          Never mind
        </button>
      </div>
    {/if}

    <form
      class="freeform-row"
      onsubmit={(e) => {
        e.preventDefault();
        handleFreeform();
      }}
    >
      <input
        type="text"
        class="freeform-input"
        bind:value={freeformText}
        placeholder="Or say it in your own words…"
      />
      <button
        class="freeform-submit"
        type="submit"
        disabled={!freeformText.trim() || freeformIsOversized}
        title={freeformIsOversized ? OVERSIZE_MESSAGE_HINT : ""}
      >
        Send
      </button>
    </form>
    {#if freeformIsOversized}
      <div class="oversize-hint" role="status">
        {OVERSIZE_MESSAGE_HINT}
      </div>
    {/if}
  </div>
{/if}

<style>
  .clarification-card {
    background: var(--bg-secondary);
    border: 1px solid var(--accent);
    border-left: 3px solid var(--accent);
    border-radius: var(--radius-lg);
    margin-bottom: 10px;
    overflow: hidden;
    flex-shrink: 0;
  }

  .clar-header {
    display: flex;
    align-items: center;
    gap: 8px;
    background: rgba(201, 168, 76, 0.08);
    padding: 10px 16px;
    border-bottom: 1px solid var(--border);
  }
  .header-mark {
    color: var(--accent);
    font-size: 1rem;
    font-weight: 700;
    line-height: 1;
  }
  .header-label {
    font-size: 0.76rem;
    font-weight: 700;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--accent);
    flex: 1;
  }

  .dismiss-btn {
    width: 22px;
    height: 22px;
    padding: 0;
    line-height: 1;
    font-size: 1.1rem;
    color: var(--text-muted);
    background: transparent;
    border: 1px solid var(--border-mid);
    border-radius: 50%;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    transition: color 0.15s, border-color 0.15s, background 0.15s;
  }
  .dismiss-btn:hover {
    color: var(--text-primary);
    border-color: var(--text-primary);
    background: var(--bg-primary);
  }

  .clar-question {
    margin: 0;
    padding: 12px 16px;
    font-family: var(--font-serif, Georgia, serif);
    font-size: 0.95rem;
    line-height: 1.5;
    color: var(--text-primary, var(--text-secondary));
  }

  .option-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 6px 16px 10px;
  }
  .option-chip {
    padding: 6px 12px;
    font-size: 0.85rem;
    background: var(--bg-primary);
    color: var(--text-secondary);
    border: 1px solid var(--border-mid);
    border-radius: 12px;
    cursor: pointer;
    transition: background 0.15s, color 0.15s, border-color 0.15s;
  }
  .option-chip:hover {
    background: var(--accent);
    color: var(--bg-primary);
    border-color: var(--accent);
  }

  .option-chip.dismiss-chip {
    color: var(--text-muted);
    border-style: dashed;
  }
  .option-chip.dismiss-chip:hover {
    background: transparent;
    color: var(--text-primary);
    border-color: var(--text-primary);
    border-style: solid;
  }

  .freeform-row {
    display: flex;
    gap: 6px;
    padding: 10px 16px 14px;
    border-top: 1px dashed var(--border);
  }
  .freeform-input {
    flex: 1;
    padding: 7px 10px;
    font-size: 0.9rem;
    font-family: var(--font-sans);
    background: var(--bg-input, var(--bg-primary));
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    outline: none;
  }
  .freeform-input:focus {
    border-color: var(--accent);
  }
  .freeform-submit {
    padding: 7px 14px;
    font-size: 0.85rem;
    font-weight: 500;
    background: var(--accent);
    color: var(--bg-primary);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    cursor: pointer;
  }
  .freeform-submit:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .oversize-hint {
    margin: 0 16px 12px;
    padding: 8px 12px;
    background: rgba(201, 168, 76, 0.08);
    border: 1px dashed var(--accent);
    border-radius: var(--radius);
    font-size: 0.78rem;
    color: var(--text-secondary);
    line-height: 1.45;
  }
</style>
