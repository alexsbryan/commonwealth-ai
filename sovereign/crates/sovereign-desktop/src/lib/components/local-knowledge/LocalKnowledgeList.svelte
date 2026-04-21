<script lang="ts">
  import type { LocalCorpusConfig } from "../../types";
  import OrganizerPanel from "./obsidian/OrganizerPanel.svelte";

  interface Props {
    corpora: LocalCorpusConfig[];
    onRemove: (id: string) => void;
  }

  let { corpora, onRemove }: Props = $props();

  let expandedId: string | null = $state(null);

  function sourceLabel(c: LocalCorpusConfig): string {
    if (c.source_type === "DocumentFolder") return "Folder";
    return "Obsidian vault";
  }

  function isVault(c: LocalCorpusConfig): boolean {
    return c.source_type !== "DocumentFolder";
  }

  function toggleExpand(id: string) {
    expandedId = expandedId === id ? null : id;
  }
</script>

{#if corpora.length === 0}
  <p class="empty-state">
    No local knowledge connected yet. Add a folder or vault below.
  </p>
{:else}
  <div class="list">
    {#each corpora as c (c.id)}
      <div class="row-wrap">
        <div class="row">
          <div class="info">
            <div class="name">{c.display_name}</div>
            <div class="meta">
              <span class="source">{sourceLabel(c)}</span>
              <span class="sep">·</span>
              <span class="path" title={c.root_path}>{c.root_path}</span>
            </div>
          </div>
          <div class="actions">
            {#if isVault(c)}
              <button
                class="action-btn"
                onclick={() => toggleExpand(c.id)}
                aria-expanded={expandedId === c.id}
              >
                {expandedId === c.id ? "Hide" : "Organize"}
              </button>
            {/if}
            <button class="remove-btn" onclick={() => onRemove(c.id)}>
              Remove
            </button>
          </div>
        </div>
        {#if expandedId === c.id && isVault(c)}
          <div class="expanded">
            <OrganizerPanel config={c} />
          </div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .empty-state {
    font-size: 13px;
    color: var(--color-text-muted, #6b6b6b);
    font-style: italic;
    padding: 8px 0 0;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 14px;
    border: 1px solid var(--color-border, #d4d4d4);
    border-radius: 6px;
    background: var(--color-surface, #fff);
  }
  .info {
    min-width: 0;
    flex: 1;
  }
  .name {
    font-size: 14px;
    font-weight: 500;
    margin-bottom: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    font-size: 12px;
    color: var(--color-text-muted, #6b6b6b);
    display: flex;
    gap: 6px;
    min-width: 0;
  }
  .path {
    font-family: var(--font-mono, ui-monospace, monospace);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    flex: 1;
  }
  .sep {
    color: var(--color-text-muted, #6b6b6b);
  }
  .row-wrap {
    display: flex;
    flex-direction: column;
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  .action-btn {
    padding: 6px 12px;
    font-size: 13px;
    border: 1px solid var(--color-accent, #3a5fc9);
    background: transparent;
    color: var(--color-accent, #3a5fc9);
    border-radius: 4px;
    cursor: pointer;
  }
  .action-btn:hover {
    background: color-mix(in srgb, var(--color-accent, #3a5fc9) 10%, transparent);
  }
  .expanded {
    padding: 12px 14px 0;
    border-top: 1px solid var(--color-border, #e4e4e4);
    margin-top: -1px;
  }
  .remove-btn {
    padding: 6px 12px;
    font-size: 13px;
    border: 1px solid var(--color-border, #d4d4d4);
    background: transparent;
    border-radius: 4px;
    cursor: pointer;
    color: var(--color-text-muted, #6b6b6b);
  }
  .remove-btn:hover {
    border-color: var(--color-error, #c92a2a);
    color: var(--color-error, #c92a2a);
  }
</style>
