<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { corpusProgressStore } from "../stores/corpusProgress.svelte";

  interface Props {
    onOpenSettings?: () => void;
  }

  let { onOpenSettings }: Props = $props();

  // Single shared listener lives in the store; this component just
  // reads from it. Multiple instances of the banner (or KnowledgeStatus
  // mounted in parallel) subscribe to the same underlying source.
  onMount(async () => {
    await corpusProgressStore.init();
  });

  // Friendly display names for known corpus IDs.
  const NAMES: Record<string, string> = {
    wikipedia: "Wikipedia",
    "wikipedia-simple": "Simple English Wikipedia",
    sep: "Stanford Encyclopedia of Philosophy",
    openalex: "OpenAlex",
    stackexchange: "Stack Exchange",
    "stackexchange-knowledge": "Stack Exchange Knowledge",
    gutenberg: "Project Gutenberg",
    crs_reports: "CRS Reports",
  };

  // Layer 0 of the layered Wikipedia stack — installs alongside Core
  // but doesn't have its own row in the corpus picker. Hide its
  // progress events from the banner too so the user sees one
  // "Wikipedia" entry whose phase reflects whichever layer is
  // currently active. While Simple is downloading, the Wikipedia row
  // shows Core's "downloading" since the layered install pipelines
  // both starts; once Simple finishes its progress entry drops out
  // and only Core remains.
  const HIDDEN_FROM_BANNER = new Set(["wikipedia-simple"]);

  let visible = $derived(
    corpusProgressStore.active.filter((p) => !HIDDEN_FROM_BANNER.has(p.corpus_id)),
  );

  function phaseLabel(phase: string): string {
    switch (phase) {
      case "downloading":
        return "downloading";
      case "parsing":
        return "indexing";
      // Emitted post-expansion while the IVF-PQ vector index is
      // retrained over the new (larger) chunk set. Search remains
      // live; this is the user-visible signal that "more is
      // happening" between the last embed batch and the corpus
      // flipping back to ✓ Indexed.
      case "optimizing_index":
        return "optimizing search index";
      default:
        return phase;
    }
  }

  function displayName(id: string): string {
    return NAMES[id] ?? id;
  }
</script>

{#if visible.length > 0}
  <!-- role="status" (a polite live region): phase transitions
       ("downloading" → "indexing" → "optimizing") are announced to
       screen readers as the text updates, without stealing focus. -->
  <div class="corpus-banner" role="status" aria-live="polite">
    {#each visible as item (item.corpus_id)}
      <button
        class="banner-row"
        onclick={() => onOpenSettings?.()}
        title="Click to view in settings"
      >
        <span class="icon" aria-hidden="true">📚</span>
        <span class="name">{displayName(item.corpus_id)}</span>
        <span class="phase">{phaseLabel(item.phase)}</span>
        {#if item.percent > 0}
          <span class="pct">{item.percent.toFixed(0)}%</span>
        {/if}
        <div class="progress-bar">
          <div
            class="progress-fill"
            style="width: {Math.max(item.percent, 2)}%"
          ></div>
        </div>
      </button>
    {/each}
  </div>
{/if}

<style>
  .corpus-banner {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 12px;
    background: var(--bg-surface);
    border-bottom: 1px solid var(--border);
  }

  .banner-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 10px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    width: 100%;
    text-align: left;
    cursor: pointer;
    transition: border-color 0.15s;
  }

  .banner-row:hover {
    border-color: var(--accent);
  }

  .icon {
    font-size: 1rem;
    flex-shrink: 0;
  }

  .name {
    font-size: 0.85rem;
    font-weight: 500;
    color: var(--text-primary);
    flex-shrink: 0;
  }

  .phase {
    font-size: 0.75rem;
    color: var(--text-muted);
    text-transform: lowercase;
  }

  .pct {
    font-size: 0.75rem;
    color: var(--text-secondary);
    font-weight: 500;
    margin-left: auto;
  }

  .progress-bar {
    flex: 1;
    height: 4px;
    background: var(--border);
    border-radius: 2px;
    overflow: hidden;
    min-width: 80px;
    max-width: 200px;
  }

  .progress-fill {
    height: 100%;
    background: var(--accent);
    border-radius: 2px;
    transition: width 0.3s;
  }
</style>
