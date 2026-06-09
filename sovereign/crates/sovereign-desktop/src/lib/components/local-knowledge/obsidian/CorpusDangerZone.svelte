<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";

  import { lcClean, lcListSnapshots, lcRollback } from "../../../api";
  import type { SnapshotMeta } from "../../../types";

  interface Props {
    corpusId: string;
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
    return new Date(ts).toLocaleString();
  }

  async function confirmRollback(snap: SnapshotMeta) {
    const ok = window.confirm(
      `Restore your vault to the state from ${formatRelative(snap.taken_at)}? ` +
        `This removes all Sovereign tags written in that run and restores ` +
        `${snap.file_count} notes to their previous state.`,
    );
    if (!ok) return;
    try {
      const r = await lcRollback(corpusId, snap.snapshot_path);
      statusMessage = `Restored ${r.files_restored} notes, deleted ${r.index_notes_deleted} index notes.`;
      if (r.files_skipped.length > 0) {
        statusMessage += ` ${r.files_skipped.length} skipped.`;
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
        "Your own notes and tags are not affected.",
    );
    if (!ok) return;
    try {
      const r = await lcClean(corpusId);
      statusMessage = `Removed Sovereign tags from ${r.tags_removed_from} notes, deleted ${r.index_notes_deleted} index notes.`;
      onReset?.();
    } catch (e) {
      error = `Clean failed: ${e}`;
    }
  }
</script>

<div class="danger">
  {#if error}
    <p class="notice is-error">{error}</p>
  {/if}
  {#if statusMessage}
    <p class="notice is-ok">{statusMessage}</p>
  {/if}

  {#if loading}
    <p class="loading">Loading restore points…</p>
  {:else}
    {#if snapshots.length > 0}
      <section class="section">
        <header class="section-head">
          <h4 class="section-title">Restore to an earlier state</h4>
          <p class="section-desc">
            Before every write, Sovereign saves a snapshot of your notes'
            frontmatter. Restoring rolls back all tags from that run and
            restores the previous state exactly.
          </p>
        </header>
        <ol class="snapshots">
          {#each snapshots as snap (snap.snapshot_path)}
            <li class="snapshot">
              <div class="snap-info">
                <span class="snap-date">{formatRelative(snap.taken_at)}</span>
                <span class="snap-meta lk-folio">
                  {snap.file_count} notes · version {snap.sovereign_version}
                  {#if snap.git_commit}
                    · git <code>{snap.git_commit.slice(0, 7)}</code>
                  {/if}
                </span>
              </div>
              <button
                class="lk-btn lk-btn--quiet"
                onclick={() => confirmRollback(snap)}
              >
                Restore
              </button>
            </li>
          {/each}
        </ol>
      </section>

      <hr class="lk-rule-h section-rule" />
    {/if}

    <section class="section clean-section">
      <div class="clean-text">
        <h4 class="section-title">Remove Sovereign tags</h4>
        <p class="section-desc">
          Strips every <code>sovereign/*</code> tag and deletes the generated
          index notes. Your own notes and tags are untouched.
        </p>
      </div>
      <button class="lk-btn lk-btn--quiet danger-btn" onclick={confirmClean}>
        Remove tags
      </button>
    </section>
  {/if}
</div>

<style>
  .danger {
    padding: 4px 0 10px;
  }
  .notice {
    margin: 0 0 14px;
    padding: 10px 12px;
    font-size: var(--lk-size-meta);
    border-left: 3px solid;
    border-radius: var(--radius);
  }
  .notice.is-error {
    border-color: var(--lk-err);
    background: var(--lk-err-wash);
    color: var(--lk-ink);
  }
  .notice.is-ok {
    border-color: var(--lk-crown);
    background: var(--lk-crown-wash);
    color: var(--lk-ink);
  }
  .loading {
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
  }

  .section { margin-bottom: 16px; }
  .section-rule { margin: 16px 0; }
  .section-head { margin-bottom: 10px; }
  .section-title {
    margin: 0 0 4px;
    font-size: var(--lk-size-body);
    font-weight: 500;
    color: var(--lk-ink);
  }
  .section-desc {
    margin: 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-soft);
    line-height: 1.5;
    max-width: 60ch;
  }

  .snapshots {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .snapshot {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding: 8px 12px;
    border: 1px solid var(--lk-rule);
    background: var(--lk-paper);
    border-radius: var(--radius);
  }
  .snap-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .snap-date {
    font-size: var(--lk-size-meta);
    color: var(--lk-ink);
  }
  .snap-meta {
    color: var(--lk-ink-faded);
  }
  .snap-meta code {
    font-family: var(--lk-font-mono);
    padding: 0 2px;
    color: var(--lk-stamp-ink);
    background: transparent;
  }

  .clean-section {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 14px;
    align-items: center;
  }
  .danger-btn {
    border-color: var(--lk-err);
    color: var(--lk-err);
  }
  .danger-btn:hover:not(:disabled) {
    background: var(--lk-err-wash);
    border-color: var(--lk-err);
    color: var(--lk-err);
  }

  code {
    font-family: var(--lk-font-mono);
    background: transparent;
  }
</style>
