<!--
  FolderCompletePanel — ingest-only completion screen. Used when
  the atlas build was skipped, cancelled, or still in flight when
  the user surfaced the result.

  Not a dead end. Even without atlas-mined Question atoms, we
  surface:
    • Cleaned-up excerpts (sentence-boundary trim, filename
      humanisation)
    • A "Try asking" chip row synthesised from the excerpts —
      click-to-ask gives the user a concrete payoff instead of
      a bare "Done" button that drops them on an empty chat
  When atlas keeps building, the note above the chips acknowledges
  that richer questions are on the way.
-->
<script lang="ts">
  import type { IngestStats, StarterQuestion } from "../../../types";
  import {
    cleanExcerptBody,
    cleanExcerptTitle,
    deriveExcerptStarters,
  } from "../../../onboarding/excerpt_helpers";
  import StarterChips from "../../StarterChips.svelte";

  interface Props {
    stats: IngestStats;
    onDone: () => void;
    /// Optional: when the user clicks a starter chip, fire so the
    /// chat view opens with the question seeded + auto-submitted.
    /// Falls back to `onDone` behaviour when unwired.
    onStartChat?: (question: StarterQuestion) => void;
    /// When true, the parent knows an atlas build is still running
    /// for this corpus — we add a gentle note below the chips so
    /// the user doesn't think the current set is all they'll get.
    atlasStillBuilding?: boolean;
  }

  let {
    stats,
    onDone,
    onStartChat,
    atlasStillBuilding = false,
  }: Props = $props();

  /// Starter chips synthesised from the picked excerpts. Empty
  /// when there are no usable excerpts — in that case we fall back
  /// to just the Done CTA.
  let starters = $derived(deriveExcerptStarters(stats.excerpt_chunks, 4));

  function handlePick(q: StarterQuestion) {
    if (onStartChat) {
      onStartChat(q);
    } else {
      onDone();
    }
  }
</script>

