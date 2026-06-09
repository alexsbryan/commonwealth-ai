<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
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
  <aside class="resume" aria-label="Unfinished indexing">
    <p class="resume-text">
      <span class="lk-label resume-label">Unfinished</span>
      Indexing was interrupted. Resume or discard.
    </p>

    <ul class="jobs">
      {#each jobs as job (job.corpus_id)}
        <li class="job">
          <div class="job-info">
            <span class="job-name">{job.display_name}</span>
            <span class="job-meter lk-folio">
              {job.files_done} / {job.files_total} files
            </span>
          </div>
          <div class="job-actions">
            <button
              class="lk-btn lk-btn--mark"
              onclick={() => onResume(job.corpus_id)}
            >
              Resume
            </button>
            <button
              class="lk-btn lk-btn--ghost"
              onclick={() => onDiscard(job.corpus_id)}
            >
              Discard
            </button>
          </div>
        </li>
      {/each}
    </ul>
  </aside>
{/if}

<style>
  .resume {
    margin-bottom: 20px;
    padding: 14px 16px;
    border: 1px solid var(--lk-stamp);
    background: var(--lk-stamp-wash);
    border-radius: var(--radius);
    animation: lk-fade-in 220ms ease-out both;
  }
  .resume-text {
    margin: 0 0 10px;
    font-size: var(--lk-size-body);
    color: var(--lk-ink);
  }
  .resume-label {
    color: var(--lk-stamp-ink);
    margin-right: 10px;
  }
  .jobs {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .job {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 14px;
    padding: 8px 10px;
    background: var(--lk-paper);
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
  }
  .job-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .job-name {
    font-size: var(--lk-size-body);
    font-weight: 500;
    color: var(--lk-ink);
  }
  .job-meter {
    color: var(--lk-ink-faded);
  }
  .job-actions {
    display: flex;
    gap: 6px;
  }
</style>
