<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import {
    lcWatchConfirmDeletion,
    lcWatchRemove,
    lcWatchResume,
  } from "../../api";
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

  // Full removal for a wedged watched folder: deregisters it from the sweep
  // scheduler AND wipes the (possibly incomplete) index — the escape hatch
  // for the "initial ingest never finished" error, whose own text tells the
  // user to remove + re-add but which the banner previously gave no way to
  // do. `lcWatchRemove` → DELETE /internal/corpus/watch/{id} is idempotent
  // and tolerates a missing `_corpus_meta.json`, so it always clears.
  async function remove(id: string) {
    if (
      !window.confirm(
        "Remove this watched folder? Its index is deleted and sweeping " +
          "stops. Your original files are untouched — you can re-add it to " +
          "start over.",
      )
    )
      return;
    inflight = id;
    actionError = null;
    try {
      await lcWatchRemove(id);
      await onChanged();
    } catch (e) {
      actionError = String(e);
    }
    inflight = null;
  }

  // The raw errored `message` is a developer-facing diagnostic that can run
  // several sentences (path + cause + remediation) — rendered verbatim it
  // overflowed the banner. Show a bounded one-line headline; the full text
  // stays available via the row's `title` tooltip (see the template).
  function errorHeadline(message: string): string {
    const cleaned = message.replace(/^watched_folder:\s*/, "").trim();
    // Our worker errors read "<what> at <path> — <cause>. <remediation>".
    // The user-meaningful part is the <cause> after the em-dash; prefer it so
    // the headline drops the path dump. The remediation clause is now
    // embodied by the Remove button, so we cut at the first sentence.
    const afterDash = cleaned.includes(" — ")
      ? cleaned.slice(cleaned.indexOf(" — ") + 3)
      : cleaned;
    const firstSentence = afterDash.split(". ")[0].trim();
    return firstSentence.length > 140
      ? `${firstSentence.slice(0, 139)}…`
      : firstSentence;
  }

  // Concise, bounded reason line for the row. Errored rows also expose the
  // full message via `fullDetail` (tooltip).
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
        return `Last sweep errored: ${errorHeadline(s.message)}`;
      default:
        return "Needs attention";
    }
  }

  // Full (untrimmed) detail for the tooltip; empty for non-errored states.
  function fullDetail(entry: WatchedFolderListEntry): string {
    return entry.status.kind === "errored" ? entry.status.message : "";
  }
</script>

{#if blocked.length > 0}
  <section class="banner">
    <header class="head">
      <span class="dot" aria-hidden="true">●</span>
      <h2 class="title">
        {blocked.length === 1
          ? "1 live folder needs attention"
          : `${blocked.length} live folders need attention`}
      </h2>
    </header>

    <ul class="list">
      {#each blocked as entry (entry.corpus_id)}
        <li class="item">
          <div class="info">
            <span class="name">{entry.display_name}</span>
            <span class="reason" title={fullDetail(entry)}>{summary(entry)}</span>
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
                title="Try the sweep again on the next tick. Won't help if the initial ingest never completed — use Remove to start over."
              >
                Retry
              </button>
              <button
                class="ghost danger"
                onclick={() => remove(entry.corpus_id)}
                disabled={inflight === entry.corpus_id}
                title="Stop watching and delete this incomplete index. Your files are untouched."
              >
                Remove
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
    /* Bound a long errored diagnostic to two lines so it can never
       overflow the banner; the full text lives in the `title` tooltip. */
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    overflow-wrap: anywhere;
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
  .ghost.danger { color: var(--lk-err); }
  .ghost.danger:hover:not(:disabled) { border-color: var(--lk-err); }
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
