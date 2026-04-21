<script lang="ts">
  import type { IncompleteJob } from "../../types";

  interface Props {
    jobs: IncompleteJob[];
    onResume: (corpusId: string) => void;
    onDiscard: (corpusId: string) => void;
  }

  let { jobs, onResume, onDiscard }: Props = $props();
</script>

{#if jobs.length > 0}
  <div class="resume-prompt">
    <p class="title">You have indexing jobs that didn't finish.</p>

    {#each jobs as job (job.corpus_id)}
      <div class="job-row">
        <div class="job-info">
          <span class="job-name">{job.display_name}</span>
          <span class="job-progress">
            {job.files_done} / {job.files_total} files
          </span>
        </div>
        <div class="job-actions">
          <button class="btn-primary" onclick={() => onResume(job.corpus_id)}>
            Continue
          </button>
          <button class="btn-ghost" onclick={() => onDiscard(job.corpus_id)}>
            Discard
          </button>
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .resume-prompt {
    padding: 12px 14px;
    margin-bottom: 16px;
    background: color-mix(in srgb, var(--color-accent, #3a5fc9) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--color-accent, #3a5fc9) 40%, transparent);
    border-radius: 6px;
  }
  .title {
    margin: 0 0 10px;
    font-size: 14px;
    font-weight: 500;
  }
  .job-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 0;
  }
  .job-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .job-name {
    font-size: 13px;
    font-weight: 500;
  }
  .job-progress {
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
  }
  .job-actions {
    display: flex;
    gap: 8px;
  }
  .btn-primary,
  .btn-ghost {
    padding: 6px 12px;
    border-radius: 4px;
    font-size: 13px;
    cursor: pointer;
    border: none;
  }
  .btn-primary {
    background: var(--color-accent, #3a5fc9);
    color: #fff;
  }
  .btn-ghost {
    background: transparent;
    color: var(--color-text-muted, #6b6b6b);
  }
  .btn-ghost:hover {
    color: var(--color-text, #1a1a1a);
  }
</style>
