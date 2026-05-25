<script lang="ts">
  /// Sliding drawer surfacing past inner-work entries and debug
  /// actions for today's session. Slides in from the left when
  /// opened; the rest of the surface stays visible behind a soft
  /// scrim. Closes on Esc, on backdrop click, or on the close button.
  ///
  /// The list is a flat array of `{ dateIso, conversationId, dateLabel,
  /// preview, isToday }` resolved by the parent — this component is
  /// purely presentational so a future change to the indexing source
  /// (today: localStorage; later: a backend tag) doesn't need to
  /// touch the drawer.
  ///
  /// Debug actions sit in the footer, gated on `isOnToday`. "Reset
  /// today's session" is non-destructive (clears local draft + the
  /// date→id map; the conversation stays in the store and remains
  /// reachable via this same drawer). "Delete today's entry…" is
  /// destructive and confirms before firing — it removes the
  /// conversation from the store entirely.

  export interface DrawerEntry {
    dateIso: string;
    conversationId: string;
    dateLabel: string;
    preview: string;
    isCurrent: boolean;
  }

  interface Props {
    open: boolean;
    loading: boolean;
    entries: DrawerEntry[];
    /// True when the surface is currently viewing today's date —
    /// gates the debug actions and hides the "← Today" button.
    isOnToday: boolean;
    onClose: () => void;
    onSelect: (entry: DrawerEntry) => void;
    onReturnToToday: () => void;
    onResetToday: () => void;
    onDeleteToday: () => void;
  }

  let {
    open,
    loading,
    entries,
    isOnToday,
    onClose,
    onSelect,
    onReturnToToday,
    onResetToday,
    onDeleteToday,
  }: Props = $props();

  // Inline two-step confirm for the destructive delete action.
  // We don't use Tauri's WebView `window.confirm()` because it returns
  // the user's choice unreliably under the WKWebView bridge — Cancel
  // observed to delete anyway. A native dialog from `@tauri-apps/plugin-dialog`
  // would work, but a system modal lands jarringly in this surface's
  // quiet aesthetic. The two-step inline pattern keeps the
  // confirmation in the same column as the action and is the standard
  // way destructive UI gets armed (GitHub-style "type the name to
  // confirm" is overkill for a debug action with an undelete escape
  // valve — the conversation can be re-summoned by writing again).
  let confirmingDelete = $state(false);

  function handleDeleteClick() {
    if (!confirmingDelete) {
      confirmingDelete = true;
      return;
    }
    confirmingDelete = false;
    onDeleteToday();
  }

  function cancelDeleteConfirm() {
    confirmingDelete = false;
  }

  // Auto-disarm: drawer closing or the user leaving today's view both
  // invalidate the armed state. Without this, opening the drawer on a
  // past entry could carry over a confirm armed from a prior session.
  $effect(() => {
    if (!open || !isOnToday) {
      confirmingDelete = false;
    }
  });
</script>

<aside
  class="drawer"
  class:open
  aria-label="Past inner-work entries"
  aria-hidden={!open}
