<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Glassbox view of every ingest currently in flight — the destination the
  top-of-chat progress banner links to. For each active corpus it shows the
  phase, percent, live chunk throughput, and an ETA derived from the backend's
  real embed rate (not a fabricated guess). It also hosts the CONTEXTUAL
  peer-assist offer: this is where a user watching a slow local embed sees
  "≈X left on this machine" and can hand the job to chosen mesh peers, then
  watch the same glassbox progress with help applied.

  Fed purely by `corpusProgressStore.active`; names are resolved from the
  local-corpus registry (a mid-ingest vault/folder is already registered).
  Renders nothing when idle.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import {
    corpusProgressStore,
    etaSecondsFor,
    formatEta,
  } from "../../stores/corpusProgress.svelte";
  import { assistProgressStore } from "../../stores/assistProgress.svelte";
  import { lcList, meshAssistStart } from "../../api";
  import type { CorpusProgressPayload } from "../../types";
  import PeerAssistOffer from "../mesh/PeerAssistOffer.svelte";
  import AssistProgressPanel from "../mesh/AssistProgressPanel.svelte";

  const STANDING_ASSIST_TTL_SECS = 24 * 60 * 60; // backend caps at 24h

  // corpus_id → display name, from the local-corpus registry. A corpus mid
  // embed/enrich is already registered, so its real folder/vault name is
  // available here even though the progress payload only carries the id.
  let names = $state<Record<string, string>>({});
  let namesPollHandle: ReturnType<typeof setInterval> | null = null;

  async function refreshNames() {
    try {
      const configs = await lcList();
      const next: Record<string, string> = {};
      for (const c of configs) next[c.id] = c.display_name;
      names = next;
    } catch {
      // Manager not ready / no local corpora — fall back to id cleanup.
    }
  }

  onMount(() => {
    void corpusProgressStore.init();
    void refreshNames();
    namesPollHandle = setInterval(refreshNames, 5000);
    return () => {
      if (namesPollHandle) clearInterval(namesPollHandle);
    };
  });

  let active = $derived(corpusProgressStore.active);

  function label(id: string): string {
    if (names[id]) return names[id];
    if (id.startsWith("obsidian-vault-")) return "Obsidian vault";
    if (id.startsWith("watched-")) return "Watched folder";
    if (id.startsWith("document-folder-") || id.startsWith("folder-"))
      return "Folder";
    return id;
  }

  function phaseLabel(phase: string): string {
    if (phase.startsWith("enriching_")) return "Enriching";
    switch (phase) {
      case "downloading":
        return "Downloading";
      case "extracting":
        return "Reading documents";
      case "chunking":
        return "Chunking";
      case "embedding":
        return "Embedding";
      case "indexing":
        return "Building search index";
      case "optimizing_index":
        return "Optimizing search index";
      case "complete":
        return "Done";
      case "failed":
        return "Failed";
      default:
        return phase;
    }
  }

  function throughputLine(p: CorpusProgressPayload): string | null {
    const rate = p.chunks_per_sec ?? 0;
    const total = p.chunks_total ?? 0;
    if (rate <= 0 && total <= 0) return null;
    const parts: string[] = [];
    if (total > 0) {
      parts.push(
        `${p.chunks_processed.toLocaleString()} / ${total.toLocaleString()} chunks`,
      );
    } else if (p.chunks_processed > 0) {
      parts.push(`${p.chunks_processed.toLocaleString()} chunks`);
    }
    if (rate > 0) parts.push(`${Math.round(rate)}/s`);
    return parts.join(" · ");
  }

  // Per-corpus peer-assist decision (several ingests can run at once).
  let assistDecisions = $state<
    Record<string, { enabled: boolean; peerNodeIds: string[] }>
  >({});
  let assistStarting = $state<Set<string>>(new Set());
  let assistErrors = $state<Record<string, string>>({});

  async function startAssist(id: string) {
    const decision = assistDecisions[id];
    if (!decision?.enabled || decision.peerNodeIds.length === 0) return;
    assistStarting = new Set([...assistStarting, id]);
    assistErrors = { ...assistErrors, [id]: "" };
    try {
      const handle = await meshAssistStart(
        id,
        decision.peerNodeIds,
        STANDING_ASSIST_TTL_SECS,
      );
      assistProgressStore.track({
        corpus_id: handle.corpus_id,
        handoff_id: handle.handoff_id,
        grant_expires_at_ms: handle.grant_expires_at_ms,
      });
    } catch (e) {
      assistErrors = { ...assistErrors, [id]: String(e) };
    }
    assistStarting = new Set([...assistStarting].filter((x) => x !== id));
  }
