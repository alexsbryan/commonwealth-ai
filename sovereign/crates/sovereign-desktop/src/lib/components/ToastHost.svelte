<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  ToastHost — renders `toastStore.current` as a fixed-position
  notification at the bottom-center of the app. Mounted once at the
  App.svelte root.

  Pure view over the store. The primary action (when present) fires
  the payload-bound handler and dismisses the toast atomically via
  `toastStore._consume()`.
-->
<script lang="ts">
  import { toastStore } from "../stores/toast.svelte";

  async function handleAction() {
    const t = toastStore.current;
    if (!t) return;
    const consumed = toastStore._consume(t.id);
    consumed?.onAction?.();
  }

  function handleDismiss() {
    toastStore.clear();
  }
</script>

{#if toastStore.current}
  {@const t = toastStore.current}
  <div
    class="toast"
    role="status"
    aria-live="polite"
    aria-atomic="true"
  >
    <div class="toast-body">
      <p class="toast-title">{t.title}</p>
      {#if t.body}
        <p class="toast-sub">{t.body}</p>
      {/if}
    </div>
    <div class="toast-actions">
      {#if t.actionLabel && t.onAction}
        <button class="toast-action" onclick={handleAction}>
          {t.actionLabel}
        </button>
      {/if}
      <button
        class="toast-close"
        onclick={handleDismiss}
        aria-label="Dismiss"
      >
        ×
      </button>
    </div>
  </div>
{/if}

<style>
  /* Rendered as a gold-bordered stamp on dark plum — reads as a
     letterpress seal rather than a web toast. Bottom-center so it
     sits in the reader's natural focus without covering content. */
  .toast {
    position: fixed;
    bottom: 28px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 60;
    display: flex;
    gap: 18px;
    align-items: flex-start;
    padding: 14px 20px;
    min-width: 360px;
    max-width: 620px;
    background: var(--bg-elevated);
    color: var(--text-primary);
    border: 1px solid var(--accent);
    border-radius: 10px;
    box-shadow:
      0 8px 28px rgba(0, 0, 0, 0.5),
      0 0 0 4px var(--bg-root),
      inset 0 1px 0 rgba(223, 192, 104, 0.18),
      0 0 40px var(--accent-glow);
    animation: toast-rise 320ms cubic-bezier(0.2, 0.8, 0.2, 1) both;
  }
  .toast-body {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .toast-title {
    margin: 0;
    font-family: var(--font-serif);
    font-style: italic;
    font-size: 1.04rem;
    font-weight: 500;
    line-height: 1.25;
    color: var(--accent-light);
    letter-spacing: -0.005em;
  }
  .toast-sub {
    margin: 0;
    font-size: 0.85rem;
    color: var(--text-secondary);
    line-height: 1.45;
  }
  .toast-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }
  /* Primary action: gold-foil chip that matches the main CTA
     styling so it reads as the press-of-approval. */
  .toast-action {
    background: var(--accent);
    color: var(--text-on-accent);
    border: 1px solid var(--accent);
    padding: 6px 14px;
    border-radius: 999px;
    font-family: var(--font-sans);
    font-size: 0.84rem;
    font-weight: 600;
    letter-spacing: 0.01em;
    cursor: pointer;
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.18),
      0 1px 0 rgba(0, 0, 0, 0.2);
    transition: background 140ms ease, transform 120ms ease;
  }
  .toast-action:hover {
    background: var(--accent-light);
    transform: translateY(-1px);
  }
  .toast-action:active {
    transform: translateY(0);
  }
  .toast-close {
    background: transparent;
    border: 1px solid transparent;
    color: var(--text-muted);
    font-size: 1.15rem;
    line-height: 1;
    width: 24px;
    height: 24px;
    padding: 0;
    cursor: pointer;
    border-radius: 4px;
    transition: color 140ms ease, border-color 140ms ease;
  }
  .toast-close:hover {
    color: var(--text-primary);
    border-color: var(--border-bright);
  }
  @keyframes toast-rise {
    from {
      opacity: 0;
      transform: translate(-50%, 12px);
    }
    to {
      opacity: 1;
      transform: translate(-50%, 0);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .toast { animation: none; }
  }
</style>