>
  <button
    type="button"
    class="backdrop"
    aria-label="Close past entries"
    tabindex={open ? 0 : -1}
    onclick={onClose}
  ></button>

  <div class="panel" role="dialog" aria-modal="true" aria-labelledby="iw-history-title">
    <header class="head">
      <span id="iw-history-title" class="title">Past entries</span>
      <button
        type="button"
        class="close"
        onclick={onClose}
        aria-label="Close"
        title="Esc"
      >×</button>
    </header>

    {#if !isOnToday}
      <button
        type="button"
        class="today-btn"
        onclick={onReturnToToday}
      >← Today</button>
    {/if}

    <div class="list-wrap">
      {#if loading}
        <p class="empty">Loading…</p>
      {:else if entries.length === 0}
        <p class="empty">No entries yet. Each Cmd+Return on a new date adds one.</p>
      {:else}
        <ul class="list">
          {#each entries as entry (entry.conversationId)}
            <li>
              <button
                type="button"
                class="entry"
                class:current={entry.isCurrent}
                onclick={() => onSelect(entry)}
              >
                <span class="date">{entry.dateLabel}</span>
                {#if entry.preview}
                  <span class="preview">{entry.preview}</span>
                {/if}
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <footer class="foot">
      <span class="footer-label">debug</span>
      <button
        type="button"
        class="debug-btn"
        onclick={onResetToday}
        disabled={!isOnToday}
        title="Clear today's draft and unbind today's conversation. The conversation itself stays in the store."
      >
        Reset today's session
      </button>

      {#if !confirmingDelete}
        <button
          type="button"
          class="debug-btn destructive"
          onclick={handleDeleteClick}
          disabled={!isOnToday}
          title="Delete today's inner-work conversation from the store."
        >
          Delete today's entry…
        </button>
      {:else}
        <div class="confirm-row" role="group" aria-label="Confirm delete">
          <button
            type="button"
            class="debug-btn destructive armed"
            onclick={handleDeleteClick}
          >
            Confirm delete
          </button>
          <button
            type="button"
            class="debug-btn"
            onclick={cancelDeleteConfirm}
          >
            Cancel
          </button>
        </div>
      {/if}
    </footer>
  </div>
</aside>

<style>
  /* Inherits the inner-work palette from `.root` in the surface. The
     drawer is positioned fixed against the viewport so it doesn't
     scroll with the document. */
  .drawer {
    position: fixed;
    inset: 0;
    pointer-events: none;
    z-index: 4;
  }

  .drawer.open {
    pointer-events: auto;
  }

  .backdrop {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    background: oklch(15% 0.005 250 / 0);
    border: 0;
    padding: 0;
    cursor: default;
    transition: background 320ms ease;
  }

  .drawer.open .backdrop {
    background: oklch(15% 0.005 250 / 0.18);
    cursor: pointer;
  }

  @media (prefers-color-scheme: dark) {
    .drawer.open .backdrop {
      background: oklch(8% 0.005 250 / 0.4);
    }
  }

  .panel {
    position: absolute;
    top: 0;
    /* Start flush with the nav rail's right edge so the panel doesn't
       slide out behind it. --nav-rail-width is defined in app.css. */
    left: var(--nav-rail-width, 60px);
    height: 100%;
    width: 360px;
    max-width: calc(80vw - var(--nav-rail-width, 60px));
    display: flex;
    flex-direction: column;
    background: var(--inner-bg-warm);
    color: var(--inner-ink);
    transform: translateX(-100%);
    transition: transform 320ms cubic-bezier(0.2, 0.8, 0.2, 1);
    box-shadow: 0 0 0 1px var(--inner-rule), 4px 0 18px oklch(15% 0.005 250 / 0.06);
    font-family: var(--inner-font-sans);
  }

  .drawer.open .panel {
    transform: translateX(0);
  }

  @media (prefers-reduced-motion: reduce) {
    .panel,
    .backdrop {
      transition: none;
    }
  }

  .head {
    display: flex;
    align-items: baseline;
    padding: 1.4rem 1.5rem 1rem;
    gap: 0.75em;
  }

  .title {
    font-variant: small-caps;
    letter-spacing: 0.06em;
    color: var(--inner-ink);
    font-size: 0.98em;
  }

  .close {
    margin-left: auto;
    background: transparent;
    border: 0;
    color: var(--inner-ink-faint);
    font-size: 1.2em;
    line-height: 1;
    padding: 0 6px;
    cursor: pointer;
    border-radius: 3px;
    opacity: 0.7;
    transition: opacity 200ms ease, color 200ms ease;
  }

  .close:hover,
  .close:focus-visible {
    opacity: 1;
    color: var(--inner-ink-muted);
    outline: none;
  }

  .close:focus-visible {
    box-shadow: 0 0 0 2px var(--inner-focus);
  }

  .today-btn {
    margin: 0 1.5rem 0.75rem;
    padding: 0.4em 0.75em;
    background: transparent;
    border: 0;
    border-radius: 4px;
    color: var(--inner-ink-muted);
    font: inherit;
    font-size: 0.92em;
    text-align: left;
    cursor: pointer;
    transition: background 200ms ease, color 200ms ease;
  }

  .today-btn:hover,
  .today-btn:focus-visible {
    color: var(--inner-ink);
    background: oklch(from var(--inner-bg-cool) calc(l - 0.02) c h);
    outline: none;
  }

  .list-wrap {
    flex: 1 1 auto;
    overflow-y: auto;
    padding: 0 0.75rem 0.75rem;
  }

  .empty {
    color: var(--inner-ink-faint);
    font-style: italic;
    margin: 1rem 0.75rem;
    font-size: 0.9em;
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .list li {
    margin: 0;
  }

  .entry {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 0.25em;
    width: 100%;
    padding: 0.7em 0.85em;
    background: transparent;
    border: 0;
    border-radius: 4px;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    transition: background 180ms ease;
  }

  .entry:hover,
  .entry:focus-visible {
    background: oklch(from var(--inner-bg-cool) calc(l - 0.025) c h);
    outline: none;
  }

  .entry:focus-visible {
    box-shadow: 0 0 0 2px var(--inner-focus);
  }

  .entry.current {
    background: oklch(from var(--inner-bg-cool) calc(l - 0.04) c h);
  }

  .date {
    color: var(--inner-ink);
    font-size: 0.95em;
  }

  .preview {
    color: var(--inner-ink-muted);
    font-size: 0.85em;
    line-height: 1.45;
    /* Two-line preview clamp via classic line-clamp pattern. */
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .foot {
    display: flex;
    flex-direction: column;
    gap: 0.4em;
    padding: 0.85rem 1.5rem 1.25rem;
    border-top: 1px solid var(--inner-rule);
  }

  .footer-label {
    color: var(--inner-ink-faint);
    font-variant: small-caps;
    letter-spacing: 0.08em;
    font-size: 0.78em;
    margin-bottom: 0.2em;
  }

  .debug-btn {
    width: 100%;
    padding: 0.55em 0.75em;
    background: transparent;
    border: 1px solid var(--inner-rule);
    border-radius: 4px;
    color: var(--inner-ink-muted);
    font: inherit;
    font-size: 0.88em;
    text-align: left;
    cursor: pointer;
    transition: background 180ms ease, color 180ms ease, border-color 180ms ease;
  }

  .debug-btn:hover:not(:disabled),
  .debug-btn:focus-visible:not(:disabled) {
    color: var(--inner-ink);
    background: oklch(from var(--inner-bg-cool) calc(l - 0.025) c h);
    outline: none;
  }

  .debug-btn:focus-visible {
    box-shadow: 0 0 0 2px var(--inner-focus);
  }

  .debug-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .debug-btn.destructive:hover:not(:disabled),
  .debug-btn.destructive:focus-visible:not(:disabled) {
    color: oklch(55% 0.12 25);
    border-color: oklch(55% 0.12 25 / 0.5);
  }

  /* When armed, the destructive button signals its readiness with a
     filled background — the second click will commit. Paired with a
     "Cancel" of equal weight so the user has an obvious off-ramp,
     answering the parent fix: a confirm Cancel must actually cancel. */
  .debug-btn.destructive.armed,
  .debug-btn.destructive.armed:hover,
  .debug-btn.destructive.armed:focus-visible {
    color: oklch(98% 0.005 250);
    background: oklch(55% 0.12 25);
    border-color: oklch(55% 0.12 25);
  }

  .confirm-row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.4em;
  }
</style>
