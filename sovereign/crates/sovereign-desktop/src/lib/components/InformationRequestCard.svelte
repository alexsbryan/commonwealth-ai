<!--
  InformationRequestCard — the post-answer "would source X sharpen
  this?" prompt (kind: refinement) and the planned task-blocking step
  card (kind: step_block). One component, two chromes.

  Polish brief (2026-05-25):

  The card historically landed with no transition right after a long
  stream settled — the BAM problem. This rewrite addresses three
  things simultaneously:

  1. **Visual weight.** The five context fields (current understanding,
     gap, why-it-matters, satisfying source, search hints) all rendered
     as equal-weight stacked sections. The card was tall enough that
     the user had to scroll/scan to find the actionable question. The
     redesign elevates `gap` to a focal serif block with a gold
     left-rule; the other four fields fold under a `<details>`
     disclosure ("Context") so they're available but not demanding.

  2. **Entrance choreography.** The card now slides in from below
     (translateY 12px → 0) with opacity 0 → 1, then cascades its
     internal sections with a 60ms stagger. Reverse on dismiss.
     Built on Svelte's `transition:slide` + a local CSS animation
     for the inner cascade. All animations gate behind
     `prefers-reduced-motion: no-preference`.

  3. **Chip → card bridge.** The latest `gap_check_fired` chip in
     NarrationChip.svelte renders in a "bridging" state that draws a
     short gold tether downward; this card's header lines up the
     same gold accent color so the chip and card read as one
     continuous gesture rather than two separate objects. See
     NarrationChip.svelte's `.narration-chip.bridging` rules.

  StepBlock kind (planned task suspension) overrides the header label
  to "Task paused — needs info" + shows `task_title`; refinement uses
  "Information request" + the gold accent only (no task chip).
