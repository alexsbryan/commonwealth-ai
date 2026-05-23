<script lang="ts">
  // Pending engine-gap escalations. Each request is a JSON file in
  // the maintainer inbox; the card shows a one-line summary so the
  // partner can see what's been bubbled up to the maintainer without
  // chasing files.
  import Card from "./Card.svelte";
  import type { DashboardNoteEntry } from "../../types";

  let { requests }: { requests: DashboardNoteEntry[] } = $props();

  function statusLabel(p: unknown): string {
    if (p && typeof p === "object" && "status" in p) {
      return String((p as Record<string, unknown>).status ?? "submitted");
    }
    return "submitted";
  }
</script>

<Card title="Capability requests" counter={requests.length}>
  {#if requests.length === 0}
    <p class="muted">No engine-gap escalations.</p>
  {:else}
    <ul>
      {#each requests as r (r.id)}
        <li>
          <div class="row-head">
            <span class="status">{statusLabel(r.payload)}</span>
            <span class="when">{r.created_at.slice(0, 10)}</span>
          </div>
          <p class="content">{r.content}</p>
        </li>
      {/each}
    </ul>
  {/if}
</Card>

<style>
  .muted {
    margin: 0;
    color: var(--muted, #8a8c93);
    font-style: italic;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  li {
    border-left: 2px solid color-mix(in srgb, var(--accent) 50%, transparent);
    padding: 0.1rem 0 0.1rem 0.55rem;
  }
  .row-head {
    display: flex;
    gap: 0.4rem;
    align-items: baseline;
    font-size: 0.72rem;
  }
  .status {
    text-transform: uppercase;
    color: var(--accent-light);
    font-weight: 600;
    letter-spacing: 0.04em;
  }
  .when {
    margin-left: auto;
    color: var(--muted, #8a8c93);
  }
  .content {
    margin: 0.2rem 0 0;
    font-size: 0.82rem;
    line-height: 1.35;
  }
</style>
