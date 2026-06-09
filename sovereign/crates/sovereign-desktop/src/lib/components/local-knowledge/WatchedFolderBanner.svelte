<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { lcWatchConfirmDeletion, lcWatchResume } from "../../api";
  import type { WatchedFolderListEntry } from "../../types";

  interface Props {
    /** Subset of the list that's blocked or errored — i.e., the
     *  user should know about them. The parent computes this and
     *  hides the banner entirely when empty. */
    blocked: WatchedFolderListEntry[];
    onChanged: () => Promise<void> | void;
  }

  let { blocked, onChanged }: Props = $props();

  let inflight: string | null = $state(null);
  let actionError: string | null = $state(null);

  async function confirmDeletion(id: string) {
    inflight = id;
    actionError = null;
    try {
      await lcWatchConfirmDeletion(id);
      await onChanged();
    } catch (e) {
      actionError = String(e);
    }
    inflight = null;
  }

  async function resume(id: string) {
    inflight = id;
    actionError = null;
    try {
      await lcWatchResume(id);
      await onChanged();
    } catch (e) {
      actionError = String(e);
    }
    inflight = null;
  }

  function summary(entry: WatchedFolderListEntry): string {
    const s = entry.status;
    switch (s.kind) {
      case "paused_awaiting_confirmation": {
        const removed = s.diff_summary.removed;
        const live = s.diff_summary.live_before;
        const rule =
          s.tripped_rule.rule === "absolute"
            ? `${removed} files would be deleted (threshold: ${s.tripped_rule.threshold})`
            : `${removed} of ${live} live docs would be deleted (${(s.tripped_rule.observed * 100).toFixed(0)}%, threshold: ${(s.tripped_rule.threshold * 100).toFixed(0)}%)`;
        return rule;
      }
      case "errored":
        return `Last sweep errored: ${s.message}`;
      default:
        return "Needs attention";
    }
  }
</script>

{#if blocked.length > 0}
  <section class="banner">
    <header class="head">
      <span class="dot" aria-hidden="true">●</span>
      <h2 class="title">
        {blocked.length === 1
          ? "1 watched folder needs attention"
          : `${blocked.length} watched folders need attention`}
      </h2>
    </header>

    <ul class="list">
      {#each blocked as entry (entry.corpus_id)}
        <li class="item">
          <div class="info">
            <span class="name">{entry.display_name}</span>
            <span class="reason">{summary(entry)}</span>
          </div>
          <div class="actions">
            {#if entry.status.kind === "paused_awaiting_confirmation"}
              <button
                class="primary"
                onclick={() => confirmDeletion(entry.corpus_id)}
                disabled={inflight === entry.corpus_id}
              >
                Apply deletion
              </button>
            {:else if entry.status.kind === "errored"}
              <button
                class="ghost"
                onclick={() => resume(entry.corpus_id)}
                disabled={inflight === entry.corpus_id}
              >
                Retry on next sweep
              </button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>

    {#if actionError}
      <p class="error">{actionError}</p>
    {/if}
  </section>
{/if}

<style>
  .banner {
    margin-bottom: 18px;
    padding: 16px 18px;
    background: var(--lk-warn-wash);
    border: 1px solid var(--lk-warn);
    border-radius: var(--radius);
    animation: lk-fade-in 240ms ease-out both;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 12px;
  }
  .dot {
    color: var(--lk-warn);
    font-size: 1rem;
    line-height: 1;
  }
  .title {
    margin: 0;
    font-family: var(--lk-font-display);
    font-size: var(--lk-size-lead);
    font-weight: 600;
    color: var(--lk-ink);
  }
  .list {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 14px;
    padding: 10px 12px;
    background: var(--lk-paper);
    border: 1px solid var(--lk-rule);
    border-radius: 6px;
  }
  .info {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .name {
    font-weight: 500;
    color: var(--lk-ink);
  }
  .reason {
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
  }
  .actions {
    display: flex;
    gap: 6px;
    flex-shrink: 0;
  }
  .primary {
    padding: 6px 14px;
    background: var(--lk-warn);
    border: 1px solid var(--lk-warn);
    border-radius: 6px;
    color: white;
    font-weight: 500;
    cursor: pointer;
    font-size: var(--lk-size-meta);
  }
  .ghost {
    padding: 6px 12px;
    background: transparent;
    border: 1px solid var(--lk-rule);
    border-radius: 6px;
    color: var(--lk-ink);
    cursor: pointer;
    font-size: var(--lk-size-meta);
  }
  .ghost:hover { border-color: var(--lk-warn); }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  .error {
    margin: 10px 0 0;
    padding: 8px 12px;
    border-left: 3px solid var(--lk-err);
    background: var(--lk-err-wash);
    color: var(--lk-ink);
    font-size: var(--lk-size-meta);
  }
</style>