-->
<script lang="ts">
  import { slide } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import {
    submitInformationResponse,
    submitInformationSearch,
  } from "../api";
  import type {
    InformationRequestPayload,
    SearchAugmentation,
  } from "../types";

  interface Props {
    request: InformationRequestPayload | null;
    onHandled: () => void;
    /** Active conversation id — threaded through to
     *  `submit_information_search` so the runtime's `tool_decision`
     *  write keys against this conversation. The next turn's Tool-
     *  Mastery dossier then surfaces the prior unsuccessful lookup.
     *  `null` falls back to a global write that won't filter into
     *  any single conversation's per-turn dossier. */
    conversationId?: string | null;
    /** Fired the moment the user kicks off a refinement (paste-submit
     *  or search-submit). ChatView uses this to mark the targeted
     *  assistant bubble as `refining: true` so AssistantMessage can
     *  render a "Refining…" overlay until the corresponding
     *  MESSAGE_REFINED event arrives. Optional — leaving it unset
     *  preserves the original no-indicator behaviour. */
    onRefiningStarted?: () => void;
    /** Fired after a successful `submitInformationSearch` so the
     *  ChatView can stash the search provenance on the targeted
     *  message. The post-refine bubble then renders the
     *  "Augmented via web search" footer. */
    onSearchAugmented?: (augmentation: SearchAugmentation) => void;
  }

  let {
    request,
    onHandled,
    conversationId = null,
    onRefiningStarted,
    onSearchAugmented,
  }: Props = $props();

  let pasteValue = $state("");
  let submitting = $state(false);
  // Reflects the loading state of the web-search affordance.
  // Separate from `submitting` because a failed search leaves the
  // card live (user can paste / skip / retry); we want the spinner
  // off and the other buttons re-enabled while the gap text stays.
  let searching = $state(false);
  // Surfaced inline when a search affordance fails (zero results,
  // network error). Cleared on the next interaction.
  let searchError = $state("");
  // Context disclosure starts closed for refinement cards. The gap
  // is the actionable question; the four supporting fields are
  // available on demand. For step_block cards we leave it closed too
  // — task_title in the header carries the immediate "what task is
  // this" signal; deeper context is one click away.
  let contextOpen = $state(false);

  // Has any of the four optional context fields been populated by
  // the producer? When all four are empty, the disclosure renders
  // nothing useful and we hide it entirely. Recomputed reactively
  // so a fresh request with different shape re-evaluates.
  let hasContext = $derived(
    !!request &&
      (request.current_understanding.trim().length > 0 ||
        request.relevance.trim().length > 0 ||
        request.satisfying_source.trim().length > 0 ||
        (request.search_hints && request.search_hints.length > 0)),
  );

  // Count of populated context fields — surfaced as the disclosure
  // badge "Context (3)" so the user knows how much is hiding before
  // they expand. Search hints count as one regardless of array size.
  let contextCount = $derived(
    !request
      ? 0
      : (request.current_understanding.trim() ? 1 : 0) +
        (request.relevance.trim() ? 1 : 0) +
        (request.satisfying_source.trim() ? 1 : 0) +
        (request.search_hints && request.search_hints.length > 0 ? 1 : 0),
  );

  // StepBlock cards carry a task goal — surface in the header.
  // Refinement cards have an empty task_title; the header reads
  // "Information request" instead.
  let isStepBlock = $derived(request?.kind === "step_block");
  let headerLabel = $derived(
    isStepBlock ? "Task paused — needs info" : "Information request",
  );

  async function handleSubmit() {
    if (!request || submitting || !pasteValue.trim()) return;
    submitting = true;
    // Mark the targeted bubble as refining BEFORE the Tauri call so
    // the UI swaps to the "Refining…" state immediately rather than
    // after the round-trip. The runtime's post-stream refinement
    // fires once the channel resolves, so the indicator covers the
    // entire wait.
    onRefiningStarted?.();
    try {
      await submitInformationResponse(request.key, pasteValue.trim());
    } catch (e) {
      console.error("Failed to submit information response:", e);
    }
    submitting = false;
    pasteValue = "";
    onHandled();
  }

  async function handleSkip() {
    if (!request || submitting) return;
    submitting = true;
    try {
      await submitInformationResponse(request.key, null);
    } catch (e) {
      console.error("Failed to submit skip:", e);
    }
    submitting = false;
    pasteValue = "";
    onHandled();
  }

  // Run a web search against the gap text. On success the daemon
  // resolves the same pending channel that `handleSubmit` would
  // resolve, so the runtime gets identical re-synthesis input — no
  // new wire path for the search result. On failure (zero results
  // from a bot-blocked DDG fallback, or a backend error), surface
  // the message inline and leave the card live.
  //
  // The returned `SearchAugmentation` is forwarded to ChatView so
  // the post-refine bubble can render an "Augmented via web search:
  // <query> (N sources)" footer with the source URLs clickable.
  async function handleSearch() {
    if (!request || submitting || searching) return;
    searchError = "";
    searching = true;
    // Same pre-call indicator-on as `handleSubmit` — the bubble
    // shows "Refining…" the moment the user commits to the action.
    onRefiningStarted?.();
    try {
      const augmentation = await submitInformationSearch(
        request.key,
        request.gap,
        conversationId,
      );
      if (augmentation.accepted) {
        onSearchAugmented?.(augmentation);
      }
      // Daemon resolved the pending channel; tear down the card.
      onHandled();
    } catch (e) {
      // Tauri command-handler errors arrive as strings.
      searchError =
        typeof e === "string"
          ? e
          : (e as { message?: string })?.message || "Web search failed";
      console.error("Web search affordance failed:", e);
    }
    searching = false;
  }
</script>

