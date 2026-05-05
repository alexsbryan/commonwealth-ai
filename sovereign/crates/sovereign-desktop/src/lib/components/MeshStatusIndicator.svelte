<script lang="ts">
  import { onMount } from "svelte";
  import { meshGetState, meshIsRunning } from "../api";
  import type { MeshStateResponse } from "../types";

  const POLL_INTERVAL_MS = 10_000;

  interface Props {
    /** Called when the user clicks the indicator. Opens settings to the mesh section. */
    onOpen: () => void;
  }

  let { onOpen }: Props = $props();

  let mesh: MeshStateResponse | null = $state(null);

  onMount(() => {
    void refresh();
    const handle = setInterval(refresh, POLL_INTERVAL_MS);
    return () => clearInterval(handle);
  });

  async function refresh() {
    try {
      mesh = (await meshIsRunning()) ? await meshGetState() : null;
    } catch {
      mesh = null;
    }
  }
</script>

{#if mesh}
  <button class="indicator" onclick={onOpen} title="Open mesh settings">
    <span class="dot online" aria-hidden="true"></span>
    <span class="net-info">
      <span class="net-name">{mesh.status.name}</span>
      <span class="net-count">{mesh.status.members_online} / {mesh.status.members_total} nodes</span>
    </span>
    <svg class="arrow" width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden="true">
      <path d="M3.5 2l3 3-3 3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
    </svg>
  </button>
{/if}

<style>
  .indicator {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 12px;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    width: 100%;
    text-align: left;
    transition: border-color 0.2s, box-shadow 0.2s, background 0.2s;
  }

  .indicator:hover {
    background: var(--bg-elevated);
    border-color: var(--growth);
    box-shadow: 0 0 10px var(--growth-glow);
  }

  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-muted);
    flex-shrink: 0;
  }

  .dot.online {
    background: var(--growth);
    box-shadow: 0 0 5px var(--growth);
    animation: node-pulse 2.4s ease-in-out infinite;
  }

  @keyframes node-pulse {
    0%, 100% {
      box-shadow: 0 0 4px var(--growth);
      opacity: 1;
    }
    50% {
      box-shadow: 0 0 12px var(--growth), 0 0 20px var(--growth-glow);
      opacity: 0.8;
    }
  }

  .net-info {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .net-name {
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .net-count {
    font-size: 0.67rem;
    color: var(--growth);
    font-family: var(--font-mono);
    letter-spacing: 0.03em;
  }

  .arrow {
    color: var(--text-muted);
    flex-shrink: 0;
    transition: color 0.2s;
  }

  .indicator:hover .arrow {
    color: var(--growth);
  }
</style>
