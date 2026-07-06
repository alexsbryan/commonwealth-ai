<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  AddSheet — the one place you add a notebook (Phase 1 UX refactor).

  It folds the five ingest paths that used to be scattered across the
  Settings → Knowledge and → Imports tabs, by RE-PARENTING the existing
  surfaces unchanged — no new ingest logic lives here:

    - Your files     → LocalKnowledgeSection (embedded): pick a folder,
                       watch a folder, connect an Obsidian vault.
    - Conversations  → ImportsTab: Claude / ChatGPT exports.
    - Catalog        → KnowledgeStatus: install + manage featured corpora.

  A freshly-ingested source becomes a notebook on the shelf; the parent
  LibraryView reloads `notebook_list` when this sheet closes.
-->
<script lang="ts">
  import LocalKnowledgeSection from "../local-knowledge/LocalKnowledgeSection.svelte";
  import ImportsTab from "../settings/ImportsTab.svelte";
  import KnowledgeStatus from "../KnowledgeStatus.svelte";
  import type { StarterQuestion } from "../../types";

  let {
    onClose,
    onOpenChatWithSeed,
    onDropToChat,
  }: {
    onClose: () => void;
    onOpenChatWithSeed?: (question: StarterQuestion) => void;
    onDropToChat?: () => void;
  } = $props();

  type Section = "files" | "imports" | "catalog";
  let section = $state<Section>("files");

  const SECTIONS: { id: Section; label: string; sub: string }[] = [
    { id: "files", label: "Your files", sub: "Folders, vaults, watched directories" },
    { id: "imports", label: "Conversations", sub: "Import your email, Claude, or ChatGPT" },
    { id: "catalog", label: "Catalog", sub: "Install a ready-made library" },
  ];
</script>

<div class="add-sheet" data-testid="add-sheet">
  <header class="add-header">
    <button class="back" onclick={onClose} data-testid="add-sheet-close" aria-label="Back to Library">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m15 18-6-6 6-6" />
      </svg>
      Library
    </button>
    <h1>Add a notebook</h1>
  </header>

  <nav class="add-nav" aria-label="Add a notebook — source">
    {#each SECTIONS as s}
      <button
        class:active={section === s.id}
        data-testid={`add-section-${s.id}`}
        onclick={() => (section = s.id)}
      >
        <span class="an-label">{s.label}</span>
        <span class="an-sub">{s.sub}</span>
      </button>
    {/each}
  </nav>

  <div class="add-body">
    {#if section === "files"}
      <LocalKnowledgeSection embedded {onOpenChatWithSeed} {onDropToChat} />
    {:else if section === "imports"}
      <ImportsTab />
    {:else}
      <KnowledgeStatus />
    {/if}
  </div>
</div>

<style>
  .add-sheet {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
    background: var(--bg-primary);
  }
  .add-header {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 12px 18px 10px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .back {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font: inherit;
    font-size: 0.82rem;
    font-weight: 500;
    color: var(--text-secondary);
    background: none;
    border: none;
    cursor: pointer;
    padding: 4px 6px;
    border-radius: var(--radius);
  }
  .back:hover { color: var(--text-primary); background: var(--bg-elevated); }
  .add-header h1 {
    font-size: 1.02rem;
    font-weight: 650;
    color: var(--text-primary);
    margin: 0;
  }

  .add-nav {
    display: flex;
    gap: 8px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-secondary);
    flex-shrink: 0;
  }
  .add-nav button {
    flex: 1;
    text-align: left;
    display: flex;
    flex-direction: column;
    gap: 2px;
    font: inherit;
    cursor: pointer;
    padding: 9px 13px;
    border-radius: 9px;
    border: 1px solid var(--border);
    background: var(--bg-elevated);
    color: var(--text-secondary);
  }
  .add-nav button:hover { color: var(--text-primary); }
  .add-nav button.active {
    border-color: color-mix(in oklch, var(--accent) 45%, var(--border));
    background: color-mix(in oklch, var(--accent) 8%, var(--bg-elevated));
    color: var(--text-primary);
  }
  .an-label { font-weight: 600; font-size: 0.9rem; }
  .an-sub { font-size: 0.76rem; color: var(--text-muted); }

  .add-body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
</style>
