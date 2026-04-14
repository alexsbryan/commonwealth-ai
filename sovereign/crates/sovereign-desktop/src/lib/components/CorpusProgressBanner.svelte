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
    sep: "Stanford Encyclopedia of Philosophy",
    openalex: "OpenAlex",
    stackexchange: "Stack Exchange",
    gutenberg: "Project Gutenberg",
    crs_reports: "CRS Reports",
  };

  let visible = $derived(corpusProgressStore.active);

  function phaseLabel(phase: string): string {
    switch (phase) {
      case "downloading":
        return "downloading";
      case "parsing":
        return "indexing";
      default:
        return phase;
    }
  }

  function displayName(id: string): string {
    return NAMES[id] ?? id;
  }
</script>

{#if visible.length > 0}
  <div class="corpus-banner">
    {#each visible as item (item.corpus_id)}
      <button
        class="banner-row"
        onclick={() => onOpenSettings?.()}
        title="Click to view in settings"
      >
        <span class="icon">📚</span>
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
