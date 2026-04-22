<script lang="ts">
  import type { IngestStats } from "../../../types";

  interface Props {
    stats: IngestStats;
    onDone: () => void;
  }

  let { stats, onDone }: Props = $props();
</script>

<section class="complete">
  <header class="head">
    <h1 class="title">Indexed.</h1>
    <p class="count">
      <span class="lk-num">{stats.files_indexed}</span> documents,
      <span class="lk-num">{stats.chunks_written.toLocaleString()}</span> passages,
      ready to search.
    </p>
  </header>

  {#if stats.excerpt_chunks.length > 0}
    <section class="excerpts">
      <p class="lk-label excerpts-label">A sample of what was indexed</p>
      <ol class="excerpt-list">
        {#each stats.excerpt_chunks as chunk}
          <li class="excerpt">
            <p class="excerpt-body">{chunk.text}</p>
            <p class="excerpt-source">
              — {chunk.source_name}{#if chunk.page_ref}, {chunk.page_ref}{/if}
            </p>
          </li>
        {/each}
      </ol>
    </section>
  {/if}

  {#if stats.runtime_failures.length > 0}
    <section class="failures">
      <p class="lk-label failures-label">Skipped</p>
      <ul class="failures-list">
        {#each stats.runtime_failures as f}
          <li>{f.file.display_name}</li>
        {/each}
      </ul>
      <p class="failures-note">Not in your index.</p>
    </section>
  {/if}

  <aside class="privacy">
    Your documents stayed on your machine.
    <strong>Nothing was uploaded.</strong>
  </aside>

  <div class="actions">
    <button class="lk-btn lk-btn--mark" onclick={onDone}>Done</button>
  </div>
</section>

<style>
  .complete {
    padding: 8px 0;
    max-width: 720px;
    animation: lk-fade-in 320ms ease-out both;
  }

  .head { margin-bottom: 22px; }
  .title {
    margin: 0 0 6px;
    font-size: 2.625rem;
    font-weight: 600;
    line-height: 1;
    letter-spacing: -0.025em;
    color: var(--lk-ink);
  }
  .count {
    margin: 0;
    font-size: var(--lk-size-lead);
    color: var(--lk-ink-soft);
  }
  .count .lk-num {
    color: var(--lk-ink);
    font-size: 1.25em;
    margin: 0 2px;
  }

  .excerpts {
    margin: 24px 0;
    padding-top: 20px;
    border-top: 1px solid var(--lk-rule);
  }
  .excerpts-label { margin-bottom: 14px; }
  .excerpt-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .excerpt {
    padding-bottom: 16px;
    border-bottom: 1px solid var(--lk-rule-soft);
  }
  .excerpt:last-child {
    border-bottom: 0;
    padding-bottom: 0;
  }
  .excerpt-body {
    margin: 0;
    font-size: var(--lk-size-body);
    color: var(--lk-ink);
    line-height: 1.55;
  }
  .excerpt-source {
    margin: 4px 0 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
  }

  .failures {
    margin: 20px 0;
    padding: 14px 16px;
    background: var(--lk-paper-deep);
    border-left: 2px solid var(--lk-ink-faded);
    border-radius: var(--radius);
  }
  .failures-label { margin-bottom: 8px; }
  .failures-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .failures-list li {
    font-family: var(--lk-font-mono);
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
  }
  .failures-note {
    margin: 8px 0 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
  }

  /* The privacy statement. Spec §8.6: same visual weight as the
     document count, non-dismissable, static. Plain copy, slight
     emphasis on the guarantee via gold. */
  .privacy {
    margin: 28px 0;
    padding: 16px 0;
    border-top: 1px solid var(--lk-rule);
    border-bottom: 1px solid var(--lk-rule);
    font-size: var(--lk-size-lead);
    color: var(--lk-ink);
    line-height: 1.5;
  }
  .privacy strong {
    font-weight: 600;
    color: var(--lk-stamp-ink);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 16px;
  }
</style>
