<script lang="ts">
  import PositionBadge from "./PositionBadge.svelte";
  import type { InsightNodeDto, InsightSinkState } from "../types";
  import { insightStore } from "../stores/insights.svelte";

  interface Props {
    node: InsightNodeDto;
    sinkConnected?: boolean;
  }

  let { node, sinkConnected = false }: Props = $props();

  interface SyncLabel {
    text: string;
    style: "local" | "synced" | "pending" | "error";
  }

  let syncLabel: SyncLabel = $derived(getSyncLabel(node.sink_state, sinkConnected));

  function getSyncLabel(
    state: InsightSinkState,
    connected: boolean,
  ): SyncLabel {
    if (!connected || state === "Local")
      return { text: "local", style: "local" };
    if (state === "PendingSync") return { text: "syncing\u2026", style: "pending" };
    if (typeof state === "object") {
      if ("Synced" in state) return { text: "synced", style: "synced" };
      if ("SyncFailed" in state) return { text: "sync failed", style: "error" };
    }
    return { text: "local", style: "local" };
  }

  function positionBorderColor(): string {
    if (!node.position) return "";
    const s = node.position.style;
    if (s === "Compatibilism") return "var(--pos-compat-border)";
    if (s === "HardIncompatibilism") return "var(--pos-incompat-border)";
    if (s === "Libertarianism") return "var(--pos-libert-border)";
    if (typeof s === "object" && "Custom" in s) return s.Custom.border;
    return "var(--border-mid)";
  }

  function formatRelativeTime(iso: string): string {
    const date = new Date(iso);
    const now = Date.now();
    const diffMs = now - date.getTime();
    const diffMin = Math.floor(diffMs / 60000);
    if (diffMin < 1) return "just now";
    if (diffMin < 60) return `${diffMin}m ago`;
    const diffHr = Math.floor(diffMin / 60);
    if (diffHr < 24) return `${diffHr}h ago`;
    const diffDay = Math.floor(diffHr / 24);
    return `${diffDay}d ago`;
  }

  async function handleDelete() {
    await insightStore.remove(node.id);
  }
</script>

<div
  class="sv-clip-card"
  class:has-position={!!node.position}
  style={node.position ? `border-left: 2px solid ${positionBorderColor()}` : ""}
>
  {#if node.position}
    <PositionBadge name={node.position.name} style={node.position.style} />
  {/if}

  <p class="sv-clip-text">{node.clipped_text}</p>

  <div class="sv-clip-meta">
    <span>{node.source.article_title ?? node.source.corpus_id ?? "Unknown"}</span>
    <span class="sv-meta-dot"></span>
    <span>{formatRelativeTime(node.created_at)}</span>
    <span class="sv-sync-tag {syncLabel.style}">{syncLabel.text}</span>
    <button class="sv-delete-btn" onclick={handleDelete} title="Remove insight"
      >&times;</button
    >
  </div>

  {#if node.adjacent.length > 0}
    <div class="sv-adj-tags">
      {#each node.adjacent as tag}
        <span class="sv-adj-tag">{tag}</span>
      {/each}
    </div>
  {/if}
</div>

<style>
  .sv-clip-card {
    background: var(--bg-primary);
    border: 0.5px solid var(--border-mid);
    border-radius: var(--radius);
    padding: 11px;
    margin-bottom: 9px;
  }

  .sv-clip-text {
    font-family: var(--font-serif);
    font-size: 12.5px;
    line-height: 1.6;
    color: var(--text-primary);
    margin: 5px 0 8px;
    display: -webkit-box;
    -webkit-line-clamp: 4;
    line-clamp: 4;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .sv-clip-meta {
    font-size: 11px;
    color: var(--text-muted);
    display: flex;
    align-items: center;
    gap: 4px;
    flex-wrap: wrap;
    margin-bottom: 7px;
    font-family: var(--font-sans);
  }

  .sv-meta-dot {
    width: 3px;
    height: 3px;
    border-radius: 50%;
    background: var(--border-mid);
  }

  .sv-sync-tag {
    font-size: 10px;
    padding: 1px 5px;
    border-radius: 999px;
  }

  .sv-sync-tag.local {
    background: var(--bg-surface);
    border: 0.5px solid var(--border-mid);
    color: var(--text-muted);
  }

  .sv-sync-tag.synced {
    background: rgba(99, 153, 34, 0.08);
    border: 0.5px solid var(--pos-compat-border);
    color: var(--pos-compat-text);
  }

  .sv-sync-tag.error {
    background: rgba(201, 95, 120, 0.08);
    border: 0.5px solid var(--error);
    color: var(--error);
  }

  .sv-delete-btn {
    margin-left: auto;
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 14px;
    padding: 0 2px;
    opacity: 0;
    transition: opacity 0.15s, color 0.15s;
  }

  .sv-clip-card:hover .sv-delete-btn {
    opacity: 1;
  }

  .sv-delete-btn:hover {
    color: var(--error);
  }

  .sv-adj-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .sv-adj-tag {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 999px;
    background: var(--bg-surface);
    border: 0.5px solid var(--border-mid);
    color: var(--text-muted);
    font-family: var(--font-sans);
  }
</style>
