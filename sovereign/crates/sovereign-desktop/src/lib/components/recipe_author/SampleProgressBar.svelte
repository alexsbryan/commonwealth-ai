<script lang="ts">
  // Visual position on the test-scale progression: 50 → 200 → 1000 →
  // full. The agent climbs the ladder during a session; the partner
  // sees how far they've come at a glance.
  import Card from "./Card.svelte";

  let { currentSampleSize }: { currentSampleSize: number | null } = $props();

  // Stops on the ladder. `null` represents "full corpus" — the agent
  // sets this when the final pass succeeds.
  const STOPS = [50, 200, 1000];

  function reached(stop: number, current: number | null): boolean {
    if (current === null) return false;
    return current >= stop;
  }
</script>

<Card title="Test scale">
  {#if currentSampleSize === null}
    <p class="muted">No sample run yet.</p>
  {:else}
    <ol class="ladder">
      {#each STOPS as stop}
        <li class:hit={reached(stop, currentSampleSize)}>
          <span class="dot" aria-hidden="true"></span>
          n={stop}
        </li>
      {/each}
      <li class:hit={currentSampleSize > STOPS[STOPS.length - 1]}>
        <span class="dot" aria-hidden="true"></span>
        full
      </li>
    </ol>
    <p class="now">
      Current: <strong>n={currentSampleSize}</strong>
    </p>
  {/if}
</Card>

<style>
  .ladder {
    list-style: none;
    margin: 0 0 0.4rem;
    padding: 0;
    display: flex;
    justify-content: space-between;
    gap: 0.4rem;
    font-size: 0.78rem;
  }
  .ladder li {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.2rem;
    color: var(--muted, #8a8c93);
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--border-mid);
    border: 1px solid var(--border-bright);
  }
  .ladder li.hit {
    color: var(--growth);
  }
  .ladder li.hit .dot {
    background: color-mix(in srgb, var(--growth) 60%, transparent);
    border-color: color-mix(in srgb, var(--growth) 80%, transparent);
  }
  .now {
    margin: 0.3rem 0 0;
    font-size: 0.82rem;
    color: var(--fg, #e6e6e8);
  }
  .muted {
    margin: 0;
    color: var(--muted, #8a8c93);
    font-style: italic;
    font-size: 0.82rem;
  }
</style>
