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

  // `atom_id` is only unique WITHIN a corpus's atlas — every atlas
  // restarts numbering from `question-0001`, so a list merged from
  // multiple corpora collides on the bare id. Callers that round-
  // robin across corpora SHOULD pass `corpus_id` per item; we then
  // key by `${corpus_id}:${atom_id}`. Single-corpus callers can
  // omit it and rely on `atom_id` alone.
  type StarterChipQuestion = StarterQuestion & { corpus_id?: string };

  interface Props {
    questions: StarterChipQuestion[];
    onPick: (question: StarterChipQuestion) => void;
    /// Optional heading rendered above the chips. Default: omitted.
    heading?: string;
    /// Fine-print rendered under the heading. Default: omitted.
    subheading?: string;
    /// Visually de-emphasise the chips (used when we know the atlas
    /// is not yet built and we're showing excerpt-derived fallbacks).
    muted?: boolean;
  }

  let { questions, onPick, heading, subheading, muted = false }: Props = $props();

  function keyOf(q: StarterChipQuestion): string {
    return q.corpus_id ? `${q.corpus_id}:${q.atom_id}` : q.atom_id;
  }
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
      {#each questions as q (keyOf(q))}
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
    gap: 10px;
  }
  .starters.muted {
    opacity: 0.78;
  }
  .starters-heading {
    margin: 0;
    font-size: 0.78em;
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--text-secondary, var(--text-primary));
  }
  .starters-sub {
    margin: 0 0 4px;
    font-size: 0.82em;
    color: var(--text-muted, var(--text-secondary));
    line-height: 1.5;
  }
  .chip-row {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
  }
  /* Lifted from the page background with a tinted lavender wash +
     a stroke ~2 steps brighter than `--border-mid`. The previous
     `var(--border)` (#1E1832) was visually identical to the
     surrounding bg-secondary surface and read as no border at all.
     Combined with the wash we now have a real "chip" silhouette
     without leaning on a saturated outline. */
  /* Hard-cornered terminal chip. Flat fill, 1px lavender stroke,
     monospace label. A leading `>` glyph sells the prompt register
     and gives the chip an asymmetric left edge — distinct, no pill
     softness. */
  .chip {
    position: relative;
    background: transparent;
    border: 1px solid var(--border-bright, #3D3364);
    color: var(--text-primary, #eee);
    padding: 12px 16px 12px 32px;
    border-radius: 2px;
    font-size: 0.86rem;
    font-family: var(--font-mono);
    font-weight: 400;
    line-height: 1.5;
    letter-spacing: 0.01em;
    text-align: left;
    cursor: pointer;
    max-width: 540px;
    transition:
      background 160ms ease,
      border-color 160ms ease,
      color 160ms ease,
      box-shadow 160ms ease;
  }
  .chip::before {
    content: ">";
    position: absolute;
    left: 14px;
    top: 12px;
    color: var(--accent);
    font-family: var(--font-mono);
    font-weight: 600;
    line-height: 1.5;
    transition: color 160ms ease, transform 160ms ease;
  }
  .chip:hover,
  .chip:focus-visible {
    background: var(--accent-glow);
    border-color: var(--accent, #c4a46a);
    color: var(--accent-light, #DFC068);
    box-shadow:
      inset 2px 0 0 var(--accent),
      0 0 14px -4px rgba(201, 168, 76, 0.35);
    outline: none;
  }
  .chip:hover::before,
  .chip:focus-visible::before {
    color: var(--accent-light);
    transform: translateX(1px);
  }
  .starters.muted .chip {
    color: var(--text-secondary, var(--text-primary));
    border-style: dashed;
  }
</style>
