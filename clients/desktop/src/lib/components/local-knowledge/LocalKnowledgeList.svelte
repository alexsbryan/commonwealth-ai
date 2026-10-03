<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { LocalCorpusConfig, StarterQuestion } from "../../types";
  import OrganizerPanel from "./obsidian/OrganizerPanel.svelte";

  interface Props {
    corpora: LocalCorpusConfig[];
    onRemove: (id: string) => void;
    onOpenChatWithSeed?: (question: StarterQuestion) => void;
  }

  let { corpora, onRemove, onOpenChatWithSeed }: Props = $props();

  let expandedId: string | null = $state(null);

  // `source_type` is an externally-tagged union: the unit variant
  // `DocumentFolder` serializes as the bare string "DocumentFolder", while
  // the data-carrying variants serialize as a single-key object
  // (`{ObsidianVault: {...}}` / `{WatchedFolder: {...}}`). The old
  // `!== "DocumentFolder" ⇒ vault` shortcut therefore mislabeled EVERY
  // watched folder as an Obsidian vault (and lit its vault-only "Organize"
  // button). Discriminate explicitly instead.
  function sourceKind(
    c: LocalCorpusConfig,
  ): "folder" | "obsidian" | "watched" {
    const st = c.source_type;
    if (st === "DocumentFolder") return "folder";
    if (st && typeof st === "object") {
      if ("ObsidianVault" in st) return "obsidian";
      if ("WatchedFolder" in st) return "watched";
    }
    return "folder";
  }

  function sourceLabel(c: LocalCorpusConfig): string {
    switch (sourceKind(c)) {
      case "obsidian":
        return "Obsidian vault";
      case "watched":
        return "Watched folder";
      default:
        return "Folder";
    }
  }

  // Only a true Obsidian vault gets the tag/link Organizer panel.
  function isVault(c: LocalCorpusConfig): boolean {
    return sourceKind(c) === "obsidian";
  }

  function toggleExpand(id: string) {
    expandedId = expandedId === id ? null : id;
  }
</script>

{#if corpora.length === 0}
  <p class="empty">
    Nothing connected yet. Drop a folder or vault below to begin.
  </p>
{:else}
  <ul class="sources">
    {#each corpora as c (c.id)}
      <li class="source" class:source--expanded={expandedId === c.id}>
        <div class="row">
          <div class="info">
            <h3 class="name">{c.display_name}</h3>
            <p class="meta">
              <span class="kind">{sourceLabel(c)}</span>
              <span class="sep">·</span>
              <span class="path lk-folio" title={c.root_path}>{c.root_path}</span>
            </p>
          </div>
          <div class="actions">
            {#if isVault(c)}
              <button
                class="lk-btn lk-btn--mark"
                onclick={() => toggleExpand(c.id)}
                aria-expanded={expandedId === c.id}
              >
                {expandedId === c.id ? "Close" : "Organize"}
              </button>
            {/if}
            <button class="lk-btn lk-btn--quiet" onclick={() => onRemove(c.id)}>
              Remove
            </button>
          </div>
        </div>

        {#if expandedId === c.id && isVault(c)}
          <div class="expanded">
            <OrganizerPanel config={c} {onOpenChatWithSeed} />
          </div>
        {/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .empty {
    margin: 0;
    padding: 16px 0;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
  }

  .sources {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .source {
    border-bottom: 1px solid var(--lk-rule);
  }
  .source:last-child {
    border-bottom: 0;
  }
  .source--expanded {
    background: var(--lk-paper-subtle);
  }

  .row {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 16px;
    padding: 14px 4px;
    align-items: center;
  }
  .info {
    min-width: 0;
  }
  .name {
    margin: 0 0 2px;
    font-size: var(--lk-size-lead);
    font-weight: 500;
    color: var(--lk-ink);
    line-height: 1.25;
  }
  .meta {
    margin: 0;
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: var(--lk-size-meta);
    color: var(--lk-ink-faded);
    min-width: 0;
  }
  .kind {
    color: var(--lk-ink-soft);
  }
  .sep {
    color: var(--lk-ink-faded);
  }
  .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  .actions {
    display: flex;
    gap: 8px;
  }

  .expanded {
    padding: 0 4px 20px;
    animation: lk-fade-in 250ms ease-out both;
  }
</style>
