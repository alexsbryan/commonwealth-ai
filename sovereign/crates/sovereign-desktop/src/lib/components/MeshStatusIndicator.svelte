<script lang="ts">
  import { onMount } from "svelte";
  import { meshGetState, meshIsRunning } from "../api";
  import type { MeshStateResponse } from "../types";

  const POLL_INTERVAL_MS = 10_000;

  // The colon annotation does NOT propagate through $state() in Svelte 5
  // runes — TypeScript narrows to the literal type of the initializer
  // (`null`), and every subsequent assignment treats `mesh` as `never`.
  // Passing the type as the generic parameter keeps the union intact.
  let mesh = $state<MeshStateResponse | null>(null);

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

  let label = $derived(
    mesh
      ? mesh.status.members_online === 1
        ? "1 connected"
        : `${mesh.status.members_online} connected`
      : null
  );
</script>

{#if label}
  <div class="mesh-status" aria-live="polite">
    <span class="dot" aria-hidden="true"></span>
    <span class="count">{label}</span>
  </div>
{/if}

<style>
  .mesh-status {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 10px 14px;
  }

  .dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--growth);
    flex-shrink: 0;
    animation: node-pulse 2.4s ease-in-out infinite;
  }

  @keyframes node-pulse {
    0%, 100% { opacity: 1; box-shadow: 0 0 3px var(--growth); }
    50%       { opacity: 0.7; box-shadow: 0 0 8px var(--growth); }
  }

  .count {
    font-family: var(--font-mono);
    font-size: 0.65rem;
    letter-spacing: 0.04em;
    color: var(--text-muted);
  }
</style>