{#if request}
  <!--
    `transition:slide` collapses height + opacity together. `cubicOut`
    matches the chip-arrive curve in NarrationChip.svelte so the
    chip→card morph reads as one motion vocabulary. The inner
    cascade (`.cascade-in` rules) is a separate concern — that
    animates content within the now-mounted card.
  -->
  <div
    class="info-card"
    class:kind-step-block={isStepBlock}
    transition:slide={{ duration: 320, easing: cubicOut }}
  >
    <div class="info-header">
      <span class="header-mark" aria-hidden="true">◈</span>
      <span class="header-label">{headerLabel}</span>
      {#if isStepBlock && request.task_title}
        <span class="task-pill" title="Task goal">{request.task_title}</span>
      {/if}
    </div>

    <!-- Hairline that draws in left→right under the header on mount.
         Pure CSS — no JS measurement needed. The grow animation
         shares the chip→card visual lineage. -->
    <div class="header-rule" aria-hidden="true"></div>

    <section class="focal" data-cascade="1">
      <div class="focal-rule" aria-hidden="true"></div>
      <div class="focal-body">
        <div class="focal-label">
          {isStepBlock ? "What this task needs" : "What would sharpen this"}
        </div>
        <p class="focal-text">{request.gap}</p>
      </div>
    </section>

    {#if hasContext}
      <details
        class="context-disclosure"
        bind:open={contextOpen}
        data-cascade="2"
      >
        <summary>
          <span class="caret" aria-hidden="true">▸</span>
          <span class="context-label">Context</span>
          <span class="context-count">({contextCount})</span>
        </summary>
        <div class="context-body">
          {#if request.current_understanding}
            <div class="ctx-row">
              <div class="ctx-key">Current understanding</div>
              <div class="ctx-val">{request.current_understanding}</div>
            </div>
          {/if}
          {#if request.relevance}
            <div class="ctx-row">
              <div class="ctx-key">Why it matters</div>
              <div class="ctx-val">{request.relevance}</div>
            </div>
          {/if}
          {#if request.satisfying_source}
            <div class="ctx-row">
              <div class="ctx-key">What would satisfy</div>
              <div class="ctx-val">{request.satisfying_source}</div>
            </div>
          {/if}
          {#if request.search_hints && request.search_hints.length > 0}
            <div class="ctx-row">
              <div class="ctx-key">Where to look</div>
              <div class="ctx-val">
                <ul class="hints">
                  {#each request.search_hints as hint}
                    <li>{hint}</li>
                  {/each}
                </ul>
              </div>
            </div>
          {/if}
        </div>
      </details>
    {/if}

    <section class="input-section" data-cascade="3">
      <textarea
        bind:value={pasteValue}
        placeholder="Paste a passage, paragraph, or source here — or use Search the web below."
        rows="4"
        disabled={submitting}
      ></textarea>
    </section>

    {#if searchError}
      <div class="search-error" role="alert">{searchError}</div>
    {/if}

    <div class="info-actions" data-cascade="4">
      <button
        class="btn skip"
        onclick={handleSkip}
        disabled={submitting || searching}
      >
        {isStepBlock ? "Skip step" : "Skip"}
      </button>
      <div class="action-spacer"></div>
      <button
        class="btn search"
        onclick={handleSearch}
        disabled={submitting || searching}
        title="Run a web search using the gap text and feed results back to the agent"
      >
        {#if searching}
          <span class="dot-pulse" aria-hidden="true"></span>
          Searching
        {:else}
          Search the web
        {/if}
      </button>
      <button
        class="btn submit"
        onclick={handleSubmit}
        disabled={submitting || searching || !pasteValue.trim()}
      >
        Submit
      </button>
    </div>
  </div>
{/if}

<style>
  /* ─── Card container ───────────────────────────────────────────
     Lavender-court palette: warm plum surface, gold accent border,
     soft accent wash under the header band. `flex-shrink: 0` is
     load-bearing — see the comment in the prior version of this
     file (kept the constraint, dropped the prose since the rest of
     the card is rebuilt). */
  .info-card {
    background: var(--bg-secondary);
    border: 1px solid color-mix(in srgb, var(--accent) 35%, var(--border-mid));
    border-left: 3px solid var(--accent);
    border-radius: var(--radius-lg);
    margin-bottom: 12px;
    overflow: hidden;
    flex-shrink: 0;
    /* Subtle inner glow — picks up the body's radial gold pool.
       Reads as "lit from within" rather than a flat panel. */
    box-shadow:
      0 1px 0 0 color-mix(in srgb, var(--accent) 8%, transparent) inset,
      0 8px 24px -16px color-mix(in srgb, var(--accent) 30%, transparent);
  }

  /* StepBlock variant: slightly more urgent — solid 3px gold rule,
     stronger header wash. The card is structurally identical to
     the refinement variant; the visual delta says "your task is
     paused" without redesigning the whole layout. */
  .info-card.kind-step-block {
    border-left-color: var(--accent);
    border-left-width: 3px;
  }
  .info-card.kind-step-block .info-header {
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--accent) 14%, transparent) 0%,
      color-mix(in srgb, var(--accent) 6%, transparent) 60%,
      transparent 100%
    );
  }

  /* ─── Header ──────────────────────────────────────────────────
     Reads as a continuation of the bridging chip — same gold
     accent, same uppercase tracked label. The diamond glyph
     persists across both surfaces. */
  .info-header {
    display: flex;
    align-items: center;
    gap: 10px;
    background: color-mix(in srgb, var(--accent) 8%, transparent);
    padding: 11px 16px 10px;
  }
  .header-mark {
    color: var(--accent);
    font-size: 1rem;
    line-height: 1;
    /* Soft glow + slow breath — mirrors the bridging chip's pulse
       but at a lower amplitude so the card doesn't visually shout. */
    text-shadow: 0 0 6px color-mix(in srgb, var(--accent) 45%, transparent);
    animation: glyph-breathe 3.4s ease-in-out infinite;
  }
  .header-label {
    flex: 1;
    font-family: var(--font-sans);
    font-size: 0.74rem;
    font-weight: 600;
    letter-spacing: 0.22em;
    text-transform: uppercase;
    color: var(--accent);
  }

  /* StepBlock task title: a small pill on the header showing the
     blocked task's goal text. Only renders when task_title is non-
     empty. Truncates with ellipsis on overflow rather than wrapping
     so the header height stays predictable. */
  .task-pill {
    max-width: 50%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    padding: 3px 10px;
    font-family: var(--font-sans);
    font-size: 0.72rem;
    font-weight: 500;
    letter-spacing: 0.04em;
    color: var(--lavender-light);
    background: var(--lavender-dim);
    border: 1px solid color-mix(in srgb, var(--lavender) 35%, transparent);
    border-radius: 999px;
  }

  /* Hairline that draws under the header on mount — the closing
     gesture of the chip→card morph. Starts at 0% width, grows to
     100%. 1px tall, sits flush against the next section. */
  .header-rule {
    height: 1px;
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--accent) 60%, transparent) 0%,
      color-mix(in srgb, var(--accent) 30%, transparent) 60%,
      transparent 100%
    );
    transform-origin: left center;
    animation: rule-draw 420ms cubic-bezier(0.2, 0.7, 0.2, 1) 160ms backwards;
  }

  /* ─── Focal block ─────────────────────────────────────────────
     The actionable question. Source Serif 4 + opsz 14 + slightly
     reduced weight = "settled literary prose" per app.css doctrine.
     Gold left-rule, generous padding. This is the only section
     designed to demand visual weight. */
  .focal {
    display: flex;
    gap: 14px;
    padding: 16px 18px 14px;
  }
  .focal-rule {
    flex: 0 0 2px;
    background: linear-gradient(
      to bottom,
      var(--accent) 0%,
      color-mix(in srgb, var(--accent) 30%, transparent) 100%
    );
    border-radius: 2px;
    align-self: stretch;
  }
  .focal-body {
    flex: 1;
    min-width: 0;
  }
  .focal-label {
    font-family: var(--font-sans);
    font-size: 0.68rem;
    font-weight: 600;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--text-muted);
    margin-bottom: 6px;
  }
  .focal-text {
    margin: 0;
    font-family: var(--font-serif);
    /* opsz 14 + weight 380 = settled literary register; matches
       prose body styling so the question reads as a continuation
       of the assistant's voice, not a system dialog string. */
    font-variation-settings: "opsz" 14;
    font-weight: 420;
    font-feature-settings: "kern", "liga", "calt";
    font-size: 1.02rem;
    line-height: 1.55;
    color: var(--text-primary);
    /* Subtle bias toward a hanging-punctuation feel — the question
       sits in the gold-ruled column like a pull quote. */
    text-wrap: pretty;
  }

  /* ─── Context disclosure ──────────────────────────────────────
     Native <details> for keyboard + screen-reader semantics. The
     caret rotates on open; the body slides in via max-height
     animation (CSS-only, no JS). */
  .context-disclosure {
    border-top: 1px dashed var(--border);
    border-bottom: 1px dashed var(--border);
    background: color-mix(in srgb, var(--bg-primary) 50%, transparent);
  }
  .context-disclosure summary {
    list-style: none;
    cursor: pointer;
    padding: 10px 18px;
    display: flex;
    align-items: center;
    gap: 8px;
    font-family: var(--font-sans);
    font-size: 0.78rem;
    color: var(--text-secondary);
    user-select: none;
    transition: color 160ms ease, background 160ms ease;
  }
  .context-disclosure summary::-webkit-details-marker {
    display: none;
  }
  .context-disclosure summary:hover {
    color: var(--text-primary);
    background: color-mix(in srgb, var(--lavender) 5%, transparent);
  }
  .caret {
    display: inline-block;
    color: var(--text-muted);
    font-size: 0.8rem;
    line-height: 1;
    transition: transform 220ms cubic-bezier(0.2, 0.7, 0.2, 1),
                color 160ms ease;
    transform-origin: 30% 50%;
  }
  .context-disclosure[open] .caret {
    transform: rotate(90deg);
    color: var(--accent);
  }
  .context-label {
    font-weight: 500;
    letter-spacing: 0.02em;
  }
  .context-count {
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 0.72rem;
    font-weight: 400;
  }

  /* Disclosure body: 2-column rows (label · value). Tighter than
     the original stacked layout — same content, ~45% less height. */
  .context-body {
    padding: 4px 18px 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    /* Native <details> doesn't animate height. We simulate the open
       by animating opacity + a small translateY on the body. Pure
       CSS — `[open]` selector toggles instantly, then the animation
       runs forward. Close is instant (no exit animation); the
       overall card slide-out handles dismissal motion. */
    animation: context-reveal 280ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }
  .ctx-row {
    display: grid;
    grid-template-columns: 130px 1fr;
    gap: 14px;
    align-items: baseline;
  }
  .ctx-key {
    font-family: var(--font-sans);
    font-size: 0.7rem;
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--text-muted);
    line-height: 1.5;
  }
  .ctx-val {
    font-family: var(--font-serif);
    font-variation-settings: "opsz" 14;
    font-size: 0.9rem;
    line-height: 1.55;
    color: var(--text-secondary);
  }
  .hints {
    margin: 0;
    padding-left: 18px;
    color: var(--text-secondary);
    font-size: 0.88rem;
  }
  .hints li {
    margin: 2px 0;
  }

  /* ─── Input section ───────────────────────────────────────────
     The textarea sits in its own padded section so the visual
     rhythm reads as: focal question → context (folded) → input →
     actions. Each pane has clear function. */
  .input-section {
    padding: 12px 18px 4px;
  }
  .input-section textarea {
    width: 100%;
    padding: 11px 13px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-family: var(--font-sans);
    font-size: 0.9rem;
    line-height: 1.55;
    resize: vertical;
    outline: none;
    transition: border-color 180ms ease, box-shadow 180ms ease,
                background 180ms ease;
  }
  .input-section textarea::placeholder {
    color: var(--text-muted);
    font-style: italic;
  }
  .input-section textarea:focus {
    border-color: color-mix(in srgb, var(--accent) 70%, var(--border));
    background: color-mix(in srgb, var(--accent) 2%, var(--bg-input));
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 12%, transparent);
  }

  /* ─── Actions ─────────────────────────────────────────────────
     Skip is left-anchored (low-commitment, easy walk-away).
     Search + Submit are right-anchored (active choices). Spacer
     between pushes them apart so Submit isn't adjacent to Skip
     — accidental click protection. */
  .info-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 18px 14px;
    background: color-mix(in srgb, var(--bg-primary) 35%, transparent);
    border-top: 1px solid var(--border);
  }
  .action-spacer {
    flex: 1;
  }

  .btn {
    padding: 7px 16px;
    border-radius: var(--radius);
    font-family: var(--font-sans);
    font-weight: 500;
    font-size: 0.86rem;
    letter-spacing: 0.01em;
    border: 1px solid transparent;
    cursor: pointer;
    transition: background 180ms ease, border-color 180ms ease,
                color 180ms ease, transform 120ms ease;
  }
  .btn:active:not(:disabled) {
    transform: translateY(1px);
  }
  .btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .skip {
    background: transparent;
    border-color: var(--border-mid);
    color: var(--text-muted);
  }
  .skip:hover:not(:disabled) {
    color: var(--text-secondary);
    border-color: var(--border-bright);
    background: color-mix(in srgb, var(--bg-elevated) 60%, transparent);
  }

  .search {
    background: transparent;
    border-color: color-mix(in srgb, var(--accent) 55%, transparent);
    color: var(--accent);
    display: inline-flex;
    align-items: center;
    gap: 7px;
  }
  .search:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    border-color: var(--accent);
  }

  .submit {
    background: var(--accent);
    color: var(--bg-primary);
    border-color: var(--accent);
    /* Subtle inner highlight — picks up the gold-on-gold reading
       from app.css's lower-left candlelight pool. */
    box-shadow: 0 1px 0 0 color-mix(in srgb, white 14%, transparent) inset;
  }
  .submit:hover:not(:disabled) {
    background: var(--accent-light);
    border-color: var(--accent-light);
  }

  /* In-flight search indicator: a 3-dot pulse that matches the
     chat composer's typing-indicator vocabulary, scaled down. */
  .dot-pulse {
    display: inline-block;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
    animation: dot-pulse 1.1s ease-in-out infinite;
  }

  .search-error {
    margin: 0 18px 8px;
    padding: 8px 12px;
    background: color-mix(in srgb, var(--error) 6%, transparent);
    border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
    border-radius: var(--radius);
    color: var(--text-secondary);
    font-family: var(--font-sans);
    font-size: 0.8rem;
    line-height: 1.45;
  }

  /* ─── Cascading entrance — inner content ──────────────────────
     The container's `transition:slide` handles the outer collapse
     gesture. Once mounted, each top-level child with
     `data-cascade="N"` fades+rises with a stagger derived from N.
     `backwards` fill so the starting opacity is honoured before
     the delay elapses (avoids a single frame of fully-opaque content
     on slow systems). */
  [data-cascade] {
    animation: cascade-in 360ms cubic-bezier(0.2, 0.7, 0.2, 1) backwards;
  }
  [data-cascade="1"] { animation-delay: 120ms; }
  [data-cascade="2"] { animation-delay: 200ms; }
  [data-cascade="3"] { animation-delay: 260ms; }
  [data-cascade="4"] { animation-delay: 320ms; }

  /* ─── Animations ──────────────────────────────────────────── */
  @keyframes cascade-in {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
  @keyframes rule-draw {
    from { transform: scaleX(0); }
    to   { transform: scaleX(1); }
  }
  @keyframes glyph-breathe {
    0%, 100% {
      opacity: 0.85;
      text-shadow: 0 0 4px color-mix(in srgb, var(--accent) 30%, transparent);
    }
    50% {
      opacity: 1;
      text-shadow: 0 0 8px color-mix(in srgb, var(--accent) 55%, transparent);
    }
  }
  @keyframes context-reveal {
    from {
      opacity: 0;
      transform: translateY(-2px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
  @keyframes dot-pulse {
    0%, 100% { opacity: 0.3; transform: scale(0.85); }
    50%      { opacity: 1;   transform: scale(1); }
  }

  @media (prefers-reduced-motion: reduce) {
    .info-card,
    [data-cascade],
    .header-rule,
    .header-mark,
    .context-body,
    .dot-pulse,
    .caret {
      animation: none !important;
      transition: none !important;
    }
    .header-rule {
      transform: scaleX(1);
    }
  }
</style>
