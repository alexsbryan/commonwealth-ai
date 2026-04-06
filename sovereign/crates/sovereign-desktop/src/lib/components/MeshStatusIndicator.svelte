<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { meshGetState, meshIsRunning } from "../api";
  import type { MeshStateResponse } from "../types";

  interface Props {
    /** Called when the user clicks the indicator. Opens settings to the mesh section. */
    onOpen: () => void;
  }

  let { onOpen }: Props = $props();

  let running = $state(false);
  let state = $state<MeshStateResponse | null>(null);
  let pollHandle: ReturnType<typeof setInterval> | null = null;

  onMount(async () => {
    await refresh();
    // Poll every 10 seconds. Cheap call — daemon is in-process.
    pollHandle = setInterval(refresh, 10000);
  });

  onDestroy(() => {
    if (pollHandle) clearInterval(pollHandle);
  });

  async function refresh() {
    try {
      running = await meshIsRunning();
      state = running ? await meshGetState() : null;
    } catch {
      running = false;
      state = null;
    }
  }
</script>

{#if running && state}
  <button
    class="indicator"
    onclick={onOpen}
    title="Open mesh settings"
  >
    <span class="dot online"></span>
    <span class="name">{state.status.name}</span>
    <span class="count">
      {state.status.members_online}/{state.status.members_total}
    </span>
  </button>
{/if}

<style>
  .indicator {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-secondary);
    font-size: 0.78rem;
    text-align: left;
    width: 100%;
    transition: background 0.2s;
  }

  .indicator:hover {
    background: var(--bg-input);
    color: var(--text-primary);
  }

  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-muted);
    flex-shrink: 0;
  }

  .dot.online {
    background: var(--success);
  }

  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .count {
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
</style>
