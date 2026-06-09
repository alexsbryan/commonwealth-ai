<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { insightStore } from "../stores/insights.svelte";
  import { exploreInsights } from "../api";

  interface Props {
    onNavigate: (conversationId: string) => void;
  }

  let { onNavigate }: Props = $props();
  let loading = $state(false);

  async function handleExplore() {
    if (loading) return;
    loading = true;
    try {
      const ids = insightStore.items.map((n) => n.id);
      const convId = await exploreInsights(ids);
      onNavigate(convId);
    } catch (e) {
      console.error("Failed to explore insights:", e);
    } finally {
      loading = false;
    }
  }
</script>

{#if insightStore.count >= 2}
  <button class="explore-btn" onclick={handleExplore} disabled={loading}>
    {loading ? "Creating\u2026" : "Explore with these \u2197"}
  </button>
{/if}

<style>
  .explore-btn {
    width: 100%;
    padding: 8px;
    margin-top: 8px;
    background: var(--accent-glow);
    border: 0.5px solid color-mix(in srgb, var(--accent) 30%, transparent);
    border-radius: var(--radius);
    color: var(--accent);
    font-size: 12px;
    font-weight: 600;
    font-family: var(--font-sans);
    cursor: pointer;
    transition: background 0.15s, border-color 0.15s;
  }

  .explore-btn:hover:not(:disabled) {
    background: var(--accent-dim);
    border-color: var(--accent);
  }

  .explore-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
