<script lang="ts">
  import {
    lcWatchPause,
    lcWatchResume,
    lcWatchConfirmDeletion,
    lcWatchRemove,
    lcWatchSyncNow,
  } from "../../api";
  import type {
    WatchedFolderListEntry,
    WatchedFolderStatus,
  } from "../../types";

  interface Props {
    corpora: WatchedFolderListEntry[];
    onChanged: () => Promise<void> | void;
    /** Open the §3.7 folder-detail panel for one corpus.
     *  Optional so the standalone-list usage (banner / tests
     *  rendering the cards in isolation) doesn't have to wire it. */
    onOpenDetail?: (corpusId: string) => void;
  }

  let { corpora, onChanged, onOpenDetail }: Props = $props();

  let actionInflight: string | null = $state(null);
  let actionError: string | null = $state(null);

  async function pause(id: string) {
    actionInflight = id;
    actionError = null;
    try {
      await lcWatchPause(id, "user");
      await onChanged();
    } catch (e) {
      actionError = String(e);
    }
    actionInflight = null;
  }

  async function resume(id: string) {
    actionInflight = id;
    actionError = null;
    try {
      await lcWatchResume(id);
      await onChanged();
    } catch (e) {
      actionError = String(e);
    }
    actionInflight = null;
  }

  async function confirmDeletion(id: string) {
    actionInflight = id;
    actionError = null;
    try {
      await lcWatchConfirmDeletion(id);
      await onChanged();
    } catch (e) {
      actionError = String(e);
    }
    actionInflight = null;
  }

  async function syncNow(id: string) {
    actionInflight = id;
    actionError = null;
    try {
      await lcWatchSyncNow(id);
      await onChanged();
    } catch (e) {
      actionError = String(e);
    }
    actionInflight = null;
  }

  async function remove(id: string, name: string) {
    if (
      !window.confirm(
        `Stop watching "${name}"? The index will be removed; the original folder is not touched.`,
      )
    )
      return;
    actionInflight = id;
    actionError = null;
    try {
      await lcWatchRemove(id);
      await onChanged();
    } catch (e) {
      actionError = String(e);
    }
    actionInflight = null;
  }

  function statusLabel(s: WatchedFolderStatus): string {
    switch (s.kind) {
      case "idle":
        return s.last_sweep_unix === 0
          ? "Idle (no sweep yet)"
          : `Idle — last swept ${formatRelative(s.last_sweep_unix)}`;
      case "sweeping":
        return s.total > 0
          ? `Sweeping ${s.phase} (${s.current}/${s.total})`
          : `Sweeping ${s.phase}`;
      case "paused_awaiting_confirmation":
        return "Pending deletion blocked — awaiting confirmation";
      case "paused_manual":
        return `Paused (${s.reason})`;
      case "errored":
        return `Errored — ${s.message}`;
    }
  }

  function statusClass(s: WatchedFolderStatus): string {
    switch (s.kind) {
      case "paused_awaiting_confirmation":
        return "warn";
      case "errored":
        return "err";
      case "paused_manual":
        return "muted";
      case "sweeping":
        return "active";
      default:
        return "ok";
    }
  }

  function formatRelative(unixSecs: number): string {
    const now = Math.floor(Date.now() / 1000);
    const delta = Math.max(0, now - unixSecs);
    if (delta < 60) return `${delta}s ago`;
    if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
    if (delta < 86_400) return `${Math.floor(delta / 3600)}h ago`;
    return `${Math.floor(delta / 86_400)}d ago`;
  }
</script>

