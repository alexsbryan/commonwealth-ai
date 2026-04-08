<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { listCorpora, installCorpus, removeCorpus, getCorpusHealth, retryEnrichmentFailures } from "../api";
  import type { CorpusEntry, CorpusProgressPayload, CorpusHealthDetail } from "../types";

  let corpora: CorpusEntry[] = $state([]);
  let progress: Record<string, CorpusProgressPayload> = $state({});
  let expanded: Set<string> = $state(new Set());
  let health: Record<string, CorpusHealthDetail> = $state({});
  let repairing: Set<string> = $state(new Set());
  let unlisten: UnlistenFn | null = null;

  const tiers: { id: string; name: string; desc: string }[] = [
    { id: "essential", name: "Essential", desc: "Wikipedia" },
    { id: "research", name: "Research", desc: "Wikipedia + scholarly sources" },
    { id: "technical", name: "Technical", desc: "Wikipedia + Stack Exchange" },
    { id: "full", name: "Full", desc: "All knowledge bases" },
  ];

  // The backend (`list_corpora` Tauri command) returns the full catalog
  // from `corpus_engine::builtin_corpora()` — there's no longer a fallback
  // path because the catalog ships in Rust source, not a sidecar TOML.

  let installedCount = $derived(
    corpora.filter((c) => c.status === "installed").length,
  );
  let anyInstalling = $derived(
    corpora.some(
      (c) =>
        c.status === "installing" ||
        (progress[c.id] &&
          progress[c.id].phase !== "complete" &&
          progress[c.id].phase !== "failed"),
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
      corpora = [];
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

  async function toggleHealth(id: string) {
    if (expanded.has(id)) {
      expanded.delete(id);
      expanded = new Set(expanded);
    } else {
      expanded.add(id);
      expanded = new Set(expanded);
      if (!health[id]) {
        try {
          const detail = await getCorpusHealth(id);
          if (detail) health = { ...health, [id]: detail };
        } catch (e) {
          console.error("Failed to load corpus health:", e);
        }
      }
    }
  }

  async function handleRepair(id: string) {
    repairing = new Set([...repairing, id]);
    try {
      await retryEnrichmentFailures(id);
      // Refresh health so the failure count and claims count update.
      const detail = await getCorpusHealth(id);
      if (detail) health = { ...health, [id]: detail };
    } catch (e) {
      console.error("Repair failed:", e);
    } finally {
      repairing = new Set([...repairing].filter((x) => x !== id));
    }
  }

  function formatDate(unixSecs: number): string {
    return new Date(unixSecs * 1000).toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }

  function phaseLabel(phase: string): string {
    switch (phase) {
      case "downloading":
        return "Downloading…";
      case "extracting":
        return "Extracting documents…";
      case "chunking":
        return "Chunking…";
      case "embedding":
        return "Embedding…";
      case "indexing":
        return "Building index…";
      case "extracting_claims":
        return "Extracting claims…";
      case "finding_relationships":
        return "Finding relationships…";
      case "extracting_relationships":
        return "Extracting relationships…";
      case "building_link_graph":
        return "Building link graph…";
      case "computing_profiles":
        return "Computing article profiles…";
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
    {@const inProgress =
      corpus.status === "installing" ||
      (progress[corpus.id] &&
        progress[corpus.id].phase !== "complete" &&
        progress[corpus.id].phase !== "failed")}
    <div class="corpus-row">
      <div class="corpus-info">
        <div class="corpus-name">
          {#if corpus.status === "installed"}
            <span class="dot installed"></span>
          {:else if inProgress}
            <span class="dot installing"></span>
          {:else}
            <span class="dot"></span>
          {/if}
          {corpus.name}
          {#if corpus.enrichment_enabled}
            <span class="enrichment-pill" title="Includes claim and relationship enrichment for the epistemic-research skill">✦ enriched</span>
          {/if}
        </div>
        <div class="corpus-detail">
          {#if corpus.status === "installed"}
            <button
              class="detail-toggle"
              onclick={() => toggleHealth(corpus.id)}
              title="Show index details"
            >
              {expanded.has(corpus.id) ? "▾" : "▸"}
            </button>
            Indexed
            {#if corpus.indexed_at}
              &middot; {formatDate(corpus.indexed_at)}
            {/if}
            {#if corpus.chunks_count}
              &middot; {corpus.chunks_count.toLocaleString()} chunks
            {/if}
            {#if expanded.has(corpus.id)}
              <div class="health-panel">
                {#if corpus.embedding_model}
                  <span class="health-chip">{corpus.embedding_model}{corpus.embedding_dimensions ? ` (${corpus.embedding_dimensions}-dim)` : ""}</span>
                {/if}
                {#if health[corpus.id]}
                  {#if health[corpus.id].claims_count > 0}
                    <span class="health-chip enriched">✦ {health[corpus.id].claims_count.toLocaleString()} claims · {health[corpus.id].relationships_count.toLocaleString()} relationships</span>
                  {:else if corpus.enrichment_enabled}
                    <span class="health-chip">✦ enriched (no claims yet)</span>
                  {:else}
                    <span class="health-chip muted">No enrichment</span>
                  {/if}
                  {#if health[corpus.id].parse_failure_count > 0}
                    <button
                      class="health-chip repair-btn"
                      disabled={repairing.has(corpus.id)}
                      onclick={() => handleRepair(corpus.id)}
                      title="{health[corpus.id].parse_failure_count.toLocaleString()} chunks failed to parse during enrichment — click to retry with repair parser"
                    >
                      {repairing.has(corpus.id)
                        ? "Repairing…"
                        : `⚠ Repair ${health[corpus.id].parse_failure_count.toLocaleString()} claims`}
                    </button>
                  {/if}
                  {#if health[corpus.id].has_article_profiles}
                    <span class="health-chip">Article profiles</span>
                  {/if}
                {:else}
                  <span class="health-chip muted">Loading…</span>
                {/if}
              </div>
            {/if}
          {:else if inProgress}
            {#if progress[corpus.id]}
              {phaseLabel(progress[corpus.id].phase)}
              {#if progress[corpus.id].percent > 0}
                · {progress[corpus.id].percent.toFixed(0)}%
              {/if}
              {#if progress[corpus.id].message}
                · {progress[corpus.id].message}
              {/if}
            {:else}
              Starting…
            {/if}
          {:else}
            ~{corpus.size_compressed_gb} GB download · ~{corpus.size_indexed_gb} GB indexed
            <div class="corpus-blurb">{corpus.description}</div>
          {/if}
        </div>
      </div>

      <div class="corpus-action">
        {#if corpus.status === "installed"}
          <button class="action-btn remove" onclick={() => handleRemove(corpus.id)}>
            Remove
          </button>
        {:else if inProgress}
          {#if progress[corpus.id]?.percent > 0}
            <div class="progress-bar">
              <div
                class="progress-fill"
                style="width: {progress[corpus.id].percent}%"
              ></div>
            </div>
          {:else}
            <span class="status-text">Working…</span>
          {/if}
        {:else}
          <button class="action-btn install" onclick={() => handleInstall(corpus.id)}>
            Install
          </button>
        {/if}
      </div>
    </div>
  {/each}

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
    color: var(--text-on-accent);
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
    color: var(--text-on-accent);
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
  .detail-toggle {
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    color: var(--text-muted);
    font-size: 0.7rem;
    line-height: 1;
    margin-right: 2px;
  }
  .detail-toggle:hover {
    color: var(--text-secondary);
  }
  .health-panel {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 4px;
    margin-left: 0;
  }
  .health-chip {
    display: inline-block;
    font-size: 0.7rem;
    color: var(--text-secondary);
    background: var(--bg-surface);
    border: 1px solid var(--border);
    padding: 1px 7px;
    border-radius: 10px;
    white-space: nowrap;
  }
  .health-chip.enriched {
    color: var(--accent-light);
    background: var(--accent-dim);
    border-color: rgba(201, 168, 76, 0.3);
  }
  .health-chip.muted {
    color: var(--text-muted);
  }
  .health-chip.repair-btn {
    cursor: pointer;
    color: var(--warning, #e6a817);
    background: transparent;
    border-color: var(--warning, #e6a817);
    font-family: inherit;
    transition: opacity 0.15s;
  }
  .health-chip.repair-btn:hover:not(:disabled) {
    opacity: 0.8;
  }
  .health-chip.repair-btn:disabled {
    cursor: default;
    opacity: 0.6;
  }
  .enrichment-pill {
    font-size: 0.65rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--accent-light);
    background: var(--accent-dim);
    border: 1px solid rgba(201, 168, 76, 0.3);
    padding: 1px 6px;
    border-radius: 10px;
    margin-left: 6px;
    white-space: nowrap;
  }
  .corpus-blurb {
    font-size: 0.75rem;
    color: var(--text-muted);
    margin-top: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
