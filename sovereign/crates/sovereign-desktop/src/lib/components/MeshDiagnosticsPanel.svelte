<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { meshDiagnostics, meshPlacement } from "../api";
  import type { MeshDiagnostics } from "../types";

  // Polls the mesh_diagnostics Tauri command every 3 seconds and
  // renders the current mDNS-discovered peer list. The panel is the
  // user-visible evidence that cross-machine LAN discovery works —
  // if a peer is expected and doesn't appear here, the problem is
  // upstream (firewall, different network, mDNS blocked) and no
  // amount of UI retrying will help.

  let data: MeshDiagnostics | null = $state(null);
  let error = $state("");
  let pollHandle: ReturnType<typeof setInterval> | null = null;

  // Shared-model placement (P0.6): the user-visible answer to "is my
  // big model actually distributed right now, and across whom?" —
  // silent local-only fallback otherwise reads as a mysteriously slow
  // model. Opaque JSON from the daemon's /status; render what's there.
  let placement: Record<string, unknown> | null = $state(null);

  function placementMode(): string | null {
    const p = placement?.placement as Record<string, unknown> | undefined;
    return (p?.mode as string) ?? null;
  }
  function placementWorkers(): { endpoint: string; blocks: number }[] {
    const p = placement?.placement as Record<string, unknown> | undefined;
    return (p?.workers as { endpoint: string; blocks: number }[]) ?? [];
  }
  function placementBlocks(): { local: number; total: number } {
    const p = placement?.placement as Record<string, unknown> | undefined;
    return {
      local: (p?.local_blocks as number) ?? 0,
      total: (p?.total_blocks as number) ?? 0,
    };
  }

  async function tick() {
    try {
      data = await meshDiagnostics();
      error = "";
    } catch (e) {
      error = String(e);
    }
    try {
      placement = await meshPlacement();
    } catch {
      placement = null; // never let placement break the diagnostics panel
    }
  }

  onMount(async () => {
    await tick();
    pollHandle = setInterval(tick, 3000);
  });

  onDestroy(() => {
    if (pollHandle) clearInterval(pollHandle);
  });
</script>

<div class="diag">
  <div class="diag-header">
    <span class="diag-label">Network diagnostics</span>
    {#if data?.daemon_running}
      <span class="diag-status online">● daemon running</span>
    {:else}
      <span class="diag-status offline">● daemon stopped</span>
    {/if}
  </div>

  {#if error}
    <div class="diag-error">{error}</div>
  {/if}

  {#if placement?.placement}
    <div class="placement">
      <span class="placement-model mono">{placement.model_id}</span>
      {#if placementMode() === "distributed"}
        <span class="placement-chip distributed" title="blocks split across machines">
          ⇄ distributed · {placementBlocks().local}/{placementBlocks().total} blocks local
        </span>
        {#each placementWorkers() as w (w.endpoint)}
          <span class="placement-worker mono" title="worker endpoint">
            {w.endpoint} · {w.blocks} blocks
          </span>
        {/each}
      {:else if placement.transitioning}
        <span class="placement-chip transitioning">… redistributing</span>
      {:else}
        <span
          class="placement-chip local"
          title="Model is running on this machine only. If workers are expected, check that their daemons are up — the mesh recovers automatically once they return."
        >
          ● local only
        </span>
      {/if}
    </div>
  {/if}

  <div class="peers-header">
    Peers discovered via mDNS
    {#if data}
      <span class="count">({data.discovered_peers.length})</span>
    {/if}
  </div>

  {#if !data || data.discovered_peers.length === 0}
    <p class="empty">
      {#if data?.daemon_running}
        No peers yet. Make sure the other machine is on the same
        network with its mesh daemon running, then wait ~5 seconds.
      {:else}
        Start or join a mesh to begin discovering peers.
      {/if}
    </p>
  {:else}
    <table class="peers">
      <thead>
        <tr>
          <th>Node</th>
          <th>Address</th>
          <th>Mesh</th>
        </tr>
      </thead>
      <tbody>
        {#each data.discovered_peers as peer (peer.address)}
          <tr>
            <td>{peer.name || "(unknown)"}</td>
            <td class="mono">{peer.address}</td>
            <td
              title={`mesh_id: ${peer.mesh_id_hex}`}
            >
              {peer.mesh_name || `mesh ${peer.mesh_id_hex.slice(0, 8)}…`}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  .diag {
    margin-top: 20px;
    padding: 14px 16px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .diag-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .diag-label {
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--text-muted);
  }

  .placement {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    font-size: 0.78rem;
  }

  .placement-model {
    color: var(--text-muted);
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .placement-chip {
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 0.72rem;
    font-weight: 600;
  }

  .placement-chip.distributed {
    background: color-mix(in srgb, var(--accent, #4a9eff) 18%, transparent);
    color: var(--accent, #4a9eff);
  }

  .placement-chip.local {
    background: color-mix(in srgb, currentColor 10%, transparent);
    color: var(--text-muted);
  }

  .placement-chip.transitioning {
    color: var(--text-muted);
    font-style: italic;
  }

  .placement-worker {
    font-size: 0.72rem;
    color: var(--text-muted);
  }

  .diag-status {
    font-size: 0.75rem;
  }
  .diag-status.online {
    color: var(--success);
  }
  .diag-status.offline {
    color: var(--text-muted);
  }

  .diag-error {
    padding: 6px 10px;
    background: color-mix(in srgb, var(--error) 10%, transparent);
    color: var(--error);
    border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
    border-radius: var(--radius);
    font-size: 0.78rem;
  }

  .peers-header {
    font-size: 0.82rem;
    color: var(--text-secondary);
  }
  .count {
    color: var(--text-muted);
    margin-left: 4px;
  }

  .empty {
    font-size: 0.8rem;
    color: var(--text-muted);
    line-height: 1.5;
    margin: 0;
  }

  table.peers {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.82rem;
  }
  table.peers th {
    text-align: left;
    font-weight: 600;
    padding: 6px 8px;
    border-bottom: 1px solid var(--border);
    color: var(--text-muted);
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  table.peers td {
    padding: 6px 8px;
    border-bottom: 1px solid var(--border);
  }
  table.peers tr:last-child td {
    border-bottom: none;
  }

  .mono {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.78rem;
  }
</style>
