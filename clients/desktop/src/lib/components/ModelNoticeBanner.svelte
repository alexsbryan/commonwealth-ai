<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";

  // Mirrors `state::builders::model_compat::ModelNoticePayload`. Emitted
  // once at boot when the configured chat model uses an architecture that
  // can't run on this machine's CPU (e.g. Qwen3.5 "Gated DeltaNet"), so a
  // dense model was substituted. Informational, not an error — the app is
  // up and working; we just explain the swap instead of silently crashing.
  interface ModelNoticePayload {
    message: string;
    requested_model: string;
    requested_arch: string;
    running_model: string;
    running_arch: string;
  }

  let notice: ModelNoticePayload | null = $state(null);
  let dismissed = $state(false);
  let unlisten: UnlistenFn | null = null;

  let visible = $derived(notice !== null && !dismissed);

  onMount(async () => {
    unlisten = await listen<ModelNoticePayload>("model-notice", (event) => {
      notice = event.payload;
      dismissed = false;
    });
  });

  onDestroy(() => {
    if (unlisten) unlisten();
  });
</script>

{#if visible && notice}
  <div class="banner" role="status">
    <div class="banner-body">
      <span class="banner-title">Swapped in a model this machine can run</span>
      <span class="banner-text">{notice.message}</span>
    </div>
    <button class="action" onclick={() => (dismissed = true)}>Got it</button>
  </div>
{/if}

<style>
  /* Info-styled sibling of ReconnectBanner — same fixed placement, calmer
     palette. Never latches to a failure colour: a substitution is a
     successful degrade, not a crash. */
  .banner {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 8px 16px;
    background: oklch(95% 0.035 240);
    color: oklch(30% 0.08 250);
    font-family: var(--font-sans);
    font-size: 0.85rem;
    border-bottom: 1px solid oklch(82% 0.06 250 / 0.6);
    -webkit-font-smoothing: antialiased;
  }

  .banner-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1 1 auto;
    min-width: 0;
  }

  .banner-title {
    font-weight: 600;
    letter-spacing: 0.01em;
  }

  .banner-text {
    opacity: 0.9;
  }

  .action {
    flex: 0 0 auto;
    font-family: inherit;
    font-size: 0.78rem;
    font-weight: 500;
    letter-spacing: 0.05em;
    color: inherit;
    background: oklch(100% 0 0 / 0.35);
    border: 1px solid currentColor;
    padding: 4px 12px;
    border-radius: 4px;
    cursor: pointer;
    transition: background 160ms ease;
  }

  .action:hover {
    background: oklch(100% 0 0 / 0.6);
  }
</style>
