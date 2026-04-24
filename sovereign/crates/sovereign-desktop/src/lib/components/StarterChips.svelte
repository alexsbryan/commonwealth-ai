<!--
  StarterChips — compact chip row surfacing atlas-mined starter
  questions. Clicking a chip fires `onPick(question)` so the caller
  can pre-fill + submit the chat input, deep-link, or advance an
  onboarding state machine.

  Pure presentation — no Tauri calls. Callers fetch via
  `enrichGetStarterQuestions` and pass the array in.
-->
<script lang="ts">
  import type { StarterQuestion } from "../types";

  interface Props {
    questions: StarterQuestion[];
    onPick: (question: StarterQuestion) => void;
    /// Optional heading rendered above the chips. Default: omitted.
    heading?: string;
    /// Fine-print rendered under the heading. Default: omitted.
    subheading?: string;
    /// Visually de-emphasise the chips (used when we know the atlas
    /// is not yet built and we're showing excerpt-derived fallbacks).
    muted?: boolean;
  }

  let { questions, onPick, heading, subheading, muted = false }: Props = $props();
</script>

{#if questions.length > 0}
  <section class="starters" class:muted>
    {#if heading}
      <p class="starters-heading">{heading}</p>
    {/if}
    {#if subheading}
      <p class="starters-sub">{subheading}</p>
    {/if}
    <ul class="chip-row">
      {#each questions as q (q.atom_id)}
        <li>
          <button
            type="button"
            class="chip"
            onclick={() => onPick(q)}
            title={q.source_section
              ? `From ${q.source_section} · ${q.question_type}`
              : q.question_type}
          >
            {q.text}
          </button>
        </li>
      {/each}
    </ul>
  </section>
{/if}

<style>
  .starters {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .starters.muted {
    opacity: 0.78;
  }
  .starters-heading {
    margin: 0;
    font-size: 0.82em;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-secondary, var(--text-primary));
  }
  .starters-sub {
    margin: 0 0 4px;
    font-size: 0.82em;
    color: var(--text-muted, var(--text-secondary));
    line-height: 1.4;
  }
  .chip-row {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    background: transparent;
    border: 1px solid var(--border, #333);
    color: var(--text-primary, #eee);
    padding: 6px 12px;
    border-radius: 999px;
    font-size: 0.88em;
    line-height: 1.3;
    text-align: left;
    cursor: pointer;
    max-width: 540px;
    transition: background 160ms ease, border-color 160ms ease;
  }
  .chip:hover,
  .chip:focus-visible {
    background: color-mix(in oklab, var(--accent, #c4a46a) 10%, transparent);
    border-color: var(--accent, #c4a46a);
    outline: none;
  }
  .starters.muted .chip {
    color: var(--text-secondary, var(--text-primary));
    border-style: dashed;
  }
</style>