{#if corpora.length === 0}
  <p class="empty">No folders watched yet.</p>
{:else}
  <ul class="list">
    {#each corpora as entry (entry.corpus_id)}
      <li class="card" class:sensitive={entry.sensitive}>
        <header class="card-head">
          <div class="name-block">
            <span class="name">{entry.display_name}</span>
            <span class="path" title={entry.root_path}>{entry.root_path}</span>
          </div>
          <div class="badges">
            <span class="badge {statusClass(entry.status)}">
              {statusLabel(entry.status)}
            </span>
            {#if entry.additional_roots_count > 0}
              <span
                class="badge subtle"
                title="This corpus is anchored on the primary folder plus additional roots layered on top. Open Details to inspect or detach them."
              >
                +{entry.additional_roots_count}
                {entry.additional_roots_count === 1 ? "folder" : "folders"}
              </span>
            {/if}
            {#if entry.sync_mode === "manual"}
              <span
                class="badge subtle"
                title="Manual sync — sweeps only on request"
              >
                Manual sync
              </span>
            {/if}
            {#if entry.sensitive}
              <span
                class="badge sensitive-badge"
                title="Excluded from ambient situated-context assembly"
              >
                Sensitive
              </span>
            {/if}
          </div>
        </header>
        {#if entry.status.kind === "idle"}
          <p class="meta">
            {entry.status.live_docs} live · {entry.status.tombstones} tombstones
          </p>
        {/if}

        <div class="actions">
          {#if onOpenDetail}
            <button
              class="ghost"
              onclick={() => onOpenDetail?.(entry.corpus_id)}
              title="Inspect formats, failed extractions, and what's not indexed"
            >
              Details
            </button>
          {/if}
          {#if entry.status.kind === "paused_awaiting_confirmation"}
            <button
              class="primary"
              onclick={() => confirmDeletion(entry.corpus_id)}
              disabled={actionInflight === entry.corpus_id}
            >
              Confirm deletion
            </button>
          {/if}
          {#if entry.sync_mode === "manual" && entry.status.kind !== "sweeping"}
            <button
              class="ghost"
              onclick={() => syncNow(entry.corpus_id)}
              disabled={actionInflight === entry.corpus_id}
              title="Trigger a sweep now (Manual mode only sweeps on request)"
            >
              Sync now
            </button>
          {/if}
          {#if entry.status.kind === "paused_manual"}
            <button
              class="ghost"
              onclick={() => resume(entry.corpus_id)}
              disabled={actionInflight === entry.corpus_id}
            >
              Resume
            </button>
          {:else if entry.status.kind === "idle" || entry.status.kind === "sweeping" || entry.status.kind === "errored"}
            <button
              class="ghost"
              onclick={() => pause(entry.corpus_id)}
              disabled={actionInflight === entry.corpus_id}
            >
              Pause
            </button>
          {/if}
          <button
            class="ghost danger"
            onclick={() => remove(entry.corpus_id, entry.display_name)}
            disabled={actionInflight === entry.corpus_id}
          >
            Stop watching
          </button>
        </div>
      </li>
    {/each}
  </ul>
{/if}

{#if actionError}
  <p class="error">{actionError}</p>
{/if}

<style>
  .empty {
    margin: 0;
    padding: 18px;
    text-align: center;
    color: var(--lk-ink-faded);
    font-size: var(--lk-size-meta);
  }
  .list {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .card {
    padding: 14px 16px;
    background: var(--lk-paper-deep);
    border: 1px solid var(--lk-rule);
    border-radius: var(--radius);
  }
  .card-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 14px;
  }
  .name-block {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .name {
    font-size: var(--lk-size-lead);
    font-weight: 500;
    color: var(--lk-ink);
  }
  .path {
    font-family: var(--lk-font-mono, monospace);
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 320px;
  }
  .badge {
    flex-shrink: 0;
    padding: 4px 10px;
    border-radius: 999px;
    font-size: var(--lk-size-meta);
    font-weight: 500;
    white-space: nowrap;
  }
  .badge.ok     { background: var(--lk-paper-deep); color: var(--lk-ink-soft); border: 1px solid var(--lk-rule); }
  .badge.warn   { background: var(--lk-warn-wash);  color: var(--lk-warn); }
  .badge.err    { background: var(--lk-err-wash);   color: var(--lk-err); }
  .badge.muted  { background: var(--lk-paper-deep); color: var(--lk-ink-faded); border: 1px solid var(--lk-rule); }
  .badge.active { background: var(--lk-crown-wash); color: var(--lk-crown-light); }
  .badge.subtle {
    background: transparent;
    color: var(--lk-ink-faded);
    border: 1px dashed var(--lk-rule);
  }
  .badge.sensitive-badge {
    background: var(--lk-paper-deep);
    color: var(--lk-ink-soft);
    border: 1px solid var(--lk-ink-faded);
    font-style: italic;
  }
  .badges {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 4px;
  }
  .card.sensitive {
    border-left: 3px solid var(--lk-ink-faded);
  }

  .meta {
    margin: 8px 0 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
  }

  .actions {
    margin-top: 12px;
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
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
  .ghost:hover { border-color: var(--lk-crown); }
  .ghost.danger { color: var(--lk-err); }
  .ghost.danger:hover { border-color: var(--lk-err); }
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
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  .error {
    margin: 8px 0 0;
    padding: 8px 12px;
    border-left: 3px solid var(--lk-err);
    background: var(--lk-err-wash);
    color: var(--lk-ink);
    font-size: var(--lk-size-meta);
  }
</style>
