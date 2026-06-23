<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { meshGetState, meshIsRunning, getRuntimeStatus } from "../api";
  import type { RuntimeStatus } from "../api";
  import type { MeshStateResponse } from "../types";

  const POLL_INTERVAL_MS = 10_000;

  // The colon annotation does NOT propagate through $state() in Svelte 5
  // runes — TypeScript narrows to the literal type of the initializer
  // (`null`), and every subsequent assignment treats `mesh` as `never`.
  // Passing the type as the generic parameter keeps the union intact.
  let mesh = $state<MeshStateResponse | null>(null);
  let runtime = $state<RuntimeStatus | null>(null);

  onMount(() => {
    void refresh();
    const handle = setInterval(refresh, POLL_INTERVAL_MS);
    return () => clearInterval(handle);
  });

  async function refresh() {
    try {
      if (await meshIsRunning()) {
        mesh = await meshGetState();
        // Capacity is best-effort glassbox — never let a /status
        // hiccup blank out the "connected" label.
        try {
          runtime = await getRuntimeStatus();
        } catch {
          runtime = null;
        }
      } else {
        mesh = null;
        runtime = null;
      }
    } catch {
      mesh = null;
      runtime = null;
    }
  }

  let label = $derived(
    mesh
      ? mesh.status.members_online === 1
        ? "1 connected"
        : `${mesh.status.members_online} connected`
      : null
  );

  // Pooled mesh capacity, surfaced honestly: free VRAM/storage summed
  // across online members (sourced from `/status`). Appended to the
  // "N connected" label as a glanceable "· 48 GB", with the full
  // breakdown on hover — glassbox over the sidebar dot so the user can
  // see the compute behind the count.
  let capacityLabel = $derived(
    runtime && runtime.pooled_vram_gb > 0
      ? ` · ${Math.round(runtime.pooled_vram_gb)} GB`
      : ""
  );
  let capacityTooltip = $derived(
    runtime
      ? `${runtime.members_online} of ${runtime.members_total} peers online` +
          ` · ${Math.round(runtime.pooled_vram_gb)} GB VRAM` +
          ` · ${Math.round(runtime.pooled_storage_gb)} GB storage pooled`
      : ""
  );
</script>

{#if label}
  <div class="mesh-status" aria-live="polite" title={capacityTooltip}>
    <span class="dot" aria-hidden="true"></span>
    <span class="count">{label}{capacityLabel}</span>
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

  /* Honour the OS "reduce motion" setting — the status is fully
     conveyed by the text label, so the pulse is decoration. Matches
     the reduced-motion guards in App/NarrationChip/ToastHost/etc. */
  @media (prefers-reduced-motion: reduce) {
    .dot {
      animation: none;
    }
  }

  .count {
    font-family: var(--font-mono);
    font-size: 0.65rem;
    letter-spacing: 0.04em;
    color: var(--text-muted);
  }
</style>
