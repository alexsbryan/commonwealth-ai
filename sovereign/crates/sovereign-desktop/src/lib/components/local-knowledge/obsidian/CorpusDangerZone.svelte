<script lang="ts">
  import { onMount } from "svelte";

  import { lcClean, lcListSnapshots, lcRollback } from "../../../api";
  import type { SnapshotMeta } from "../../../types";

  interface Props {
    corpusId: string;
    /// Called after rollback or clean completes so the parent can
    /// refresh whatever cached preview state it was holding.
    onReset?: () => void;
  }

  let { corpusId, onReset }: Props = $props();

  let snapshots = $state<SnapshotMeta[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let statusMessage = $state<string | null>(null);

  onMount(async () => {
    await reload();
  });

  async function reload() {
    loading = true;
    error = null;
    try {
      snapshots = await lcListSnapshots(corpusId);
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  function formatRelative(ts: string): string {
    // Very simple humanisation — no date-fns dep, no timezone gymnastics.
    const d = new Date(ts);
    return d.toLocaleString();
  }

  async function confirmRollback(snap: SnapshotMeta) {
    const ok = window.confirm(
      `Restore your vault to the state from ${formatRelative(snap.taken_at)}? ` +
        `This will remove all Sovereign tags written in that run and restore ` +
        `${snap.file_count} notes' frontmatter exactly as it was before.`,
    );
    if (!ok) return;
    try {
      const r = await lcRollback(corpusId, snap.snapshot_path);
      statusMessage = `Restored ${r.files_restored} notes; deleted ${r.index_notes_deleted} index notes.`;
      if (r.files_skipped.length > 0) {
        statusMessage += ` ${r.files_skipped.length} files were skipped.`;
      }
      onReset?.();
      await reload();
    } catch (e) {
      error = `Rollback failed: ${e}`;
    }
  }

  async function confirmClean() {
    const ok = window.confirm(
      "Remove all Sovereign tags and generated index notes from your vault? " +
        "Your own notes and tags will not be affected.",
    );
    if (!ok) return;
    try {
      const r = await lcClean(corpusId);
      statusMessage = `Removed Sovereign tags from ${r.tags_removed_from} notes; deleted ${r.index_notes_deleted} index notes.`;
      onReset?.();
    } catch (e) {
      error = `Clean failed: ${e}`;
    }
  }
</script>

<div class="danger-zone">
  {#if error}
    <p class="error">{error}</p>
  {/if}
  {#if statusMessage}
    <p class="status">{statusMessage}</p>
  {/if}

  {#if loading}
    <p class="loading">Loading snapshots…</p>
  {:else if snapshots.length > 0}
    <div class="restore-section">
      <h4>Restore previous state</h4>
      <p class="desc">
        Sovereign saved a restore point before each write. Restoring removes
        all tags written in that run and restores your notes' previous
        frontmatter exactly.
      </p>
      <div class="snapshot-list">
        {#each snapshots as snap (snap.snapshot_path)}
          <div class="snapshot-row">
            <div class="snapshot-info">
              <span class="date">{formatRelative(snap.taken_at)}</span>
              <span class="detail">
                {snap.file_count} notes · version {snap.sovereign_version}
                {#if snap.git_commit}
                  · git <code>{snap.git_commit.slice(0, 7)}</code>
                {/if}
              </span>
            </div>
            <button class="btn-restore" onclick={() => confirmRollback(snap)}>
              Restore
            </button>
          </div>
        {/each}
      </div>
    </div>

    <div class="divider"></div>
  {/if}

  <div class="clean-section">
    <h4>Remove Sovereign's content</h4>
    <p class="desc">
      Remove all <code>sovereign/*</code> tags and delete all generated
      index notes. Your own notes and tags are not affected.
    </p>
    <button class="btn-danger-outline" onclick={confirmClean}>
      Remove Sovereign tags
    </button>
  </div>
</div>

<style>
  .danger-zone {
    padding: 16px 0;
  }
  h4 {
    font-size: 14px;
    font-weight: 500;
    margin: 0 0 6px;
  }
  .desc {
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
    margin: 0 0 12px;
    line-height: 1.4;
  }
  .snapshot-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .snapshot-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 12px;
    border: 1px solid var(--color-border, #d4d4d4);
    border-radius: 4px;
    font-size: 13px;
  }
  .snapshot-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .date {
    font-weight: 500;
  }
  .detail {
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
  }
  .detail code {
    background: var(--color-surface-subtle, #f4f4f4);
    padding: 1px 5px;
    border-radius: 3px;
  }
  .btn-restore,
  .btn-danger-outline {
    padding: 6px 12px;
    font-size: 13px;
    border-radius: 4px;
    cursor: pointer;
  }
  .btn-restore {
    border: 1px solid var(--color-accent, #3a5fc9);
    background: transparent;
    color: var(--color-accent, #3a5fc9);
  }
  .btn-restore:hover {
    background: color-mix(in srgb, var(--color-accent, #3a5fc9) 10%, transparent);
  }
  .btn-danger-outline {
    border: 1px solid var(--color-error, #c92a2a);
    background: transparent;
    color: var(--color-error, #c92a2a);
  }
  .btn-danger-outline:hover {
    background: color-mix(in srgb, var(--color-error, #c92a2a) 10%, transparent);
  }
  .divider {
    height: 1px;
    background: var(--color-border, #d4d4d4);
    margin: 20px 0;
  }
  .loading {
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
  }
  .error {
    padding: 8px 12px;
    background: color-mix(in srgb, var(--color-error, #c92a2a) 10%, transparent);
    border-radius: 4px;
    color: var(--color-error, #c92a2a);
    font-size: 13px;
  }
  .status {
    padding: 8px 12px;
    background: color-mix(in srgb, var(--color-accent, #3a5fc9) 10%, transparent);
    border-radius: 4px;
    color: var(--color-accent, #3a5fc9);
    font-size: 13px;
  }
</style>