</script>

{#if active.length > 0}
  <section class="in-progress" data-testid="in-progress-ingests">
    <div class="ip-head">
      <span class="ip-label">In progress</span>
      <span class="ip-count">{active.length}</span>
    </div>

    {#each active as item (item.corpus_id)}
      {@const eta = formatEta(etaSecondsFor(item))}
      {@const tp = throughputLine(item)}
      {@const job = assistProgressStore.get(item.corpus_id)}
      <div class="ingest" data-corpus-id={item.corpus_id}>
        <div class="ingest-head">
          <span class="name">{label(item.corpus_id)}</span>
          <span class="phase">{phaseLabel(item.phase)}</span>
          {#if item.percent > 0}
            <span class="pct">{item.percent.toFixed(0)}%</span>
          {/if}
          {#if eta !== "—"}
            <span class="eta" title="Estimated time remaining (approximate)">
              {eta} left
            </span>
          {/if}
        </div>

        <div class="bar">
          <div class="fill" style:width={`${Math.max(item.percent, 2)}%`}></div>
        </div>

        {#if tp}
          <p class="throughput">{tp}</p>
        {:else if item.message}
          <p class="throughput">{item.message}</p>
        {/if}

        <!-- Contextual peer-assist: the natural home for "this is slow —
             hand it to the mesh". Self-hides unless the corpus is grantable
             and a compatible peer is online. -->
        {#if job}
          <AssistProgressPanel
            {job}
            onRevoke={(c) => assistProgressStore.revoke(c)}
          />
        {:else}
          <PeerAssistOffer
            corpusId={item.corpus_id}
            surface={item.corpus_id.startsWith("obsidian-vault-")
              ? "vault"
              : "folder"}
            explainWhenUnavailable={true}
            onChange={(d) =>
              (assistDecisions = { ...assistDecisions, [item.corpus_id]: d })}
          />
          {#if assistDecisions[item.corpus_id]?.enabled && assistDecisions[item.corpus_id].peerNodeIds.length > 0}
            <button
              class="assist-start"
              onclick={() => startAssist(item.corpus_id)}
              disabled={assistStarting.has(item.corpus_id)}
            >
              {assistStarting.has(item.corpus_id)
                ? "Starting…"
                : "Get mesh help"}
            </button>
          {/if}
          {#if assistErrors[item.corpus_id]}
            <p class="assist-error">{assistErrors[item.corpus_id]}</p>
          {/if}
        {/if}
      </div>
    {/each}
  </section>
{/if}

<style>
  .in-progress {
    margin-bottom: 20px;
    padding: 14px 16px;
    background: var(--bg-secondary, rgba(0, 0, 0, 0.02));
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .ip-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin-bottom: 10px;
  }
  .ip-label {
    font-size: 0.72rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-muted);
  }
  .ip-count {
    font-size: 0.72rem;
    color: var(--text-muted);
    font-family: var(--font-mono, monospace);
  }
  .ingest {
    padding: 10px 0;
    border-top: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .ingest:first-of-type {
    border-top: none;
  }
  .ingest-head {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .name {
    font-size: 0.9rem;
    font-weight: 500;
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .phase {
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  .pct {
    font-size: 0.78rem;
    color: var(--text-secondary);
    font-weight: 500;
    margin-left: auto;
  }
  .eta {
    font-size: 0.78rem;
    color: var(--text-muted);
    white-space: nowrap;
  }
  .bar {
    height: 5px;
    background: var(--border);
    border-radius: 3px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    border-radius: 3px;
    transition: width 0.3s ease;
  }
  .throughput {
    margin: 0;
    font-size: 0.76rem;
    color: var(--text-muted);
    font-family: var(--font-mono, monospace);
  }
  .assist-start {
    align-self: flex-start;
    margin-top: 2px;
    padding: 4px 12px;
    background: var(--accent);
    color: var(--text-on-accent, #fff);
    border: none;
    border-radius: var(--radius);
    font-size: 0.8rem;
    font-weight: 500;
    cursor: pointer;
  }
  .assist-start:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
  .assist-error {
    margin: 4px 0 0;
    font-size: 0.76rem;
    color: var(--error, #ef4444);
  }
</style>
