<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  LessonCard — the TEACHABLE "Learn this?" consent card
  (TEACHABLE.md §4). Chrome is cloned from InformationRequestCard so
  the lesson_drafted chip → card gesture reads as the same motion
  vocabulary as gap_check_fired → info card: ◈ header, gold accent,
  focal serif sentence, cascading entrance, reduced-motion care.

  Structural differences from the sibling, on purpose:
  - NO paste textarea, NO search affordance, NO pending backend
    channel. The payload already carries the full draft; Save calls
    `saveLesson`, and "Not this" calls NOTHING — dismissals are never
    stored (§4), so walking away leaves zero residue.
  - Edit is inline: the focal sentence swaps for a serif-styled
    textarea (editing the sentence, not filling a form). An edited
    save also sends the pre-edit sentence as `drafted_display` — the
    consented correction pair (§11). For prompt-rung lessons the
    edited sentence becomes the prompt_form too; param/transform
    lessons keep their compiled machinery untouched.
  - `{#key proposal?.id}` re-mounts the body when a new proposal
    overwrites a pending one (last-write-wins in the machine), which
    declaratively resets edit state and replays the entrance — a new
    proposal should re-announce itself.
-->
<script lang="ts">
  import { slide } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { saveLesson } from "../api";
  import type { LessonProposedPayload } from "../types";

  interface Props {
    proposal: LessonProposedPayload | null;
    /** Fired on Save success and on "Not this" — the machine clears
     *  the pending proposal either way. */
    onHandled: () => void;
  }

  let { proposal, onHandled }: Props = $props();

  let editing = $state(false);
  let editValue = $state("");
  let saving = $state(false);
  let saveError = $state("");

  function toggleEdit() {
    if (saving) return;
    editing = !editing;
    if (editing) editValue = proposal?.display ?? "";
    saveError = "";
  }

  function handleNotThis() {
    if (saving) return;
    // Pure dismissal: no backend call, nothing stored anywhere.
    onHandled();
  }

  async function handleSave() {
    if (!proposal || saving) return;
    const display = editing ? editValue.trim() : proposal.display;
    if (!display) return;
    const edited = editing && display !== proposal.display;
    saving = true;
    saveError = "";
    try {
      await saveLesson(
        {
          ...proposal,
          display,
          // Only an EDITED sentence may overwrite the compiled
          // prompt_form, and only on the prompt rung — the unedited
          // path and param/transform lessons pass it through intact.
          prompt_form:
            edited && proposal.enforcement === "prompt"
              ? display
              : proposal.prompt_form,
        },
        edited ? proposal.display : null,
      );
      onHandled();
    } catch (e) {
      saveError =
        typeof e === "string"
          ? e
          : (e as { message?: string })?.message || "Couldn't save that";
      saving = false;
      return; // card stays live so the user can retry or dismiss
    }
    saving = false;
  }
</script>

{#if proposal}
  <div
    class="lesson-card"
    transition:slide={{ duration: 320, easing: cubicOut }}
  >
    {#key proposal.id}
      <div class="lesson-header">
        <span class="header-mark" aria-hidden="true">◈</span>
        <span class="header-label">Learn this?</span>
      </div>
      <div class="header-rule" aria-hidden="true"></div>

      <section class="focal" data-cascade="1">
        <div class="focal-rule" aria-hidden="true"></div>
        <div class="focal-body">
          <div class="focal-label">What I'll keep in mind</div>
          {#if editing}
            <!-- svelte-ignore a11y_autofocus -->
            <textarea
              class="edit-textarea"
              bind:value={editValue}
              rows="2"
              autofocus
              disabled={saving}
            ></textarea>
          {:else}
            <p class="focal-text">{proposal.display}</p>
          {/if}
        </div>
      </section>

      {#if saveError}
        <div class="save-error" role="alert">{saveError}</div>
      {/if}

      <div class="lesson-actions" data-cascade="2">
        <button class="btn skip" onclick={handleNotThis} disabled={saving}>
          Not this
        </button>
        <div class="action-spacer"></div>
        <button class="btn edit" onclick={toggleEdit} disabled={saving}>
          {editing ? "Cancel edit" : "Edit"}
        </button>
        <button
          class="btn submit"
          onclick={handleSave}
          disabled={saving || (editing && !editValue.trim())}
        >
          {saving ? "Saving…" : "Save"}
        </button>
      </div>
    {/key}
  </div>
{/if}

<style>
  /* Container — same lavender-court chrome as InformationRequestCard
     so chip → card reads as one gesture. */
  .lesson-card {
    background: var(--bg-secondary);
    border: 1px solid color-mix(in srgb, var(--accent) 35%, var(--border-mid));
    border-left: 3px solid var(--accent);
    border-radius: var(--radius-lg);
    margin-bottom: 12px;
    overflow: hidden;
    flex-shrink: 0;
    box-shadow:
      0 1px 0 0 color-mix(in srgb, var(--accent) 8%, transparent) inset,
      0 8px 24px -16px color-mix(in srgb, var(--accent) 30%, transparent);
  }

  .lesson-header {
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
    font-variation-settings: "opsz" 14;
    font-weight: 420;
    font-feature-settings: "kern", "liga", "calt";
    font-size: 1.02rem;
    line-height: 1.55;
    color: var(--text-primary);
    text-wrap: pretty;
  }

  /* Inline edit — reads as editing the sentence itself: same serif
     register as the focal text, not a sans form field. */
  .edit-textarea {
    width: 100%;
    padding: 8px 10px;
    background: var(--bg-input);
    border: 1px solid color-mix(in srgb, var(--accent) 55%, var(--border));
    border-radius: var(--radius);
    color: var(--text-primary);
    font-family: var(--font-serif);
    font-variation-settings: "opsz" 14;
    font-size: 1.02rem;
    line-height: 1.55;
    resize: vertical;
    outline: none;
  }
  .edit-textarea:focus {
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 12%, transparent);
  }

  .save-error {
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

  /* Actions — "Not this" left-anchored (easy walk-away), Edit + Save
     right-anchored, spacer for accidental-click protection. */
  .lesson-actions {
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

  .edit {
    background: transparent;
    border-color: color-mix(in srgb, var(--accent) 55%, transparent);
    color: var(--accent);
  }
  .edit:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    border-color: var(--accent);
  }

  .submit {
    background: var(--accent);
    color: var(--bg-primary);
    border-color: var(--accent);
    box-shadow: 0 1px 0 0 color-mix(in srgb, white 14%, transparent) inset;
  }
  .submit:hover:not(:disabled) {
    background: var(--accent-light);
    border-color: var(--accent-light);
  }

  [data-cascade] {
    animation: cascade-in 360ms cubic-bezier(0.2, 0.7, 0.2, 1) backwards;
  }
  [data-cascade="1"] { animation-delay: 120ms; }
  [data-cascade="2"] { animation-delay: 200ms; }

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

  @media (prefers-reduced-motion: reduce) {
    .lesson-card,
    [data-cascade],
    .header-rule,
    .header-mark {
      animation: none !important;
      transition: none !important;
    }
    .header-rule {
      transform: scaleX(1);
    }
  }
</style>
