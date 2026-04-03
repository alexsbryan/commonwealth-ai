<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { listCorpora, installCorpus, removeCorpus } from "../api";
  import type { CorpusEntry, CorpusProgressPayload } from "../types";

  let corpora: CorpusEntry[] = $state([]);
  let progress: Record<string, CorpusProgressPayload> = $state({});
  let unlisten: UnlistenFn | null = null;

  const tiers: { id: string; name: string; desc: string }[] = [
    { id: "essential", name: "Essential", desc: "Wikipedia" },
    { id: "research", name: "Research", desc: "Wikipedia + scholarly sources" },
    { id: "technical", name: "Technical", desc: "Wikipedia + Stack Exchange" },
    { id: "full", name: "Full", desc: "All knowledge bases" },
  ];

  let installedCount = $derived(
    corpora.filter((c) => c.status === "installed").length,
  );
  let anyInstalling = $derived(
    corpora.some(
      (c) => c.status === "installing" || progress[c.id]?.phase === "downloading" || progress[c.id]?.phase === "parsing",
    ),
  );

  onMount(async () => {
    await refresh();
    unlisten = await listen<CorpusProgressPayload>(
      "corpus-progress",
      (event) => {
        const p = event.payload;
        progress = { ...progress, [p.corpus_id]: p };
        if (p.phase === "complete" || p.phase === "failed") {
          refresh();
        }
      },
    );
  });

  onDestroy(() => {
    if (unlisten) unlisten();
  });

  async function refresh() {
    try {
      corpora = await listCorpora();
    } catch (e) {
      console.error("Failed to list corpora:", e);
    }
  }

  async function handleInstall(id: string) {
    try {
      await installCorpus(id);
      corpora = corpora.map((c) =>
        c.id === id ? { ...c, status: "installing" as const } : c,
      );
    } catch (e) {
      console.error("Install failed:", e);
    }
  }

  async function handleRemove(id: string) {
    try {
      await removeCorpus(id);
      await refresh();
    } catch (e) {
      console.error("Remove failed:", e);
    }
  }

  async function installTier(tierId: string) {
    const tierCorpora = corpora.filter(
      (c) => c.tiers.includes(tierId) && c.status === "not_installed",
    );
    for (const c of tierCorpora) {
      await handleInstall(c.id);
    }
  }

  function phaseLabel(phase: string): string {
    switch (phase) {
      case "downloading":
        return "Downloading...";
      case "parsing":
        return "Indexing...";
      case "complete":
        return "Complete";
      case "failed":
        return "Failed";
      default:
        return phase;
    }
  }
</script>

<div class="knowledge-status">
  {#if corpora.length > 0 && installedCount === 0 && !anyInstalling}
    <div class="tier-banner">
      <p class="tier-prompt">No knowledge bases installed. Quick-install a tier:</p>
      <div class="tier-buttons">
        {#each tiers as tier}
          <button class="tier-btn" onclick={() => installTier(tier.id)}>
            {tier.name}
          </button>
        {/each}
      </div>
    </div>
  {/if}

  {#each corpora as corpus}
    <div class="corpus-row">
      <div class="corpus-info">
        <div class="corpus-name">
          {#if corpus.status === "installed"}
            <span class="dot installed"></span>
          {:else if corpus.status === "installing" || progress[corpus.id]?.phase === "downloading" || progress[corpus.id]?.phase === "parsing"}
            <span class="dot installing"></span>
          {:else}
            <span class="dot"></span>
          {/if}
          {corpus.name}
        </div>
        <div class="corpus-detail">
          {#if corpus.status === "installed"}
            Indexed
            {#if corpus.chunks_count}
              &middot; {corpus.chunks_count.toLocaleString()} chunks
            {/if}
          {:else if corpus.status === "installing" || progress[corpus.id]}
            {#if progress[corpus.id]}
              {phaseLabel(progress[corpus.id].phase)}
              {#if progress[corpus.id].percent > 0}
                {progress[corpus.id].percent.toFixed(0)}%
              {/if}
            {:else}
              Starting...
            {/if}
          {:else}
            {corpus.size_indexed_gb} GB &middot; {corpus.description}
          {/if}
        </div>
      </div>

      <div class="corpus-action">
        {#if corpus.status === "installed"}
          <button class="action-btn remove" onclick={() => handleRemove(corpus.id)}>
            Remove
          </button>
        {:else if corpus.status === "installing" || progress[corpus.id]}
          {#if progress[corpus.id]?.percent > 0}
            <div class="progress-bar">
              <div
                class="progress-fill"
                style="width: {progress[corpus.id].percent}%"
              ></div>
            </div>
          {:else}
            <span class="status-text">Working...</span>
          {/if}
        {:else}
          <button class="action-btn install" onclick={() => handleInstall(corpus.id)}>
            Install
          </button>
        {/if}
      </div>
    </div>
  {/each}

  {#if corpora.length === 0}
    <p class="empty">No knowledge bases available. Check that corpora.toml is in your data directory.</p>
  {/if}
</div>

<style>
  .knowledge-status {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .tier-banner {
    padding: 12px 16px;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    margin-bottom: 8px;
  }
  .tier-prompt {
    font-size: 0.85rem;
    color: var(--text-secondary);
    margin-bottom: 8px;
  }
  .tier-buttons {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .tier-btn {
    padding: 4px 14px;
    font-size: 0.8rem;
    font-weight: 500;
    background: var(--accent);
    color: white;
    border-radius: var(--radius);
  }
  .tier-btn:hover {
    background: var(--accent-hover);
  }
  .corpus-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 0;
    border-bottom: 1px solid var(--border);
  }
  .corpus-row:last-child {
    border-bottom: none;
  }
  .corpus-info {
    flex: 1;
    min-width: 0;
  }
  .corpus-name {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.9rem;
    font-weight: 500;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-muted);
    flex-shrink: 0;
  }
  .dot.installed {
    background: var(--success, #22c55e);
  }
  .dot.installing {
    background: var(--accent);
    animation: pulse 1.5s infinite;
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.4;
    }
  }
  .corpus-detail {
    font-size: 0.8rem;
    color: var(--text-muted);
    margin-left: 16px;
    margin-top: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .corpus-action {
    flex-shrink: 0;
    min-width: 80px;
    text-align: right;
  }
  .action-btn {
    padding: 4px 12px;
    border-radius: var(--radius);
    font-size: 0.8rem;
    font-weight: 500;
  }
  .action-btn.install {
    background: var(--accent);
    color: white;
  }
  .action-btn.install:hover {
    background: var(--accent-hover);
  }
  .action-btn.remove {
    background: var(--bg-surface);
    color: var(--text-secondary);
    border: 1px solid var(--border);
  }
  .action-btn.remove:hover {
    border-color: var(--error, #ef4444);
    color: var(--error, #ef4444);
  }
  .progress-bar {
    width: 80px;
    height: 6px;
    background: var(--bg-surface);
    border-radius: 3px;
    overflow: hidden;
  }
  .progress-fill {
    height: 100%;
    background: var(--accent);
    border-radius: 3px;
    transition: width 0.3s;
  }
  .status-text {
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .empty {
    font-size: 0.85rem;
    color: var(--text-muted);
    text-align: center;
    padding: 12px 0;
  }
</style>
