<script lang="ts">
  import type { IngestStats } from "../../../types";

  interface Props {
    stats: IngestStats;
    onDone: () => void;
  }

  let { stats, onDone }: Props = $props();
</script>

<div class="complete-panel">
  <h2>Your research library is ready.</h2>
  <p class="count">{stats.files_indexed} documents indexed.</p>

  {#if stats.excerpt_chunks.length > 0}
    <div class="excerpts">
      <p class="excerpts-label">Here's a sample of what I've read:</p>
      {#each stats.excerpt_chunks as chunk}
        <div class="excerpt">
          <blockquote>"{chunk.text}"</blockquote>
          <cite>
            — {chunk.source_name}
            {#if chunk.page_ref}, {chunk.page_ref}{/if}
          </cite>
        </div>
      {/each}
    </div>
  {/if}

  {#if stats.runtime_failures.length > 0}
    <div class="runtime-failures">
      <p>
        {stats.runtime_failures.length} files couldn't be fully read during indexing:
      </p>
      {#each stats.runtime_failures as f}
        <div class="failure-row">— {f.file.display_name}</div>
      {/each}
      <p class="failure-note">These are excluded from your library.</p>
    </div>
  {/if}

  <!-- Spec §8.6: static text, same visual weight as the doc count,
       not dismissable. Do not turn this into a toast. -->
  <p class="privacy-statement">
    These documents are on your computer.
    Nothing was uploaded anywhere.
  </p>

  <button class="btn-primary" onclick={onDone}>Done</button>
</div>

<style>
  .complete-panel {
    padding: 16px 0;
    max-width: 640px;
  }
  h2 {
    font-size: 18px;
    font-weight: 500;
    margin: 0 0 6px;
  }
  .count {
    font-size: 15px;
    color: var(--color-text, #1a1a1a);
    margin: 0 0 20px;
  }
  .excerpts {
    margin: 20px 0;
  }
  .excerpts-label {
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
    margin-bottom: 10px;
  }
  .excerpt {
    margin-bottom: 16px;
  }
  blockquote {
    margin: 0 0 4px;
    padding: 10px 14px;
    background: var(--color-surface-subtle, #f4f4f4);
    border-left: 3px solid var(--color-accent, #3a5fc9);
    border-radius: 4px;
    font-size: 14px;
    color: var(--color-text, #1a1a1a);
    font-style: italic;
  }
  cite {
    display: block;
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
    font-style: normal;
    padding-left: 14px;
  }
  .runtime-failures {
    margin: 16px 0;
    padding: 12px;
    background: var(--color-surface-subtle, #f4f4f4);
    border-radius: 6px;
    font-size: 13px;
  }
  .failure-row {
    font-family: var(--font-mono, ui-monospace, monospace);
    padding: 2px 0;
  }
  .failure-note {
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
    margin: 6px 0 0;
  }
  .privacy-statement {
    margin: 24px 0;
    font-size: 15px;
    color: var(--color-text, #1a1a1a);
    line-height: 1.4;
  }
  .btn-primary {
    padding: 8px 16px;
    border-radius: 6px;
    font-size: 14px;
    cursor: pointer;
    border: none;
    background: var(--color-accent, #3a5fc9);
    color: #fff;
  }
  .btn-primary:hover {
    background: var(--color-accent-hover, #2f4fb3);
  }
</style>