<section class="complete">
  <header class="head">
    <h1 class="title">Indexed.</h1>
    <p class="count">
      <span class="lk-num">{stats.files_indexed}</span>
      document{stats.files_indexed === 1 ? "" : "s"},
      <span class="lk-num">{stats.chunks_written.toLocaleString()}</span>
      passages — ready to search.
    </p>
  </header>

  {#if starters.length > 0}
    <section class="payoff">
      <p class="payoff-invitation">What connections can we make?</p>
      <StarterChips
        questions={starters}
        onPick={handlePick}
        subheading={atlasStillBuilding
          ? "Questions from this pass — the atlas is still building; richer ones will arrive shortly."
          : "Pick one to start your first chat."}
      />
    </section>
  {/if}

  {#if stats.excerpt_chunks.length > 0}
    <section class="excerpts">
      <p class="excerpts-label">What was indexed</p>
      <ol class="excerpt-list">
        {#each stats.excerpt_chunks as chunk}
          <li class="excerpt">
            <p class="excerpt-source">
              <span class="excerpt-mark" aria-hidden="true">▤</span>
              <span class="excerpt-source-title">
                {cleanExcerptTitle(chunk.source_name)}
              </span>
              {#if chunk.page_ref}
                <span class="excerpt-page">· {chunk.page_ref}</span>
              {/if}
            </p>
            <p class="excerpt-body">
              {cleanExcerptBody(chunk.text, chunk.source_name)}
            </p>
          </li>
        {/each}
      </ol>
    </section>
  {/if}

  {#if stats.runtime_failures.length > 0}
    <section class="failures">
      <p class="failures-label">Skipped</p>
      <ul class="failures-list">
        {#each stats.runtime_failures as f}
          <li>{f.file.display_name}</li>
        {/each}
      </ul>
      <p class="failures-note">Not indexed — see the list above.</p>
    </section>
  {/if}

  <aside class="privacy">
    Your documents stayed on your machine.
    <strong>Nothing was uploaded.</strong>
  </aside>

  <div class="actions">
    <button class="lk-btn lk-btn--quiet" onclick={onDone}>
      {starters.length > 0 ? "Not now — take me to chat" : "Done"}
    </button>
  </div>
</section>

<style>
  .complete {
    padding: 8px 0 4px;
    max-width: 720px;
    color: var(--lk-ink);
    animation: lk-fade-in 320ms ease-out both;
  }

  .head { margin-bottom: 22px; }
  .title {
    margin: 0 0 6px;
    font-family: var(--font-serif);
    font-style: italic;
    font-size: 2.25rem;
    font-weight: 500;
    line-height: 1.05;
    letter-spacing: -0.01em;
    color: var(--accent-light);
  }
  .count {
    margin: 0;
    font-size: 1rem;
    color: var(--lk-ink-soft);
    line-height: 1.55;
  }
  .count .lk-num {
    color: var(--lk-ink);
    font-weight: 600;
    margin: 0 2px;
  }

  /* ── Payoff chip row (primary "ask-now" affordance) ──── */
  .payoff {
    margin: 20px 0 8px;
    padding: 18px 20px;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-left: 2px solid var(--accent);
    border-radius: var(--radius-lg, 10px);
    box-shadow:
      inset 0 1px 0 rgba(223, 192, 104, 0.08),
      0 1px 12px var(--accent-glow);
  }
  .payoff-invitation {
    margin: 0 0 14px;
    font-family: var(--font-serif);
    font-style: italic;
    font-size: 1.25rem;
    line-height: 1.2;
    color: var(--accent-light);
  }

  /* ── Indexed excerpts (secondary, "here's a sample") ─── */
  .excerpts {
    margin: 24px 0 18px;
    padding-top: 20px;
    border-top: 1px solid var(--lk-rule);
  }
  .excerpts-label {
    margin: 0 0 14px;
    font-family: var(--font-mono);
    font-size: 0.64rem;
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--text-muted);
  }
  .excerpt-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .excerpt {
    padding-bottom: 18px;
    border-bottom: 1px solid var(--lk-rule-soft);
  }
  .excerpt:last-child { border-bottom: 0; padding-bottom: 0; }
  .excerpt-source {
    margin: 0 0 6px;
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 0.86rem;
    color: var(--lk-ink);
  }
  .excerpt-mark {
    color: var(--accent);
    font-size: 0.88rem;
    line-height: 1;
    flex-shrink: 0;
  }
  .excerpt-source-title {
    font-family: var(--font-serif);
    font-style: italic;
    font-size: 0.98rem;
    color: var(--text-primary);
  }
  .excerpt-page {
    font-family: var(--font-mono);
    font-size: 0.74rem;
    color: var(--text-muted);
  }
  .excerpt-body {
    margin: 0;
    padding-left: 20px;
    font-size: 0.92rem;
    color: var(--lk-ink-soft);
    line-height: 1.6;
    border-left: 1px solid var(--border);
  }

  .failures {
    margin: 20px 0;
    padding: 14px 16px;
    background: var(--bg-secondary);
    border-left: 2px solid var(--text-muted);
    border-radius: var(--radius);
  }
  .failures-label {
    margin: 0 0 8px;
    font-family: var(--font-mono);
    font-size: 0.64rem;
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--text-muted);
  }
  .failures-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .failures-list li {
    font-family: var(--font-mono);
    font-size: 0.78rem;
    color: var(--lk-ink-soft);
  }
  .failures-note {
    margin: 8px 0 0;
    font-size: 0.78rem;
    color: var(--text-muted);
  }

  .privacy {
    margin: 24px 0;
    padding: 14px 0;
    border-top: 1px solid var(--lk-rule);
    border-bottom: 1px solid var(--lk-rule);
    font-size: 0.94rem;
    color: var(--lk-ink-soft);
    line-height: 1.55;
  }
  .privacy strong {
    font-weight: 600;
    color: var(--text-primary);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 12px;
  }
</style>
