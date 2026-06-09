<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { insightStore } from "../stores/insights.svelte";
  import { sinkStore } from "../stores/sinks.svelte";
  import InsightCard from "./InsightCard.svelte";
  import ExploreButton from "./ExploreButton.svelte";
  import VaultConnector from "./VaultConnector.svelte";

  interface Props {
    conversationId: string | null;
    onNavigate: (conversationId: string) => void;
    onClose: () => void;
  }

  let { conversationId, onNavigate, onClose }: Props = $props();

  $effect(() => {
    insightStore.init();
    sinkStore.load();
  });
</script>

<aside class="insights-panel">
  <div class="panel-header">
    <h3 class="panel-title">&#x25C8; Insights</h3>
    {#if insightStore.count > 0}
      <span class="count-badge">{insightStore.count}</span>
    {/if}
    <button class="close-btn" onclick={onClose} title="Close panel">&times;</button>
  </div>

  <div class="insights-list">
    {#if insightStore.count === 0}
      <div class="empty-state">
        <div class="empty-mark">&#x25C8;</div>
        <p>Clip paragraphs with &#x25C8; to capture insights</p>
      </div>
    {:else}
      {#each insightStore.items as node (node.id)}
        <InsightCard {node} sinkConnected={sinkStore.anyConnected} />
      {/each}
      <ExploreButton {onNavigate} />
    {/if}
  </div>

  <div class="panel-footer">
    <VaultConnector />
  </div>
</aside>

<style>
  .insights-panel {
    width: 280px;
    min-width: 280px;
    background: var(--bg-secondary);
    border-left: 1px solid var(--border-mid);
    display: flex;
    flex-direction: column;
    height: 100%;
  }

  .panel-header {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 14px 14px 10px;
    border-bottom: 1px solid var(--border);
  }

  .panel-title {
    font-family: var(--font-sans);
    font-size: 0.8rem;
    font-weight: 700;
    letter-spacing: 0.1em;
    color: var(--accent);
    text-transform: uppercase;
    flex: 1;
  }

  .count-badge {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--accent-glow);
    border: 0.5px solid color-mix(in srgb, var(--accent) 30%, transparent);
    color: var(--accent);
    font-family: var(--font-mono);
  }

  .close-btn {
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 16px;
    padding: 0 2px;
  }

  .close-btn:hover {
    color: var(--text-secondary);
  }

  .insights-list {
    flex: 1;
    overflow-y: auto;
    padding: 10px 10px 4px;
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    padding: 40px 20px;
    gap: 12px;
  }

  .empty-mark {
    font-size: 2rem;
    color: var(--text-muted);
    opacity: 0.3;
  }

  .empty-state p {
    font-size: 12px;
    color: var(--text-muted);
    line-height: 1.5;
  }

  .panel-footer {
    padding: 10px 14px;
    border-top: 1px solid var(--border);
    display: flex;
    justify-content: center;
  }
</style>
