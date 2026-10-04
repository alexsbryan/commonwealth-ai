<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  DraftPreview — the draft-stream experiment's visual affordance
  (SOVEREIGN_DRAFT_STREAM=1). Renders the UNVERIFIED draft as it forms
  behind the grounding gate, in an explicitly-provisional presentation
  (thinking-section style): dimmed, italic-headed, watermarked
  "unverified", never stylable as a final answer. Unmounts when the
  gated answer starts streaming (ChatView renders it only while
  loading), which reads as the draft collapsing into the real reply.

  The affordance contract (product decision, 2026-07-11): drafted
  output may be shown ONLY with presentation that makes non-finality
  unmistakable. If you restyle this component, keep that contract.
-->
<script lang="ts">
  import { routingStore } from "../stores/routing.svelte";

  let container: HTMLDivElement | undefined = $state();
  const draft = $derived(routingStore.draftPreview);

  // Follow the tail as text streams in (the user is watching it form).
  $effect(() => {
    if (draft && container) {
      container.scrollTop = container.scrollHeight;
    }
  });
</script>

{#if draft}
  <div class="draft-preview" aria-label="Unverified draft being checked">
    <div class="draft-header">
      <span class="draft-mark pulse">✎</span>
      <span class="draft-label">Drafting — unverified, being checked against your sources…</span>
    </div>
    <div class="draft-body" bind:this={container}>{draft}</div>
  </div>
{/if}

<style>
  .draft-preview {
    margin: 0.5rem 0;
    border: 1px dashed var(--border-color, rgba(128, 128, 128, 0.35));
    border-radius: 8px;
    background: var(--surface-dim, rgba(128, 128, 128, 0.06));
    overflow: hidden;
  }
  .draft-header {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.4rem 0.75rem;
    font-size: 0.78rem;
    font-style: italic;
    opacity: 0.75;
    border-bottom: 1px dashed var(--border-color, rgba(128, 128, 128, 0.25));
  }
  .draft-mark.pulse {
    animation: draft-pulse 1.6s ease-in-out infinite;
  }
  @keyframes draft-pulse {
    0%,
    100% {
      opacity: 0.4;
    }
    50% {
      opacity: 1;
    }
  }
  .draft-body {
    max-height: 14rem;
    overflow-y: auto;
    padding: 0.6rem 0.75rem;
    white-space: pre-wrap;
    font-size: 0.86rem;
    line-height: 1.45;
    opacity: 0.62;
  }
</style>
